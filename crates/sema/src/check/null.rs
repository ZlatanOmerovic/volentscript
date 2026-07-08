//! Null safety (SPECS §4.1): reference types are non-nullable by default;
//! `T?` opts in. The ES4 drafts sketched `T?` — this finishes it.
//!
//! Enforcement model (declared-slot level):
//! - a possibly-null expression cannot flow into a non-nullable slot
//!   (locals, params, fields, returns) — E0312;
//! - dereferencing a possibly-null receiver is an error — E0313;
//! - non-nullable reference locals/fields need initializers — E0314;
//! - `*` remains freely nullable (the migration escape hatch, §4.1);
//! - narrowing: `if (x != null)` / truthy `if (x)` narrow a local in the
//!   branch (and after the `if` when the other branch cannot complete).
//!   Assignments update the narrow set based on the assigned value.

use ast::BinaryOp;
use diagnostics::ErrorCode;
use span::Span;

use crate::tast::*;
use crate::ty::Ty;

use super::Checker;

impl<'a> Checker<'a> {
    /// Whether null-safety applies to this type at all: reference types
    /// except the `*` escape hatch.
    pub(crate) fn null_tracked(&self, ty: Ty) -> bool {
        ty.is_reference() && ty != Ty::Any && ty != Ty::Null
    }

    /// Conservative "could this expression be null" (SPECS §4.1).
    pub(crate) fn expr_nullable(&self, e: &TExpr) -> bool {
        self.expr_nullable_under(e, &[])
    }

    /// Like [`Self::expr_nullable`] with extra locals proven non-null in
    /// this context (branch-local facts a conditional's condition proves —
    /// the scope stack no longer holds them when callers query).
    fn expr_nullable_under(&self, e: &TExpr, proven: &[LocalId]) -> bool {
        // The null literal itself (typed `Null`) is definitely null.
        if e.ty == Ty::Null {
            return true;
        }
        if !self.null_tracked(e.ty) {
            return false;
        }
        match &e.kind {
            TExprKind::Null => true,
            // `as` yields null on mismatch (SPECS §3.1) — unless an `is`
            // guard on the same local proves the cast cannot miss.
            TExprKind::As(inner, ty) => !self.is_guarded(inner, *ty),
            TExprKind::Str(_)
            | TExprKind::New(..)
            | TExprKind::This
            | TExprKind::VectorLit(..)
            | TExprKind::Array(_)
            | TExprKind::CallArr(..)
            | TExprKind::CallVec(..)
            // Operators produce values, never null (concat, is, etc.).
            | TExprKind::Binary(..)
            | TExprKind::Unary(..)
            | TExprKind::Postfix(..)
            | TExprKind::Is(..)
            | TExprKind::CallBuiltin(..)
            // Function values are freshly constructed, never null.
            | TExprKind::FnRef(_)
            | TExprKind::BuiltinRef(_)
            | TExprKind::Closure(_)
            | TExprKind::BoundMethod(..) => false,
            // exec/match return null on no-match (§15.10.6.2, §15.5.4.10).
            TExprKind::CallMethod(recv, name, _) => {
                (recv.ty == Ty::RegExp && name == "exec")
                    || (recv.ty == Ty::String && name == "match")
                    // Socket reads return null at EOF (SPECS §6).
                    || (recv.ty == Ty::Socket && (name == "readLine" || name == "read"))
            }
            TExprKind::RegExp(..)
            | TExprKind::NewRegExp(_)
            | TExprKind::NewDate(_)
            | TExprKind::NamespaceVal(_)
            | TExprKind::NewNamespace(_) => false,
            // A captured variable's nullability is its origin local's
            // (walk the capture chain up the enclosing-function stack).
            TExprKind::CaptureGet(slot) => {
                let mut depth = self.fn_stack.len() - 1;
                let mut slot = *slot;
                loop {
                    match self.functions[self.fn_stack[depth]].captures.get(slot) {
                        Some(CapSrc::ParentCapture(i)) => {
                            let i = *i;
                            if depth == 0 {
                                return true;
                            }
                            depth -= 1;
                            slot = i;
                        }
                        Some(CapSrc::ParentLocal(l)) => {
                            if depth == 0 {
                                return true;
                            }
                            return self.functions[self.fn_stack[depth - 1]]
                                .locals
                                .get(l.0 as usize)
                                .is_none_or(|loc| loc.nullable);
                        }
                        None => return true,
                    }
                }
            }
            // Vector.<T> elements are non-nullable by construction (all
            // writes flow-checked); Array elements are `*`.
            TExprKind::Index(recv, _) => !matches!(recv.ty, Ty::Vector(_)),
            TExprKind::LocalGet(id) => {
                let fn_index = *self.fn_stack.last().expect("fn");
                let local = &self.functions[fn_index].locals[id.0 as usize];
                local.nullable
                    && !self.narrowed.iter().any(|set| set.contains(id))
                    && !proven.contains(id)
            }
            TExprKind::LocalSet(_, v) => self.expr_nullable_under(v, proven),
            TExprKind::FieldGet(_, class, slot) => self
                .registry
                .field_by_slot(*class, *slot)
                .is_some_and(|f| f.nullable),
            TExprKind::StaticGet(class, index) => {
                self.registry.classes[class.0 as usize].static_fields[*index].nullable
            }
            TExprKind::CallFn(id, _) | TExprKind::CallDirect { fn_id: id, .. } => {
                self.fn_ret_nullable(*id)
            }
            TExprKind::CallVirtual { class, vslot, .. } => {
                self.registry.classes[class.0 as usize].vtable[*vslot].sig.ret_nullable
            }
            TExprKind::CallIface { iface, islot, .. } => {
                self.registry.ifaces[iface.0 as usize].methods[*islot].sig.ret_nullable
            }
            // Each branch keeps the facts the condition proves for it
            // (`x == null ? fallback : x` — the else side has x non-null).
            TExprKind::Conditional(c, a, b) => {
                let (when_true, when_false) = Self::narrowing_of(c);
                let mut pt: Vec<LocalId> = proven.to_vec();
                pt.extend(when_true);
                let mut pf: Vec<LocalId> = proven.to_vec();
                pf.extend(when_false);
                self.expr_nullable_under(a, &pt) || self.expr_nullable_under(b, &pf)
            }
            TExprKind::Logical(op, a, b) => {
                // `a && b`: result is a-falsy or b; `a || b`: a-truthy or b.
                // Truthy values are non-null, so `||` is null only via b.
                if matches!(op, ast::BinaryOp::LogOr) {
                    self.expr_nullable_under(b, proven)
                } else {
                    self.expr_nullable_under(a, proven) || self.expr_nullable_under(b, proven)
                }
            }
            // Natives: only File.read / System.getenv return null.
            TExprKind::CallNative(nf, _) => matches!(
                nf,
                crate::builtins::NativeFn::FileRead
                    | crate::builtins::NativeFn::SystemGetenv
                    | crate::builtins::NativeFn::FileList
                    | crate::builtins::NativeFn::SystemReadLine
            ),
            TExprKind::Coerce(_, v) => self.expr_nullable_under(v, proven),
            TExprKind::Comma(_, b) => self.expr_nullable_under(b, proven),
            // Anything coming out of `*` or unknown sources: assume
            // possibly null.
            _ => true,
        }
    }

    fn fn_ret_nullable(&self, id: FnId) -> bool {
        self.fn_ret_nullable_flags
            .get(id.0 as usize)
            .copied()
            .unwrap_or(false)
    }

    /// Flow into a non-nullable slot (assignment/argument/return/field).
    pub(crate) fn check_null_flow(
        &mut self,
        value: &TExpr,
        target_ty: Ty,
        target_nullable: bool,
        what: &str,
        span: Span,
    ) {
        if target_nullable || !self.null_tracked(target_ty) {
            return;
        }
        if self.expr_nullable(value) {
            self.error(
                ErrorCode::NULL_FLOW,
                format!(
                    "possibly-null value assigned to non-nullable {what} `{}` — declare it `{}?` or narrow with a null check (SPECS §4.1)",
                    self.ty_name(target_ty),
                    self.ty_name(target_ty)
                ),
                span,
            );
        }
    }

    /// Dereference (member access / method call / index) on a receiver.
    pub(crate) fn check_null_deref(&mut self, receiver: &TExpr, span: Span) {
        if self.null_tracked(receiver.ty) && self.expr_nullable(receiver) {
            self.error(
                ErrorCode::NULL_DEREF,
                format!(
                    "`{}` value may be null here — narrow with a null check first (SPECS §4.1)",
                    self.ty_name(receiver.ty)
                ),
                span,
            );
        }
    }

    /// Whether an active `is` guard proves `inner as ty` cannot miss.
    fn is_guarded(&self, inner: &TExpr, ty: Ty) -> bool {
        match &inner.kind {
            TExprKind::LocalGet(id) => self
                .is_narrowed
                .iter()
                .any(|set| set.iter().any(|&(l, t)| l == *id && t == ty)),
            TExprKind::Coerce(_, v) => self.is_guarded(v, ty),
            _ => false,
        }
    }

    /// `(local, class)` pairs a condition proves via `is` when true.
    pub(crate) fn is_narrowing_of(cond: &TExpr) -> Vec<(LocalId, Ty)> {
        fn local_of(e: &TExpr) -> Option<LocalId> {
            match &e.kind {
                TExprKind::LocalGet(id) => Some(*id),
                TExprKind::Coerce(_, v) => local_of(v),
                _ => None,
            }
        }
        let mut cond = cond;
        if let TExprKind::Coerce(Coercion::ToBoolean, v) = &cond.kind {
            cond = v;
        }
        if let TExprKind::Is(inner, ty) = &cond.kind
            && let Some(id) = local_of(inner)
        {
            return vec![(id, *ty)];
        }
        Vec::new()
    }

    /// Locals a condition proves non-null when true / when false.
    pub(crate) fn narrowing_of(cond: &TExpr) -> (Vec<LocalId>, Vec<LocalId>) {
        fn local_of(e: &TExpr) -> Option<LocalId> {
            match &e.kind {
                TExprKind::LocalGet(id) => Some(*id),
                TExprKind::Coerce(_, v) => local_of(v),
                _ => None,
            }
        }
        let mut when_true = Vec::new();
        let mut when_false = Vec::new();
        match &cond.kind {
            // `a && b` proves both sides' when-true facts when true;
            // `a || b` proves both sides' when-false facts when false
            // (short-circuit narrowing, SPECS §4.1).
            TExprKind::Logical(op, l, r) => {
                let (lt, lf) = Self::narrowing_of(l);
                let (rt, rf) = Self::narrowing_of(r);
                match op {
                    BinaryOp::LogAnd => {
                        when_true.extend(lt);
                        when_true.extend(rt);
                    }
                    BinaryOp::LogOr => {
                        when_false.extend(lf);
                        when_false.extend(rf);
                    }
                    _ => {}
                }
            }
            // if (x) — truthy implies non-null.
            TExprKind::Coerce(Coercion::ToBoolean, v) => {
                if let Some(id) = local_of(v) {
                    when_true.push(id);
                }
            }
            TExprKind::Binary(op, l, r) => {
                let operand = match (&l.kind, &r.kind) {
                    (TExprKind::Null, _) => local_of(r),
                    (_, TExprKind::Null) => local_of(l),
                    _ => None,
                };
                if let Some(id) = operand {
                    match op {
                        BinaryOp::Ne | BinaryOp::StrictNe => when_true.push(id),
                        BinaryOp::Eq | BinaryOp::StrictEq => when_false.push(id),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        (when_true, when_false)
    }
}
