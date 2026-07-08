//! P24: bounds-check elimination via loop versioning.
//!
//! Compiled numeric-`Vector` access (P23) is a bounds-checked load/store:
//! `idx < len` then branch, with a possibly-throwing slow path. Inside a
//! tight loop that branch and its unknown-memory call block LLVM's
//! autovectorizer. This pass proves, for a class of counted loops, that the
//! index is always in range and marks those accesses `unchecked` so codegen
//! emits a raw load/store — letting the loop vectorize.
//!
//! **Soundness by runtime guard.** A qualifying loop is rewritten to
//! `if (B <= v.length && …) { <fast, unchecked> } else { <slow, checked> }`.
//! The guard re-validates the bound at run time, so even if the static
//! analysis were imprecise the unchecked path only runs when the check
//! provably holds. Removing the check is therefore never a correctness risk.
//!
//! **Scope (v1, deliberately tight):**
//!   - counted loop `for (var i = C; i < B; i++)` (also `i <= B`), where `C`
//!     is an integer literal `>= 0`, the step is `i++`, and `i` is never
//!     reassigned in the body;
//!   - accesses `v[i]` where `v` is a **plain local** of unboxed-numeric
//!     `Vector.<Number|int|uint>`, the index is the **bare** induction
//!     variable, and — the key safety rule — **every** occurrence of `v` in
//!     the loop is a `v[i]` access or a `v.length` read. That rule alone
//!     rules out `push`/`pop`/`shift`/`unshift`, `v.length = …`, appends via
//!     `v[k]` (k != i), and passing `v` to a call that might grow it — any of
//!     which could change the length or reallocate the buffer mid-loop.
//!
//! **Deferred to a future phase (documented, not implemented):** affine
//! indices `v[i±k]` (the guard must cover both ends of the range) and loops
//! that legitimately grow `v` (the buffer can reallocate, so a hoisted data
//! pointer would dangle — those need per-iteration revalidation, not an
//! entry-only guard). See SPECS §4.3.

use std::collections::{HashMap, HashSet};

use crate::{BinOp, Conv, Expr, ExprKind, LocalId, Program, Stmt, Ty};
use span::Span;

/// Runs bounds-check elimination over every function body.
pub fn run(program: &mut Program) {
    // Escape hatch for measuring the pass's effect in isolation.
    if std::env::var_os("VS_NO_BCE").is_some() {
        return;
    }
    let vectors = program.vectors.clone();
    for f in &mut program.functions {
        let locals = f.locals.clone();
        let cx = Ctx {
            vectors: &vectors,
            locals: &locals,
        };
        for s in &mut f.body {
            cx.rewrite(s);
        }
    }
}

struct Ctx<'a> {
    /// Vector instantiation element types (index = `Ty::Vector` payload).
    vectors: &'a [Ty],
    /// Local slot types of the function being processed.
    locals: &'a [Ty],
}

impl Ctx<'_> {
    /// Post-order: rewrite nested loops first, then this statement.
    fn rewrite(&self, s: &mut Stmt) {
        match s {
            Stmt::Block(v) => v.iter_mut().for_each(|s| self.rewrite(s)),
            Stmt::If {
                then_branch,
                else_branch,
                ..
            } => {
                self.rewrite(then_branch);
                if let Some(e) = else_branch {
                    self.rewrite(e);
                }
            }
            Stmt::While { body, .. } | Stmt::DoWhile { body, .. } => self.rewrite(body),
            Stmt::For { body, .. } => self.rewrite(body),
            Stmt::Switch { cases, .. } => cases
                .iter_mut()
                .for_each(|c| c.body.iter_mut().for_each(|s| self.rewrite(s))),
            Stmt::Try {
                body,
                catches,
                finally,
            } => {
                body.iter_mut().for_each(|s| self.rewrite(s));
                for c in catches {
                    c.body.iter_mut().for_each(|s| self.rewrite(s));
                }
                if let Some(f) = finally {
                    f.iter_mut().for_each(|s| self.rewrite(s));
                }
            }
            _ => {}
        }
        // Now try to version this statement if it is a qualifying `for`.
        if let Some(versioned) = self.try_version(s) {
            *s = versioned;
        }
    }

    /// Returns whether a `Ty::Vector(inst)` holds an unboxed numeric element.
    fn numeric_vec_local(&self, l: LocalId) -> bool {
        match self.locals.get(l.0 as usize) {
            Some(Ty::Vector(inst)) => matches!(
                self.vectors.get(*inst as usize),
                Some(Ty::Number | Ty::Int | Ty::UInt)
            ),
            _ => false,
        }
    }

    fn try_version(&self, s: &Stmt) -> Option<Stmt> {
        let Stmt::For {
            label,
            init,
            cond,
            update,
            body,
        } = s
        else {
            return None;
        };
        // Header must be `i = C; i </<= B; i++`.
        let (iv, c0) = init.as_deref().and_then(init_var)?;
        if c0 < 0 {
            return None; // negative start — low bound not provable
        }
        if !update.as_ref().is_some_and(|u| is_inc_of(u, iv)) {
            return None;
        }
        let (op, bound) = cond.as_ref().and_then(|c| counted_cond(c, iv))?;

        // Gather facts about the body in one exhaustive walk.
        let mut facts = Facts::default();
        collect_stmt(body, iv, &mut facts);

        // The induction variable must not be mutated in the body.
        if facts.writes.contains(&iv) {
            return None;
        }
        // The bound must be loop-invariant and side-effect free.
        if !invariant(bound, &facts.writes, &facts.captures_written) {
            return None;
        }

        // A candidate `v` is safe iff every occurrence of it in the loop is a
        // `v[i]` access or a `v.length` read (total reads == good reads), it
        // is an unboxed numeric vector local, it is never reassigned, and it
        // is indexed by `i` at least once.
        let mut safe: Vec<LocalId> = facts
            .indexed
            .iter()
            .copied()
            .filter(|&v| {
                v != iv
                    && self.numeric_vec_local(v)
                    && !facts.writes.contains(&v)
                    && facts.total_reads.get(&v) == facts.good_reads.get(&v)
            })
            .collect();
        safe.sort_by_key(|l| l.0);
        safe.dedup();
        if safe.is_empty() {
            return None;
        }

        // Build the guard: for each safe `v`, `B <= v.length` (`<`) or
        // `B < v.length` (`<=`), all AND-ed. `C >= 0` is already a literal.
        let span = body.span_hint();
        let guard = safe
            .iter()
            .map(|&v| guard_term(op, bound, v, span))
            .reduce(|a, b| Expr {
                ty: Ty::Boolean,
                span,
                kind: ExprKind::Logical {
                    is_and: true,
                    lhs: Box::new(a),
                    rhs: Box::new(b),
                },
            })?;

        // Fast clone with the safe accesses marked unchecked; slow = original.
        let safe_set: HashSet<LocalId> = safe.into_iter().collect();
        let mut fast = Stmt::For {
            label: label.clone(),
            init: init.clone(),
            cond: cond.clone(),
            update: update.clone(),
            body: body.clone(),
        };
        mark_stmt(&mut fast, iv, &safe_set);
        let slow = Stmt::For {
            label: label.clone(),
            init: init.clone(),
            cond: cond.clone(),
            update: update.clone(),
            body: body.clone(),
        };
        Some(Stmt::If {
            cond: guard,
            then_branch: Box::new(fast),
            else_branch: Some(Box::new(slow)),
        })
    }
}

/// `for` init of the form `var i = <int literal>` → `(i, literal)`.
fn init_var(s: &Stmt) -> Option<(LocalId, i64)> {
    let (l, e) = match s {
        Stmt::Assign(l, e) => (*l, e),
        Stmt::Expr(Expr {
            kind: ExprKind::LocalSet(l, e),
            ..
        }) => (*l, e.as_ref()),
        _ => return None,
    };
    match e.kind {
        ExprKind::Int(v) => Some((l, i64::from(v))),
        ExprKind::UInt(v) => Some((l, i64::from(v))),
        _ => None,
    }
}

/// Whether `update` is exactly `i++` for the induction variable.
fn is_inc_of(update: &Expr, iv: LocalId) -> bool {
    matches!(
        &update.kind,
        ExprKind::IncDec { target, is_inc: true, .. } if *target == iv
    )
}

/// A counted condition `i < B` / `i <= B` (with the ToNumber coercions the
/// lowerer inserts peeled) → `(op, &B)`. `op` is `Lt` or `Le`.
fn counted_cond(cond: &Expr, iv: LocalId) -> Option<(BinOp, &Expr)> {
    let ExprKind::Binary(op, lhs, rhs) = &cond.kind else {
        return None;
    };
    if !matches!(op, BinOp::Lt | BinOp::Le) {
        return None;
    }
    if peel(lhs).as_local() != Some(iv) {
        return None;
    }
    Some((*op, peel(rhs)))
}

/// Strips the `ToNumber` coercions the lowerer wraps around integer operands.
fn peel(e: &Expr) -> &Expr {
    match &e.kind {
        ExprKind::Conv(Conv::ToNumber, inner) => peel(inner),
        _ => e,
    }
}

impl Expr {
    fn as_local(&self) -> Option<LocalId> {
        match self.kind {
            ExprKind::LocalGet(l) => Some(l),
            _ => None,
        }
    }
}

impl Stmt {
    fn span_hint(&self) -> Span {
        match self {
            Stmt::Expr(e) | Stmt::Assign(_, e) => e.span,
            Stmt::Block(v) => v.first().map_or(Span::new(span::SourceId(0), 0, 0), |s| {
                s.span_hint()
            }),
            Stmt::If { cond, .. } => cond.span,
            Stmt::While { cond, .. } | Stmt::DoWhile { cond, .. } => cond.span,
            Stmt::For { body, .. } => body.span_hint(),
            _ => Span::new(span::SourceId(0), 0, 0),
        }
    }
}

/// One guard conjunct comparing the loop bound to a vector's length.
fn guard_term(op: BinOp, bound: &Expr, v: LocalId, span: Span) -> Expr {
    // Lt (`i < B`): max index is B-1, need `B <= len`.
    // Le (`i <= B`): max index is B, need `B < len`.
    let cmp = if op == BinOp::Le { BinOp::Lt } else { BinOp::Le };
    let vget = Expr {
        ty: Ty::Vector(0), // payload irrelevant to SeqLen codegen
        span,
        kind: ExprKind::LocalGet(v),
    };
    let len = num(Expr {
        ty: Ty::UInt,
        span,
        kind: ExprKind::SeqLen(Box::new(vget)),
    });
    Expr {
        ty: Ty::Boolean,
        span,
        kind: ExprKind::Binary(cmp, Box::new(num(bound.clone())), Box::new(len)),
    }
}

/// Wraps an expression in a `ToNumber` so guard comparisons are done in f64
/// (matches how the lowerer compares mixed int operands).
fn num(e: Expr) -> Expr {
    if e.ty == Ty::Number {
        return e;
    }
    Expr {
        ty: Ty::Number,
        span: e.span,
        kind: ExprKind::Conv(Conv::ToNumber, Box::new(e)),
    }
}

/// Loop-invariance check for a bound expression: reads no mutated local and
/// no mutated capture, and is side-effect free (only literals, invariant
/// locals/captures, and `v.length` of an invariant vector).
fn invariant(e: &Expr, writes: &HashSet<LocalId>, caps: &HashSet<usize>) -> bool {
    match &e.kind {
        ExprKind::Int(_) | ExprKind::UInt(_) | ExprKind::Number(_) => true,
        ExprKind::LocalGet(l) => !writes.contains(l),
        ExprKind::CaptureGet(slot) => !caps.contains(slot),
        ExprKind::StaticGet(..) => true,
        ExprKind::Conv(_, inner) | ExprKind::SeqLen(inner) => invariant(inner, writes, caps),
        ExprKind::Binary(_, a, b) => {
            invariant(a, writes, caps) && invariant(b, writes, caps)
        }
        _ => false,
    }
}

/// Facts gathered from one loop body.
#[derive(Default)]
struct Facts {
    /// Locals written anywhere in the body.
    writes: HashSet<LocalId>,
    /// Capture slots written anywhere in the body.
    captures_written: HashSet<usize>,
    /// Locals that are the receiver of at least one `v[i]` access.
    indexed: HashSet<LocalId>,
    /// Total `LocalGet` occurrences per local.
    total_reads: HashMap<LocalId, usize>,
    /// `LocalGet` occurrences that are a safe access recv (`v[i]`/`v.length`).
    good_reads: HashMap<LocalId, usize>,
}

fn collect_stmt(s: &Stmt, iv: LocalId, f: &mut Facts) {
    match s {
        Stmt::Expr(e) | Stmt::Throw(e) => collect_expr(e, iv, f),
        Stmt::Assign(l, e) => {
            f.writes.insert(*l);
            collect_expr(e, iv, f);
        }
        Stmt::Block(v) => v.iter().for_each(|s| collect_stmt(s, iv, f)),
        Stmt::If {
            cond,
            then_branch,
            else_branch,
        } => {
            collect_expr(cond, iv, f);
            collect_stmt(then_branch, iv, f);
            if let Some(e) = else_branch {
                collect_stmt(e, iv, f);
            }
        }
        Stmt::While { cond, body, .. } | Stmt::DoWhile { body, cond, .. } => {
            collect_expr(cond, iv, f);
            collect_stmt(body, iv, f);
        }
        Stmt::For {
            init,
            cond,
            update,
            body,
            ..
        } => {
            if let Some(i) = init {
                collect_stmt(i, iv, f);
            }
            if let Some(c) = cond {
                collect_expr(c, iv, f);
            }
            if let Some(u) = update {
                collect_expr(u, iv, f);
            }
            collect_stmt(body, iv, f);
        }
        Stmt::Switch { scrutinee, cases } => {
            collect_expr(scrutinee, iv, f);
            for c in cases {
                c.body.iter().for_each(|s| collect_stmt(s, iv, f));
            }
        }
        Stmt::Return { value } => {
            if let Some(e) = value {
                collect_expr(e, iv, f);
            }
        }
        Stmt::Try {
            body,
            catches,
            finally,
        } => {
            body.iter().for_each(|s| collect_stmt(s, iv, f));
            for c in catches {
                c.body.iter().for_each(|s| collect_stmt(s, iv, f));
            }
            if let Some(fin) = finally {
                fin.iter().for_each(|s| collect_stmt(s, iv, f));
            }
        }
        Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Empty => {}
    }
}

/// Pre-order over `e` and all descendants, recording facts. The `match` is
/// exhaustive (no wildcard) so a new `ExprKind` can never silently escape the
/// mutation scan — that completeness is what keeps the elimination sound.
fn collect_expr(e: &Expr, iv: LocalId, f: &mut Facts) {
    match &e.kind {
        // Recording nodes.
        ExprKind::LocalGet(l) => {
            *f.total_reads.entry(*l).or_default() += 1;
        }
        ExprKind::LocalSet(l, v) => {
            f.writes.insert(*l);
            collect_expr(v, iv, f);
        }
        ExprKind::IncDec { target, .. } => {
            f.writes.insert(*target);
        }
        ExprKind::CaptureSet(slot, v) => {
            f.captures_written.insert(*slot);
            collect_expr(v, iv, f);
        }
        ExprKind::CaptureIncDec { slot, .. } => {
            f.captures_written.insert(*slot);
        }
        ExprKind::SeqGet(recv, idx, _) => {
            record_access(recv, idx, iv, f);
            collect_expr(recv, iv, f);
            collect_expr(idx, iv, f);
        }
        ExprKind::SeqSet(recv, idx, val, _) => {
            record_access(recv, idx, iv, f);
            collect_expr(recv, iv, f);
            collect_expr(idx, iv, f);
            collect_expr(val, iv, f);
        }
        ExprKind::SeqLen(recv) => {
            if let Some(l) = recv.as_local() {
                *f.good_reads.entry(l).or_default() += 1;
            }
            collect_expr(recv, iv, f);
        }
        // Pure leaves.
        ExprKind::Int(_)
        | ExprKind::UInt(_)
        | ExprKind::Number(_)
        | ExprKind::Str(_)
        | ExprKind::Bool(_)
        | ExprKind::Null
        | ExprKind::Undefined
        | ExprKind::This
        | ExprKind::StaticGet(..)
        | ExprKind::CaptureGet(_)
        | ExprKind::Closure(_)
        | ExprKind::FnValue(_)
        | ExprKind::BuiltinValue(_)
        | ExprKind::NamespaceVal(_)
        | ExprKind::StaticIncDec { .. }
        | ExprKind::RegExpLit(..) => {}
        // One child.
        ExprKind::StrLen(a)
        | ExprKind::Unary(_, a)
        | ExprKind::Is(a, _)
        | ExprKind::As(a, _)
        | ExprKind::Conv(_, a)
        | ExprKind::StaticSet(_, _, a)
        | ExprKind::EnumLen(a)
        | ExprKind::PropGet(a, _)
        | ExprKind::DeleteProp(a, _)
        | ExprKind::NewNamespace(a)
        | ExprKind::NsUri(a)
        | ExprKind::FieldGet(a, _, _)
        | ExprKind::BoundMethod(a, _, _)
        | ExprKind::FieldIncDec { recv: a, .. } => collect_expr(a, iv, f),
        // Two children.
        ExprKind::Binary(_, a, b)
        | ExprKind::Comma(a, b)
        | ExprKind::EnumKey(a, b)
        | ExprKind::EnumValue(a, b)
        | ExprKind::AnyIndexGet(a, b)
        | ExprKind::HasProp(a, b)
        | ExprKind::PropSet(a, _, b)
        | ExprKind::FieldSet(a, _, _, b)
        | ExprKind::NsGet(a, b, _)
        | ExprKind::SeqSetLen(a, b)
        | ExprKind::Logical { lhs: a, rhs: b, .. } => {
            collect_expr(a, iv, f);
            collect_expr(b, iv, f);
        }
        // Three children.
        ExprKind::Conditional(a, b, c) | ExprKind::AnyIndexSet(a, b, c) => {
            collect_expr(a, iv, f);
            collect_expr(b, iv, f);
            collect_expr(c, iv, f);
        }
        // Callee/recv + arg list.
        ExprKind::CallStrMethod(_, recv, args)
        | ExprKind::CallNumMethod(_, recv, args)
        | ExprKind::CallArr(_, recv, args)
        | ExprKind::CallVec(_, recv, args)
        | ExprKind::PropCall(recv, _, args)
        | ExprKind::NsCall(recv, _, _, args) => {
            collect_expr(recv, iv, f);
            args.iter().for_each(|a| collect_expr(a, iv, f));
        }
        ExprKind::CallVirtual { recv, args, .. }
        | ExprKind::CallIface { recv, args, .. }
        | ExprKind::CallDirect { recv, args, .. } => {
            collect_expr(recv, iv, f);
            args.iter().for_each(|a| collect_expr(a, iv, f));
        }
        // Bare arg lists.
        ExprKind::CallFn(_, args)
        | ExprKind::CallBuiltin(_, args)
        | ExprKind::New(_, args)
        | ExprKind::VectorLit(_, args)
        | ExprKind::CallNative(_, args)
        | ExprKind::NewRegExp(args)
        | ExprKind::CallRegex(_, args)
        | ExprKind::NewDate(args)
        | ExprKind::CallDate(_, args)
        | ExprKind::CallSocket(_, args) => args.iter().for_each(|a| collect_expr(a, iv, f)),
        ExprKind::CallFnValue {
            callee,
            this_arg,
            args,
            ..
        } => {
            collect_expr(callee, iv, f);
            if let Some(t) = this_arg {
                collect_expr(t, iv, f);
            }
            args.iter().for_each(|a| collect_expr(a, iv, f));
        }
        ExprKind::ArrayLit(elems) => {
            elems.iter().flatten().for_each(|a| collect_expr(a, iv, f));
        }
        ExprKind::ObjectLit(props) => {
            props.iter().for_each(|(_, a)| collect_expr(a, iv, f));
        }
    }
}

/// Records a `v[i]` access: if `recv` is a plain local and `idx` is the bare
/// induction variable, it counts as a "good" read of that local and marks it
/// indexed. (A non-`i` index deliberately does *not* count as good, so such a
/// receiver's total reads exceed its good reads and it is disqualified.)
fn record_access(recv: &Expr, idx: &Expr, iv: LocalId, f: &mut Facts) {
    if let Some(l) = recv.as_local()
        && peel(idx).as_local() == Some(iv)
    {
        f.indexed.insert(l);
        *f.good_reads.entry(l).or_default() += 1;
    }
}

/// Marks `v[i]` accesses on safe locals as unchecked, throughout a statement.
fn mark_stmt(s: &mut Stmt, iv: LocalId, safe: &HashSet<LocalId>) {
    match s {
        Stmt::Expr(e) | Stmt::Throw(e) | Stmt::Assign(_, e) => mark_expr(e, iv, safe),
        Stmt::Block(v) => v.iter_mut().for_each(|s| mark_stmt(s, iv, safe)),
        Stmt::If {
            cond,
            then_branch,
            else_branch,
        } => {
            mark_expr(cond, iv, safe);
            mark_stmt(then_branch, iv, safe);
            if let Some(e) = else_branch {
                mark_stmt(e, iv, safe);
            }
        }
        Stmt::While { cond, body, .. } | Stmt::DoWhile { body, cond, .. } => {
            mark_expr(cond, iv, safe);
            mark_stmt(body, iv, safe);
        }
        Stmt::For {
            init,
            cond,
            update,
            body,
            ..
        } => {
            if let Some(i) = init {
                mark_stmt(i, iv, safe);
            }
            if let Some(c) = cond {
                mark_expr(c, iv, safe);
            }
            if let Some(u) = update {
                mark_expr(u, iv, safe);
            }
            mark_stmt(body, iv, safe);
        }
        Stmt::Switch { scrutinee, cases } => {
            mark_expr(scrutinee, iv, safe);
            for c in cases {
                c.body.iter_mut().for_each(|s| mark_stmt(s, iv, safe));
            }
        }
        Stmt::Return { value } => {
            if let Some(e) = value {
                mark_expr(e, iv, safe);
            }
        }
        Stmt::Try {
            body,
            catches,
            finally,
        } => {
            body.iter_mut().for_each(|s| mark_stmt(s, iv, safe));
            for c in catches {
                c.body.iter_mut().for_each(|s| mark_stmt(s, iv, safe));
            }
            if let Some(fin) = finally {
                fin.iter_mut().for_each(|s| mark_stmt(s, iv, safe));
            }
        }
        Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Empty => {}
    }
}

/// Sets the unchecked flag on `v[i]` accesses to safe locals, recursively.
/// Only the flag on qualifying `Seq{Get,Set}` nodes changes; children are
/// still visited so nested loops' accesses are marked too.
fn mark_expr(e: &mut Expr, iv: LocalId, safe: &HashSet<LocalId>) {
    match &mut e.kind {
        ExprKind::SeqGet(recv, idx, unchecked) => {
            if recv.as_local().is_some_and(|l| safe.contains(&l))
                && peel(idx).as_local() == Some(iv)
            {
                *unchecked = true;
            }
            mark_expr(recv, iv, safe);
            mark_expr(idx, iv, safe);
        }
        ExprKind::SeqSet(recv, idx, val, unchecked) => {
            if recv.as_local().is_some_and(|l| safe.contains(&l))
                && peel(idx).as_local() == Some(iv)
            {
                *unchecked = true;
            }
            mark_expr(recv, iv, safe);
            mark_expr(idx, iv, safe);
            mark_expr(val, iv, safe);
        }
        _ => each_child_mut(e, &mut |c| mark_expr(c, iv, safe)),
    }
}

/// Applies `f` to each direct child expression. Exhaustive (no wildcard) so
/// the mutable walk can never skip a subtree.
fn each_child_mut(e: &mut Expr, f: &mut impl FnMut(&mut Expr)) {
    match &mut e.kind {
        ExprKind::Int(_)
        | ExprKind::UInt(_)
        | ExprKind::Number(_)
        | ExprKind::Str(_)
        | ExprKind::Bool(_)
        | ExprKind::Null
        | ExprKind::Undefined
        | ExprKind::This
        | ExprKind::LocalGet(_)
        | ExprKind::StaticGet(..)
        | ExprKind::CaptureGet(_)
        | ExprKind::Closure(_)
        | ExprKind::FnValue(_)
        | ExprKind::BuiltinValue(_)
        | ExprKind::NamespaceVal(_)
        | ExprKind::IncDec { .. }
        | ExprKind::StaticIncDec { .. }
        | ExprKind::CaptureIncDec { .. }
        | ExprKind::RegExpLit(..) => {}
        ExprKind::LocalSet(_, a)
        | ExprKind::StrLen(a)
        | ExprKind::Unary(_, a)
        | ExprKind::Is(a, _)
        | ExprKind::As(a, _)
        | ExprKind::Conv(_, a)
        | ExprKind::StaticSet(_, _, a)
        | ExprKind::CaptureSet(_, a)
        | ExprKind::EnumLen(a)
        | ExprKind::SeqLen(a)
        | ExprKind::PropGet(a, _)
        | ExprKind::DeleteProp(a, _)
        | ExprKind::NewNamespace(a)
        | ExprKind::NsUri(a)
        | ExprKind::FieldGet(a, _, _)
        | ExprKind::BoundMethod(a, _, _)
        | ExprKind::FieldIncDec { recv: a, .. } => f(a),
        ExprKind::Binary(_, a, b)
        | ExprKind::Comma(a, b)
        | ExprKind::EnumKey(a, b)
        | ExprKind::EnumValue(a, b)
        | ExprKind::AnyIndexGet(a, b)
        | ExprKind::HasProp(a, b)
        | ExprKind::PropSet(a, _, b)
        | ExprKind::FieldSet(a, _, _, b)
        | ExprKind::NsGet(a, b, _)
        | ExprKind::SeqSetLen(a, b)
        | ExprKind::Logical { lhs: a, rhs: b, .. } => {
            f(a);
            f(b);
        }
        ExprKind::Conditional(a, b, c) | ExprKind::AnyIndexSet(a, b, c) => {
            f(a);
            f(b);
            f(c);
        }
        ExprKind::SeqGet(a, b, _) => {
            f(a);
            f(b);
        }
        ExprKind::SeqSet(a, b, c, _) => {
            f(a);
            f(b);
            f(c);
        }
        ExprKind::CallStrMethod(_, recv, args)
        | ExprKind::CallNumMethod(_, recv, args)
        | ExprKind::CallArr(_, recv, args)
        | ExprKind::CallVec(_, recv, args)
        | ExprKind::PropCall(recv, _, args)
        | ExprKind::NsCall(recv, _, _, args) => {
            f(recv);
            args.iter_mut().for_each(&mut *f);
        }
        ExprKind::CallVirtual { recv, args, .. }
        | ExprKind::CallIface { recv, args, .. }
        | ExprKind::CallDirect { recv, args, .. } => {
            f(recv);
            args.iter_mut().for_each(&mut *f);
        }
        ExprKind::CallFn(_, args)
        | ExprKind::CallBuiltin(_, args)
        | ExprKind::New(_, args)
        | ExprKind::VectorLit(_, args)
        | ExprKind::CallNative(_, args)
        | ExprKind::NewRegExp(args)
        | ExprKind::CallRegex(_, args)
        | ExprKind::NewDate(args)
        | ExprKind::CallDate(_, args)
        | ExprKind::CallSocket(_, args) => args.iter_mut().for_each(&mut *f),
        ExprKind::CallFnValue {
            callee,
            this_arg,
            args,
            ..
        } => {
            f(callee);
            if let Some(t) = this_arg {
                f(t);
            }
            args.iter_mut().for_each(&mut *f);
        }
        ExprKind::ArrayLit(elems) => elems.iter_mut().flatten().for_each(&mut *f),
        ExprKind::ObjectLit(props) => props.iter_mut().for_each(|(_, a)| f(a)),
    }
}
