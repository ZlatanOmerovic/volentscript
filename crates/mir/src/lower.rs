//! Lowering: typed AST → MIR (SPECS §8 stage 5).
//!
//! Sema already made coercions explicit and desugared compound assignment;
//! lowering maps types/operators onto the backend universe, fills defaulted
//! arguments at callsites, turns `String#concat`-style calls into runtime
//! method ops, and rejects constructs whose phase hasn't landed with an
//! honest diagnostic (the backend never sees them).

use diagnostics::{Diagnostic, ErrorCode};
use span::Span;

use crate::*;

/// Lowers a checked program. Errors are phase-gate diagnostics (constructs
/// sema accepts but no backend supports yet).
pub fn lower(program: &sema::TProgram) -> Result<Program, Vec<Diagnostic>> {
    // Every class gets an effective constructor function. Reserve ids up
    // front (classes may extend classes declared later in the file).
    let sema_count = u32::try_from(program.functions.len()).expect("function count");
    let mut synthesized = 0u32;
    let ctor_fn: Vec<FnId> = program
        .registry
        .classes
        .iter()
        .map(|c| match c.ctor {
            Some(f) => FnId(f.0),
            None => {
                let id = FnId(sema_count + synthesized);
                synthesized += 1;
                id
            }
        })
        .collect();

    let mut lo = Lowerer {
        program,
        diagnostics: Vec::new(),
        ctor_fn,
        current_fn: 0,
        temp_base: 0,
        temps: Vec::new(),
    };
    let mut functions: Vec<Function> = program
        .functions
        .iter()
        .enumerate()
        .map(|(i, f)| {
            lo.current_fn = i;
            lo.function(f)
        })
        .collect();
    // Synthesized default constructors land at their reserved indices.
    for (index, info) in program.registry.classes.iter().enumerate() {
        if info.ctor.is_none() {
            functions.push(Function {
                name: format!("{}$ctor", info.name),
                this_class: Some(index as u32),
                ret: Ty::Void,
                locals: Vec::new(),
                param_count: 0,
                captured: Vec::new(),
                captures: Vec::new(),
                param_defaults: Vec::new(),
                body: Vec::new(),
                span: info.span,
            });
        }
    }
    // Constructor prologues: implicit super chain + field initializers.
    lo.inject_ctor_prologues(&mut functions);
    let classes = lo.lower_classes();
    // Static initializers run before top-level statements, in declaration
    // order (they are the class-load side effects).
    let mut static_inits = lo.static_init_stmts();
    static_inits.append(&mut functions[0].body);
    functions[0].body = static_inits;
    // SPECS §7: after the top-level statements run, a global `main` is
    // invoked; an `int` return becomes the process exit status.
    if let Some(main) = program.entry_main {
        let idx = main.0 as usize;
        let span = functions[idx].span;
        let ret = functions[idx].ret;
        let call = Expr {
            ty: ret,
            span,
            kind: ExprKind::CallFn(FnId(main.0), Vec::new()),
        };
        let stmt = if ret == Ty::Int {
            Stmt::Expr(Expr {
                ty: Ty::Void,
                span,
                kind: ExprKind::CallNative(SemaNativeFn::SystemExit, vec![call]),
            })
        } else {
            Stmt::Expr(call)
        };
        functions[0].body.push(stmt);
    }
    if lo.diagnostics.is_empty() {
        Ok(Program {
            functions,
            classes,
            iface_count: u32::try_from(program.registry.ifaces.len()).unwrap_or(0),
            error_classes: [
                "Error",
                "TypeError",
                "RangeError",
                "ReferenceError",
                "ArgumentError",
                "SyntaxError",
            ]
            .iter()
            .filter_map(|name| {
                program
                    .registry
                    .classes
                    .iter()
                    .position(|c| c.name == *name && c.package.is_empty())
                    .map(|i| i as u32)
            })
            .collect(),
            namespace_uris: program.namespace_uris.clone(),
            vectors: {
                let mut lo2 = Lowerer {
                    program,
                    diagnostics: Vec::new(),
                    ctor_fn: Vec::new(),
                    current_fn: 0,
                    temp_base: 0,
                    temps: Vec::new(),
                };
                program
                    .vectors
                    .iter()
                    .map(|&t| lo2.ty(t, span::Span::new(span::SourceId(0), 0, 0)))
                    .collect()
            },
        })
    } else {
        lo.diagnostics
            .sort_by_key(|d| d.span.map(|s| (s.start, s.end)));
        Err(lo.diagnostics)
    }
}

struct Lowerer<'a> {
    program: &'a sema::TProgram,
    diagnostics: Vec<Diagnostic>,
    /// Effective constructor function per class (user or synthesized).
    ctor_fn: Vec<FnId>,
    /// Index of the sema function currently being lowered.
    current_fn: usize,
    /// Base local count + temps appended by desugarings (for..in).
    temp_base: usize,
    temps: Vec<Ty>,
}

impl Lowerer<'_> {
    fn function(&mut self, f: &sema::TFunction) -> Function {
        let mut locals: Vec<Ty> = f.locals.iter().map(|l| self.ty(l.ty, f.span)).collect();
        self.temp_base = locals.len();
        self.temps.clear();
        let param_defaults = f
            .locals
            .iter()
            .take(f.param_count)
            .map(|l| {
                if l.is_rest {
                    None
                } else {
                    l.default.as_ref().map(|d| self.expr(d))
                }
            })
            .collect();
        Function {
            name: f.name.clone(),
            this_class: f.method_of.map(|c| c.0),
            ret: self.ty(f.return_ty, f.span),
            param_count: f.param_count,
            captured: {
                let mut c: Vec<bool> = f.locals.iter().map(|l| l.captured).collect();
                c.resize(c.len() + self.temps.len(), false);
                c
            },
            captures: f
                .captures
                .iter()
                .map(|c| match c {
                    sema::CapSrc::ParentLocal(id) => CapSrc::ParentLocal(LocalId(id.0)),
                    sema::CapSrc::ParentCapture(i) => CapSrc::ParentCapture(*i),
                })
                .collect(),
            param_defaults,
            body: {
                let body = f.body.iter().map(|s| self.stmt(s)).collect();
                locals.append(&mut self.temps);
                body
            },
            locals,
            span: f.span,
        }
    }

    fn ty(&mut self, ty: sema::Ty, _span: Span) -> Ty {
        match ty {
            sema::Ty::Int => Ty::Int,
            sema::Ty::UInt => Ty::UInt,
            sema::Ty::Number => Ty::Number,
            sema::Ty::Boolean => Ty::Boolean,
            // A bare `null` reaches MIR when no coercion was needed
            // (String/class contexts); every reference type is a pointer,
            // so the String kind is a safe carrier for the null literal.
            sema::Ty::String | sema::Ty::Null => Ty::String,
            sema::Ty::Void => Ty::Void,
            sema::Ty::Any => Ty::Any,
            sema::Ty::Class(id) => Ty::Object(id.0),
            sema::Ty::Iface(id) => Ty::Iface(id.0),
            sema::Ty::Array => Ty::Array,
            sema::Ty::Vector(inst) => Ty::Vector(inst),
            sema::Ty::Function => Ty::Function,
            sema::Ty::RegExp => Ty::RegExp,
            sema::Ty::Date => Ty::Date,
            sema::Ty::Socket | sema::Ty::ServerSocket => Ty::Socket,
            sema::Ty::Namespace => Ty::Namespace,
            sema::Ty::Error => {
                // Sema fails the build before lowering on real errors.
                unreachable!("error type survived sema")
            }
        }
    }

    fn gate(&mut self, span: Span, msg: &str) {
        self.diagnostics.push(
            Diagnostic::error(
                ErrorCode::NOT_IMPLEMENTED,
                format!("{msg} (not yet implemented)"),
            )
            .with_span(span),
        );
    }

    fn gated_expr(&mut self, span: Span, ty: Ty, msg: &str) -> Expr {
        self.gate(span, msg);
        Expr {
            ty,
            span,
            kind: ExprKind::Undefined,
        }
    }

    // --- classes ----------------------------------------------------------

    /// Prepends implicit `super()` (when the source constructor has no
    /// explicit call) and field initializers to each class's effective
    /// constructor.
    ///
    /// Deviation note: AVM2 runs instance initializers via iinit before the
    /// constructor body; with an explicit `super(...)` placed mid-body our
    /// initializers still run first (documented; revisit with exceptions).
    fn inject_ctor_prologues(&mut self, functions: &mut [Function]) {
        for (index, info) in self.program.registry.classes.iter().enumerate() {
            if index == 0 {
                continue; // Object root
            }
            let span = info.span;
            let mut prologue: Vec<Stmt> = Vec::new();
            // Implicit super chain: only when the user wrote none.
            let explicit_super = info
                .ctor
                .map(|f| tast_has_super(&self.program.functions[f.0 as usize].body))
                .unwrap_or(false);
            if !explicit_super
                && let Some(parent) = info.parent
                && parent.0 != 0
            {
                prologue.push(Stmt::Expr(Expr {
                    ty: Ty::Void,
                    span,
                    kind: ExprKind::CallDirect {
                        fn_id: self.ctor_fn[parent.0 as usize],
                        recv: Box::new(this_expr(parent.0, span)),
                        args: self.ctor_default_args(parent.0),
                    },
                }));
            }
            for f in &info.fields {
                let Some(init) = &f.init else { continue };
                let value = self.expr(init);
                prologue.push(Stmt::Expr(Expr {
                    ty: value.ty,
                    span,
                    kind: ExprKind::FieldSet(
                        Box::new(this_expr(index as u32, span)),
                        index as u32,
                        f.slot,
                        Box::new(value),
                    ),
                }));
            }
            if prologue.is_empty() {
                continue;
            }
            let ctor = self.ctor_fn[index];
            let body = &mut functions[ctor.0 as usize].body;
            prologue.append(body);
            *body = prologue;
        }
    }

    /// Default arguments for a zero-arg call of `class`'s constructor
    /// (sema guaranteed all params are defaulted when the chain is
    /// implicit).
    fn ctor_default_args(&mut self, class: u32) -> Vec<Expr> {
        let Some(ctor) = self.program.registry.classes[class as usize].ctor else {
            return Vec::new();
        };
        let callee = &self.program.functions[ctor.0 as usize];
        (0..callee.param_count)
            .filter_map(|i| callee.locals[i].default.as_ref())
            .map(|d| self.expr(d))
            .collect()
    }

    fn lower_classes(&mut self) -> Vec<Class> {
        let registry = &self.program.registry;
        let mut classes = Vec::with_capacity(registry.classes.len());
        for (index, info) in registry.classes.iter().enumerate() {
            let span = info.span;
            // Full slot table: ancestors own the low slots.
            let mut slot_tys = vec![(Ty::Any, span); info.total_slots];
            let mut cur = Some(sema::ClassId(index as u32));
            while let Some(c) = cur {
                let ci = &registry.classes[c.0 as usize];
                for f in &ci.fields {
                    slot_tys[f.slot] = (Ty::Any, span); // placeholder, fixed below
                }
                cur = ci.parent;
            }
            // Resolve real slot types (separate pass to appease borrows).
            let mut slots = vec![Ty::Any; info.total_slots];
            let mut cur = Some(sema::ClassId(index as u32));
            let mut pending: Vec<(usize, sema::Ty)> = Vec::new();
            while let Some(c) = cur {
                let ci = &registry.classes[c.0 as usize];
                for f in &ci.fields {
                    pending.push((f.slot, f.ty));
                }
                cur = ci.parent;
            }
            for (slot, ty) in pending {
                slots[slot] = self.ty(ty, span);
            }
            let registry = &self.program.registry;
            let info = &registry.classes[index];

            let vtable: Vec<FnId> = info.vtable.iter().map(|m| FnId(m.fn_id.0)).collect();
            let to_string = registry
                .find_vmethod(sema::ClassId(index as u32), "toString", sema::VKind::Method)
                .filter(|(_, v)| v.sig.params.is_empty() && v.sig.ret == sema::Ty::String)
                .map(|(_, v)| FnId(v.fn_id.0));
            // Interfaces from the whole ancestor chain: each class carries
            // its own tables so overrides dispatch correctly through
            // interface-typed references.
            let mut all_ifaces: Vec<u32> = Vec::new();
            let mut cur = Some(sema::ClassId(index as u32));
            while let Some(c) = cur {
                let ci = &registry.classes[c.0 as usize];
                for i in &ci.interfaces {
                    if !all_ifaces.contains(&i.0) {
                        all_ifaces.push(i.0);
                    }
                }
                cur = ci.parent;
            }
            let ifaces = all_ifaces
                .into_iter()
                .map(|iface| {
                    let table = registry.ifaces[iface as usize]
                        .methods
                        .iter()
                        .map(|m| {
                            registry
                                .find_vmethod(sema::ClassId(index as u32), &m.name, m.kind)
                                .map(|(_, v)| FnId(v.fn_id.0))
                                .unwrap_or(FnId(0)) // conformance checked by sema
                        })
                        .collect();
                    (iface, table)
                })
                .collect();
            let mut statics = Vec::new();
            let static_tys: Vec<sema::Ty> = info.static_fields.iter().map(|f| f.ty).collect();
            for ty in static_tys {
                statics.push(self.ty(ty, span));
            }
            let registry = &self.program.registry;
            let info = &registry.classes[index];
            classes.push(Class {
                name: info.qualified(),
                parent: info.parent.map(|p| p.0),
                slots,
                vtable,
                ctor: self.ctor_fn[index],
                ifaces,
                to_string,
                is_dynamic: info.is_dynamic,
                ns_members: {
                    // Reflection rows for runtime-qualified access: own
                    // namespaced fields + every namespaced vtable entry
                    // (chain walk stops at the first match, so child
                    // tables win; Virtual wrappers keep dispatch dynamic).
                    let mut rows = Vec::new();
                    for f in &info.fields {
                        if let Some((ns, raw)) = split_ns_name(&f.name) {
                            rows.push(NsMemberInfo {
                                ns,
                                name: raw,
                                field_slot: Some(f.slot as u32),
                                field_ty: Some(self.ty(f.ty, span)),
                                vslot: None,
                            });
                        }
                    }
                    let registry = &self.program.registry;
                    let info = &registry.classes[index];
                    for (vslot, m) in info.vtable.iter().enumerate() {
                        if m.kind != sema::VKind::Method {
                            continue;
                        }
                        if let Some((ns, raw)) = split_ns_name(&m.name) {
                            rows.push(NsMemberInfo {
                                ns,
                                name: raw,
                                field_slot: None,
                                field_ty: None,
                                vslot: Some(vslot as u32),
                            });
                        }
                    }
                    rows
                },
                statics,
            });
        }
        classes
    }

    /// Static initializer statements for the script prologue.
    fn static_init_stmts(&mut self) -> Vec<Stmt> {
        let mut out = Vec::new();
        for index in 0..self.program.registry.classes.len() {
            let info = &self.program.registry.classes[index];
            let span = info.span;
            let inits: Vec<(usize, &sema::TExpr)> = info
                .static_fields
                .iter()
                .filter_map(|f| f.init.as_ref().map(|e| (f.index, e)))
                .collect();
            for (sindex, init) in inits {
                let value = self.expr(init);
                out.push(Stmt::Expr(Expr {
                    ty: value.ty,
                    span,
                    kind: ExprKind::StaticSet(index as u32, sindex, Box::new(value)),
                }));
            }
        }
        out
    }

    // --- statements -----------------------------------------------------

    fn stmt(&mut self, stmt: &sema::TStmt) -> Stmt {
        use sema::TStmtKind as S;
        match &stmt.kind {
            S::Expr(e) => Stmt::Expr(self.expr(e)),
            S::Assign(local, e) => Stmt::Assign(LocalId(local.0), self.expr(e)),
            S::Block(b) => Stmt::Block(b.iter().map(|s| self.stmt(s)).collect()),
            S::If {
                cond,
                then_branch,
                else_branch,
            } => Stmt::If {
                cond: self.expr(cond),
                then_branch: Box::new(self.stmt(then_branch)),
                else_branch: else_branch.as_ref().map(|e| Box::new(self.stmt(e))),
            },
            S::While { cond, body } => Stmt::While {
                label: None,
                cond: self.expr(cond),
                body: Box::new(self.stmt(body)),
            },
            S::DoWhile { body, cond } => Stmt::DoWhile {
                label: None,
                body: Box::new(self.stmt(body)),
                cond: self.expr(cond),
            },
            S::For {
                init,
                cond,
                update,
                body,
            } => Stmt::For {
                label: None,
                init: init.as_ref().map(|i| Box::new(self.stmt(i))),
                cond: cond.as_ref().map(|c| self.expr(c)),
                update: update.as_ref().map(|u| self.expr(u)),
                body: Box::new(self.stmt(body)),
            },
            S::ForIn {
                is_each,
                target,
                object,
                body,
            } => self.lower_for_in(*is_each, *target, object, body, stmt.span),
            S::Switch { scrutinee, cases } => Stmt::Switch {
                scrutinee: self.expr(scrutinee),
                cases: cases
                    .iter()
                    .map(|c| Case {
                        test: c.test.as_ref().map(|t| self.expr(t)),
                        body: c.body.iter().map(|s| self.stmt(s)).collect(),
                    })
                    .collect(),
            },
            S::Break { label } => Stmt::Break {
                label: label.clone(),
            },
            S::Continue { label } => Stmt::Continue {
                label: label.clone(),
            },
            S::Return { value } => Stmt::Return {
                value: value.as_ref().map(|v| self.expr(v)),
            },
            S::Throw { value } => Stmt::Throw(self.expr(value)),
            S::Try {
                block,
                catches,
                finally,
            } => Stmt::Try {
                body: block.iter().map(|s| self.stmt(s)).collect(),
                catches: catches
                    .iter()
                    .map(|c| Catch {
                        binding: LocalId(c.binding.0),
                        body: c.body.iter().map(|s| self.stmt(s)).collect(),
                    })
                    .collect(),
                finally: finally
                    .as_ref()
                    .map(|f| f.iter().map(|s| self.stmt(s)).collect()),
            },
            S::Labeled { label, body } => {
                // Attach the label to the loop it names; labeled non-loops
                // are a later phase (nothing in the core corpus needs them).
                let lowered = self.stmt(body);
                match lowered {
                    Stmt::While { cond, body, .. } => Stmt::While {
                        label: Some(label.clone()),
                        cond,
                        body,
                    },
                    Stmt::DoWhile { body, cond, .. } => Stmt::DoWhile {
                        label: Some(label.clone()),
                        body,
                        cond,
                    },
                    Stmt::For {
                        init,
                        cond,
                        update,
                        body,
                        ..
                    } => Stmt::For {
                        label: Some(label.clone()),
                        init,
                        cond,
                        update,
                        body,
                    },
                    other => {
                        self.gate(stmt.span, "labels on non-loop statements — Phase 6");
                        other
                    }
                }
            }
            S::Empty => Stmt::Empty,
        }
    }

    // --- expressions ------------------------------------------------------

    fn expr(&mut self, e: &sema::TExpr) -> Expr {
        use sema::TExprKind as E;
        let span = e.span;
        let ty = self.ty(e.ty, span);
        let kind = match &e.kind {
            E::Int(v) => ExprKind::Int(*v),
            E::UInt(v) => ExprKind::UInt(*v),
            E::Number(v) => ExprKind::Number(*v),
            E::Str(v) => ExprKind::Str(v.clone()),
            E::RegExp(pat, flags) => ExprKind::RegExpLit(pat.clone(), flags.clone()),
            E::NewRegExp(args) => ExprKind::NewRegExp(args.iter().map(|a| self.expr(a)).collect()),
            E::NewDate(args) => ExprKind::NewDate(args.iter().map(|a| self.expr(a)).collect()),
            E::NamespaceVal(id) => ExprKind::NamespaceVal(*id),
            E::NewNamespace(uri) => ExprKind::NewNamespace(Box::new(self.expr(uri))),
            E::NsGet(recv, q, name) => ExprKind::NsGet(
                Box::new(self.expr(recv)),
                Box::new(self.expr(q)),
                name.clone(),
            ),
            E::NsCall(recv, q, name, args) => ExprKind::NsCall(
                Box::new(self.expr(recv)),
                Box::new(self.expr(q)),
                name.clone(),
                args.iter().map(|a| self.expr(a)).collect(),
            ),
            E::Bool(v) => ExprKind::Bool(*v),
            E::Null => ExprKind::Null,
            E::Undefined => ExprKind::Undefined,
            E::LocalGet(id) => ExprKind::LocalGet(LocalId(id.0)),
            E::LocalSet(id, v) => ExprKind::LocalSet(LocalId(id.0), Box::new(self.expr(v))),
            E::CallFn(id, args) => {
                // A capturing function's machine signature carries a hidden
                // environment parameter; direct calls don't build one yet.
                // Same v1 limitation as taking such a function's reference.
                if !self.program.functions[id.0 as usize].captures.is_empty() {
                    return self.gated_expr(
                        span,
                        ty,
                        "calling a capturing function by name — assign it to a Function                          value first, or pass state via parameters/returns",
                    );
                }
                let mut lowered: Vec<Expr> = args.iter().map(|a| self.expr(a)).collect();
                // Fill omitted defaulted arguments at the callsite (AVM2
                // fills from the method's option list; same effect).
                let callee = &self.program.functions[id.0 as usize];
                for i in lowered.len()..callee.param_count {
                    if let Some(default) = &callee.locals[i].default {
                        lowered.push(self.expr(default));
                    }
                }
                self.bundle_rest(sema::FnId(id.0), &mut lowered, span);
                ExprKind::CallFn(FnId(id.0), lowered)
            }
            E::CallBuiltin(b, args) => self.builtin_call(*b, args, span),
            E::CallMethod(receiver, name, args) => {
                return self.method_call(receiver, name, args, ty, span);
            }
            E::Member(receiver, name) => {
                if receiver.ty == sema::Ty::String && name == "length" {
                    ExprKind::StrLen(Box::new(self.expr(receiver)))
                } else if receiver.ty == sema::Ty::Namespace {
                    // Only `uri` exists (sema member table).
                    ExprKind::NsUri(Box::new(self.expr(receiver)))
                } else if matches!(receiver.ty, sema::Ty::Socket | sema::Ty::ServerSocket) {
                    // Only `localPort` exists (sema member table).
                    ExprKind::CallSocket(SocketOp::LocalPort, vec![self.expr(receiver)])
                } else if receiver.ty == sema::Ty::RegExp {
                    let op = match name.as_str() {
                        "source" => RegexOp::Source,
                        "global" => RegexOp::Global,
                        "ignoreCase" => RegexOp::IgnoreCase,
                        "multiline" => RegexOp::Multiline,
                        "lastIndex" => RegexOp::LastIndex,
                        other => unreachable!("sema admitted RegExp.{other}"),
                    };
                    ExprKind::CallRegex(op, vec![self.expr(receiver)])
                } else {
                    // Boxed receiver: runtime property lookup (SPECS §3.2).
                    ExprKind::PropGet(Box::new(self.expr(receiver)), name.clone())
                }
            }
            E::MemberSet(receiver, name, value) => ExprKind::PropSet(
                Box::new(self.expr(receiver)),
                name.clone(),
                Box::new(self.expr(value)),
            ),
            E::Index(recv, idx) if matches!(recv.ty, sema::Ty::Array | sema::Ty::Vector(_)) => {
                ExprKind::SeqGet(Box::new(self.expr(recv)), Box::new(self.expr(idx)))
            }
            E::IndexSet(recv, idx, v)
                if matches!(recv.ty, sema::Ty::Array | sema::Ty::Vector(_)) =>
            {
                ExprKind::SeqSet(
                    Box::new(self.expr(recv)),
                    Box::new(self.expr(idx)),
                    Box::new(self.expr(v)),
                )
            }
            E::Index(recv, idx) => {
                ExprKind::AnyIndexGet(Box::new(self.expr(recv)), Box::new(self.expr(idx)))
            }
            E::IndexSet(recv, idx, value) => ExprKind::AnyIndexSet(
                Box::new(self.expr(recv)),
                Box::new(self.expr(idx)),
                Box::new(self.expr(value)),
            ),
            E::Array(elements) => ExprKind::ArrayLit(
                elements
                    .iter()
                    .map(|el| el.as_ref().map(|e| self.expr(e)))
                    .collect(),
            ),
            E::Object(props) => ExprKind::ObjectLit(
                props
                    .iter()
                    .map(|(k, v)| (k.clone(), self.expr(v)))
                    .collect(),
            ),
            E::FnRef(id) => {
                if !self.program.functions[id.0 as usize].captures.is_empty() {
                    return self.gated_expr(
                        span,
                        ty,
                        "referencing a capturing sibling before its closure exists — restructure (Phase 7)",
                    );
                }
                ExprKind::FnValue(FnId(id.0))
            }
            E::BuiltinRef(b) => {
                let b = match b {
                    sema::BuiltinFn::EncodeUriComponent => Builtin::EncodeUriComponent,
                    sema::BuiltinFn::DecodeUriComponent => Builtin::DecodeUriComponent,
                    sema::BuiltinFn::Escape => Builtin::Escape,
                    sema::BuiltinFn::Unescape => Builtin::Unescape,
                    sema::BuiltinFn::Trace => Builtin::Trace,
                    sema::BuiltinFn::ParseInt => Builtin::ParseInt,
                    sema::BuiltinFn::ParseFloat => Builtin::ParseFloat,
                    sema::BuiltinFn::IsNaN => Builtin::IsNaN,
                    sema::BuiltinFn::IsFinite => Builtin::IsFinite,
                };
                ExprKind::BuiltinValue(b)
            }
            E::CallIndirect(callee, args) => ExprKind::CallFnValue {
                callee: Box::new(self.expr(callee)),
                this_arg: None,
                args: args.iter().map(|a| self.expr(a)).collect(),
                is_apply: false,
            },
            E::Unary(op, v) => return self.unary(*op, v, ty, span),
            E::Postfix(op, v) => {
                return self.inc_dec(matches!(op, ast::PostfixOp::Inc), false, v, ty, span);
            }
            E::Binary(op, l, r) => return self.binary(*op, l, r, ty, span),
            E::Logical(op, l, r) => ExprKind::Logical {
                is_and: matches!(op, ast::BinaryOp::LogAnd),
                lhs: Box::new(self.expr(l)),
                rhs: Box::new(self.expr(r)),
            },
            E::Conditional(c, t, f) => ExprKind::Conditional(
                Box::new(self.expr(c)),
                Box::new(self.expr(t)),
                Box::new(self.expr(f)),
            ),
            E::Is(v, target) => {
                let target = self.ty(*target, span);
                ExprKind::Is(Box::new(self.expr(v)), target)
            }
            E::As(v, target) => {
                let target = self.ty(*target, span);
                ExprKind::As(Box::new(self.expr(v)), target)
            }
            E::Coerce(c, v) => {
                let conv = match c {
                    sema::Coercion::ToInt => Conv::ToInt,
                    sema::Coercion::ToUInt => Conv::ToUInt,
                    sema::Coercion::ToNumber => Conv::ToNumber,
                    sema::Coercion::ToBoolean => Conv::ToBoolean,
                    sema::Coercion::ToString => Conv::ToString,
                    sema::Coercion::ToAny => Conv::ToAny,
                    sema::Coercion::FromAny => match e.ty {
                        // AVM2 coerce_s: null/undefined → null, else ToString.
                        sema::Ty::String => Conv::AnyToString,
                        // AVM2 coerce: checked class/interface conversion.
                        sema::Ty::Class(c) => Conv::AnyToObject(c.0),
                        sema::Ty::Iface(i) => Conv::AnyToIface(i.0),
                        sema::Ty::Array => Conv::AnyToArray,
                        sema::Ty::Vector(i) => Conv::AnyToVector(i),
                        sema::Ty::RegExp => Conv::AnyToRegExp,
                        sema::Ty::Date => Conv::AnyToDate,
                        sema::Ty::Socket | sema::Ty::ServerSocket => Conv::AnyToSocket,
                        // Unchecked in v1: a Namespace-typed slot only
                        // ever holds interned namespaces.
                        sema::Ty::Namespace => {
                            return self.gated_expr(
                                span,
                                ty,
                                "checked `*` to Namespace coercion (declare the variable as Namespace)",
                            );
                        }
                        _ => {
                            return self.gated_expr(
                                span,
                                ty,
                                "checked `*` coercion to this type — Phase 6",
                            );
                        }
                    },
                };
                ExprKind::Conv(conv, Box::new(self.expr(v)))
            }
            E::Comma(l, r) => ExprKind::Comma(Box::new(self.expr(l)), Box::new(self.expr(r))),
            E::This => ExprKind::This,
            E::New(class, args) => {
                let mut lowered: Vec<Expr> = args.iter().map(|a| self.expr(a)).collect();
                self.fill_method_defaults(
                    self.program.registry.classes[class.0 as usize].ctor,
                    &mut lowered,
                );
                ExprKind::New(class.0, lowered)
            }
            E::FieldGet(o, class, slot) => {
                ExprKind::FieldGet(Box::new(self.expr(o)), class.0, *slot)
            }
            E::FieldSet(o, class, slot, v) => ExprKind::FieldSet(
                Box::new(self.expr(o)),
                class.0,
                *slot,
                Box::new(self.expr(v)),
            ),
            E::CallVirtual {
                recv,
                class,
                vslot,
                args,
            } => {
                let mut lowered: Vec<Expr> = args.iter().map(|a| self.expr(a)).collect();
                // Defaults come from the static target (deviation: an
                // override changing default values is resolved statically).
                let target = self.program.registry.classes[class.0 as usize].vtable[*vslot].fn_id;
                self.fill_method_defaults(Some(target), &mut lowered);
                ExprKind::CallVirtual {
                    recv: Box::new(self.expr(recv)),
                    class: class.0,
                    vslot: *vslot,
                    args: lowered,
                }
            }
            E::CallIface {
                recv,
                iface,
                islot,
                args,
            } => {
                // Interface dispatch can't know the implementation's
                // defaults; require all declared parameters for now.
                let m = &self.program.registry.ifaces[iface.0 as usize].methods[*islot];
                if args.len() < m.sig.params.len() {
                    return self.gated_expr(
                        span,
                        ty,
                        "omitting optional arguments in interface calls — Phase 6",
                    );
                }
                let ret = self.ty(m.sig.ret, span);
                ExprKind::CallIface {
                    recv: Box::new(self.expr(recv)),
                    iface: iface.0,
                    islot: *islot,
                    ret,
                    args: args.iter().map(|a| self.expr(a)).collect(),
                }
            }
            E::CallDirect { fn_id, recv, args } => {
                let mut lowered: Vec<Expr> = args.iter().map(|a| self.expr(a)).collect();
                self.fill_method_defaults(Some(sema::FnId(fn_id.0)), &mut lowered);
                ExprKind::CallDirect {
                    fn_id: FnId(fn_id.0),
                    recv: Box::new(self.expr(recv)),
                    args: lowered,
                }
            }
            E::SuperCtor(parent, args) => {
                let mut lowered: Vec<Expr> = args.iter().map(|a| self.expr(a)).collect();
                self.fill_method_defaults(
                    self.program.registry.classes[parent.0 as usize].ctor,
                    &mut lowered,
                );
                ExprKind::CallDirect {
                    fn_id: self.ctor_fn[parent.0 as usize],
                    recv: Box::new(this_expr(parent.0, span)),
                    args: lowered,
                }
            }
            E::StaticGet(class, index) => ExprKind::StaticGet(class.0, *index),
            E::StaticSet(class, index, v) => {
                ExprKind::StaticSet(class.0, *index, Box::new(self.expr(v)))
            }
            E::VectorLit(inst, elements) => {
                ExprKind::VectorLit(*inst, elements.iter().map(|e| self.expr(e)).collect())
            }
            E::CallArr(m, recv, args) => {
                use sema::ArrMethod as A;
                let m = match m {
                    A::Push => ArrMethod::Push,
                    A::Pop => ArrMethod::Pop,
                    A::Shift => ArrMethod::Shift,
                    A::Unshift => ArrMethod::Unshift,
                    A::Slice => ArrMethod::Slice,
                    A::Splice => ArrMethod::Splice,
                    A::IndexOf => ArrMethod::IndexOf,
                    A::Concat => ArrMethod::Concat,
                    A::Join => ArrMethod::Join,
                    A::Reverse => ArrMethod::Reverse,
                    A::Sort => ArrMethod::Sort,
                    A::ForEach => ArrMethod::ForEach,
                    A::Map => ArrMethod::Map,
                    A::Filter => ArrMethod::Filter,
                    A::Some => ArrMethod::SomeM,
                    A::Every => ArrMethod::Every,
                };
                ExprKind::CallArr(
                    m,
                    Box::new(self.expr(recv)),
                    args.iter().map(|a| self.expr(a)).collect(),
                )
            }
            E::CallVec(m, recv, args) => {
                use sema::VecMethod as V;
                let m = match m {
                    V::Push => VecMethod::Push,
                    V::Pop => VecMethod::Pop,
                    V::Shift => VecMethod::Shift,
                    V::Unshift => VecMethod::Unshift,
                    V::Slice => VecMethod::Slice,
                    V::IndexOf => VecMethod::IndexOf,
                    V::Join => VecMethod::Join,
                    V::Reverse => VecMethod::Reverse,
                };
                ExprKind::CallVec(
                    m,
                    Box::new(self.expr(recv)),
                    args.iter().map(|a| self.expr(a)).collect(),
                )
            }
            E::SeqLen(o) => ExprKind::SeqLen(Box::new(self.expr(o))),
            E::SeqSetLen(o, v) => {
                ExprKind::SeqSetLen(Box::new(self.expr(o)), Box::new(self.expr(v)))
            }
            E::CaptureGet(i) => ExprKind::CaptureGet(*i),
            E::CaptureSet(i, v) => ExprKind::CaptureSet(*i, Box::new(self.expr(v))),
            E::Closure(id) => ExprKind::Closure(FnId(id.0)),
            E::BoundMethod(recv, class, vslot) => {
                ExprKind::BoundMethod(Box::new(self.expr(recv)), class.0, *vslot)
            }
            E::CallFunctionValue {
                callee,
                this_arg,
                args,
                is_apply,
            } => ExprKind::CallFnValue {
                callee: Box::new(self.expr(callee)),
                this_arg: this_arg.as_ref().map(|t| Box::new(self.expr(t))),
                args: args.iter().map(|a| self.expr(a)).collect(),
                is_apply: *is_apply,
            },
            E::CallNative(f, args) => {
                ExprKind::CallNative(*f, args.iter().map(|a| self.expr(a)).collect())
            }
            E::HasProp(key, obj) => {
                ExprKind::HasProp(Box::new(self.expr(key)), Box::new(self.expr(obj)))
            }
            E::DeleteProp(obj, key) => {
                // Sema guarantees a string-literal key here.
                let name = match &key.kind {
                    sema::TExprKind::Str(s) => s.clone(),
                    _ => String::new(),
                };
                ExprKind::DeleteProp(Box::new(self.expr(obj)), name)
            }
            E::Error => unreachable!("error expr survived sema"),
        };
        Expr { ty, span, kind }
    }

    /// Desugars `for..in`/`for each..in` into an index loop over the boxed
    /// receiver: `for (i = 0; i < enumLen(o); i++) target = key/value(o, i)`.
    fn lower_for_in(
        &mut self,
        is_each: bool,
        target: sema::LocalId,
        object: &sema::TExpr,
        body: &sema::TStmt,
        span: Span,
    ) -> Stmt {
        // Temps live in the current function (appended past sema's locals).
        let obj_t = self.add_temp(Ty::Any);
        let idx_t = self.add_temp(Ty::Int);
        let obj = self.expr(object);
        let boxed = if object.ty == sema::Ty::Any {
            obj
        } else {
            Expr {
                ty: Ty::Any,
                span,
                kind: ExprKind::Conv(Conv::ToAny, Box::new(obj)),
            }
        };
        let target_ty = {
            let f = &self.program.functions[self.current_fn];
            let t = f.locals[target.0 as usize].ty;
            self.ty(t, span)
        };
        let fetch = Expr {
            ty: Ty::Any,
            span,
            kind: if is_each {
                ExprKind::EnumValue(
                    Box::new(local_get(obj_t, Ty::Any, span)),
                    Box::new(local_get(idx_t, Ty::Int, span)),
                )
            } else {
                ExprKind::EnumKey(
                    Box::new(local_get(obj_t, Ty::Any, span)),
                    Box::new(local_get(idx_t, Ty::Int, span)),
                )
            },
        };
        // Coerce the fetched Any into the declared target type.
        let assigned = coerce_any_to(fetch, target_ty, span);
        Stmt::For {
            label: None,
            init: Some(Box::new(Stmt::Block(vec![
                Stmt::Assign(obj_t, boxed),
                Stmt::Assign(idx_t, int_lit(0, span)),
            ]))),
            cond: Some(Expr {
                ty: Ty::Boolean,
                span,
                kind: ExprKind::Binary(
                    BinOp::Lt,
                    Box::new(Expr {
                        ty: Ty::Number,
                        span,
                        kind: ExprKind::Conv(
                            Conv::ToNumber,
                            Box::new(local_get(idx_t, Ty::Int, span)),
                        ),
                    }),
                    Box::new(Expr {
                        ty: Ty::Number,
                        span,
                        kind: ExprKind::Conv(
                            Conv::ToNumber,
                            Box::new(Expr {
                                ty: Ty::Int,
                                span,
                                kind: ExprKind::EnumLen(Box::new(local_get(obj_t, Ty::Any, span))),
                            }),
                        ),
                    }),
                ),
            }),
            update: Some(Expr {
                ty: Ty::Int,
                span,
                kind: ExprKind::IncDec {
                    target: idx_t,
                    is_inc: true,
                    is_prefix: false,
                },
            }),
            body: Box::new(Stmt::Block(vec![
                Stmt::Assign(LocalId(target.0), assigned),
                self.stmt(body),
            ])),
        }
    }

    /// Bundles trailing arguments into the callee's `...rest` Array
    /// parameter (SPECS §6).
    fn bundle_rest(&mut self, callee: sema::FnId, lowered: &mut Vec<Expr>, span: Span) {
        let f = &self.program.functions[callee.0 as usize];
        if f.param_count == 0 || !f.locals[f.param_count - 1].is_rest {
            return;
        }
        let rest_at = f.param_count - 1;
        let extra: Vec<Expr> = lowered.split_off(rest_at.min(lowered.len()));
        lowered.push(Expr {
            ty: Ty::Array,
            span,
            kind: ExprKind::ArrayLit(extra.into_iter().map(Some).collect()),
        });
    }

    /// Appends omitted defaulted arguments from a sema function's parameter
    /// list (AVM2 fills from the method's option list).
    fn fill_method_defaults(&mut self, callee: Option<sema::FnId>, lowered: &mut Vec<Expr>) {
        let Some(callee) = callee else { return };
        let callee = &self.program.functions[callee.0 as usize];
        for i in lowered.len()..callee.param_count {
            if let Some(default) = &callee.locals[i].default {
                lowered.push(self.expr(default));
            }
        }
    }

    fn unary(&mut self, op: ast::UnaryOp, v: &sema::TExpr, ty: Ty, span: Span) -> Expr {
        use ast::UnaryOp as U;
        let kind = match op {
            U::Not => ExprKind::Unary(UnOp::Not, Box::new(self.expr(v))),
            U::BitNot => ExprKind::Unary(UnOp::BitNot, Box::new(self.expr(v))),
            U::Minus => ExprKind::Unary(UnOp::Neg, Box::new(self.expr(v))),
            // Unary plus is exactly ToNumber (ES3 §11.4.6); sema already
            // coerced the operand.
            U::Plus => return self.expr(v),
            U::Typeof => ExprKind::Unary(UnOp::TypeOf, Box::new(self.expr(v))),
            // `void e`: evaluate, yield undefined (ES3 §11.4.2).
            U::Void => ExprKind::Comma(
                Box::new(self.expr(v)),
                Box::new(Expr {
                    ty: Ty::Any,
                    span,
                    kind: ExprKind::Undefined,
                }),
            ),
            U::Delete => {
                return self.gated_expr(span, ty, "`delete` needs the object model — Phase 4");
            }
            U::PreInc => return self.inc_dec(true, true, v, ty, span),
            U::PreDec => return self.inc_dec(false, true, v, ty, span),
        };
        Expr { ty, span, kind }
    }

    fn inc_dec(
        &mut self,
        is_inc: bool,
        is_prefix: bool,
        operand: &sema::TExpr,
        ty: Ty,
        span: Span,
    ) -> Expr {
        // Sema guaranteed a numeric result; the P3 backend handles direct
        // numeric locals (the common case). Coerced targets (e.g. `*`
        // locals) need store-back boxing — Phase 4.
        if let sema::TExprKind::LocalGet(id) = &operand.kind {
            return Expr {
                ty,
                span,
                kind: ExprKind::IncDec {
                    target: LocalId(id.0),
                    is_inc,
                    is_prefix,
                },
            };
        }
        if let sema::TExprKind::CaptureGet(slot) = &operand.kind {
            return Expr {
                ty,
                span,
                kind: ExprKind::CaptureIncDec {
                    slot: *slot,
                    is_inc,
                    is_prefix,
                },
            };
        }
        if let sema::TExprKind::StaticGet(class, index) = &operand.kind {
            return Expr {
                ty,
                span,
                kind: ExprKind::StaticIncDec {
                    class: class.0,
                    index: *index,
                    is_inc,
                    is_prefix,
                },
            };
        }
        if let sema::TExprKind::FieldGet(recv, class, slot) = &operand.kind {
            return Expr {
                ty,
                span,
                kind: ExprKind::FieldIncDec {
                    recv: Box::new(self.expr(recv)),
                    class: class.0,
                    slot: *slot,
                    is_inc,
                    is_prefix,
                },
            };
        }
        self.gated_expr(span, ty, "++/-- on this kind of target — Phase 6")
    }

    fn binary(
        &mut self,
        op: ast::BinaryOp,
        l: &sema::TExpr,
        r: &sema::TExpr,
        ty: Ty,
        span: Span,
    ) -> Expr {
        use ast::BinaryOp as B;
        let mapped = match op {
            B::Add => BinOp::Add,
            B::Sub => BinOp::Sub,
            B::Mul => BinOp::Mul,
            B::Div => BinOp::Div,
            B::Rem => BinOp::Rem,
            B::Shl => BinOp::Shl,
            B::Shr => BinOp::Shr,
            B::Ushr => BinOp::Ushr,
            B::BitAnd => BinOp::BitAnd,
            B::BitOr => BinOp::BitOr,
            B::BitXor => BinOp::BitXor,
            B::Lt => BinOp::Lt,
            B::Gt => BinOp::Gt,
            B::Le => BinOp::Le,
            B::Ge => BinOp::Ge,
            B::Eq => BinOp::Eq,
            B::Ne => BinOp::Ne,
            B::StrictEq => BinOp::StrictEq,
            B::StrictNe => BinOp::StrictNe,
            B::In => {
                return self.gated_expr(span, ty, "`in` needs the object model — Phase 4");
            }
            B::Instanceof => {
                return self.gated_expr(
                    span,
                    ty,
                    "`instanceof` needs prototype chains — Phase 4 (prefer `is`)",
                );
            }
            B::Is | B::As | B::LogAnd | B::LogOr => unreachable!("handled by sema lowering"),
        };
        Expr {
            ty,
            span,
            kind: ExprKind::Binary(mapped, Box::new(self.expr(l)), Box::new(self.expr(r))),
        }
    }

    fn builtin_call(&mut self, b: sema::BuiltinFn, args: &[sema::TExpr], span: Span) -> ExprKind {
        let mut lowered: Vec<Expr> = args.iter().map(|a| self.expr(a)).collect();
        let builtin = match b {
            sema::BuiltinFn::EncodeUriComponent => Builtin::EncodeUriComponent,
            sema::BuiltinFn::DecodeUriComponent => Builtin::DecodeUriComponent,
            sema::BuiltinFn::Escape => Builtin::Escape,
            sema::BuiltinFn::Unescape => Builtin::Unescape,
            sema::BuiltinFn::Trace => Builtin::Trace,
            // Defaults per avmplus actionscript.lang.as:41-53.
            sema::BuiltinFn::ParseInt => {
                if lowered.is_empty() {
                    lowered.push(str_lit("NaN", span));
                }
                if lowered.len() == 1 {
                    lowered.push(int_lit(0, span));
                }
                Builtin::ParseInt
            }
            sema::BuiltinFn::ParseFloat => {
                if lowered.is_empty() {
                    lowered.push(str_lit("NaN", span));
                }
                Builtin::ParseFloat
            }
            sema::BuiltinFn::IsNaN | sema::BuiltinFn::IsFinite => {
                if lowered.is_empty() {
                    lowered.push(num_lit(f64::NAN, span));
                }
                if b == sema::BuiltinFn::IsNaN {
                    Builtin::IsNaN
                } else {
                    Builtin::IsFinite
                }
            }
        };
        ExprKind::CallBuiltin(builtin, lowered)
    }

    fn method_call(
        &mut self,
        receiver: &sema::TExpr,
        name: &str,
        args: &[sema::TExpr],
        ty: Ty,
        span: Span,
    ) -> Expr {
        let recv_ty = receiver.ty;
        let mut lowered: Vec<Expr> = args.iter().map(|a| self.expr(a)).collect();
        // Argument defaults per avmplus core/String.as & core/Number.as
        // (cited in sema::builtins).
        let fill_num = |lowered: &mut Vec<Expr>, i: usize, v: f64| {
            if lowered.len() <= i {
                lowered.push(num_lit(v, span));
            }
        };
        match recv_ty {
            sema::Ty::String => {
                let method = match name {
                    "charAt" => {
                        fill_num(&mut lowered, 0, 0.0);
                        StrMethod::CharAt
                    }
                    "charCodeAt" => {
                        fill_num(&mut lowered, 0, 0.0);
                        StrMethod::CharCodeAt
                    }
                    "indexOf" => {
                        if lowered.is_empty() {
                            lowered.push(str_lit("undefined", span));
                        }
                        fill_num(&mut lowered, 1, 0.0);
                        StrMethod::IndexOf
                    }
                    "lastIndexOf" => {
                        if lowered.is_empty() {
                            lowered.push(str_lit("undefined", span));
                        }
                        fill_num(&mut lowered, 1, 0x7FFFFFFF as f64);
                        StrMethod::LastIndexOf
                    }
                    "slice" => {
                        fill_num(&mut lowered, 0, 0.0);
                        fill_num(&mut lowered, 1, 0x7FFFFFFF as f64);
                        StrMethod::Slice
                    }
                    "substring" => {
                        fill_num(&mut lowered, 0, 0.0);
                        fill_num(&mut lowered, 1, 0x7FFFFFFF as f64);
                        StrMethod::Substring
                    }
                    "substr" => {
                        fill_num(&mut lowered, 0, 0.0);
                        fill_num(&mut lowered, 1, 0x7FFFFFFF as f64);
                        StrMethod::Substr
                    }
                    // §15.5.4.11: regex pattern dispatches to the engine;
                    // a string pattern replaces the first occurrence.
                    "replace" => {
                        // Sema coerced the `*` pattern param; look through
                        // the box to see the original type.
                        fn is_regex(a: &sema::TExpr) -> bool {
                            match &a.kind {
                                sema::TExprKind::Coerce(_, inner) => is_regex(inner),
                                _ => a.ty == sema::Ty::RegExp,
                            }
                        }
                        if args.first().is_some_and(is_regex) {
                            let recv = self.expr(receiver);
                            // Drop the ToAny box: the engine wants the
                            // RegExp pointer itself.
                            let pat = match lowered.remove(0) {
                                Expr {
                                    kind: ExprKind::Conv(Conv::ToAny, inner),
                                    ..
                                } => *inner,
                                other => other,
                            };
                            return Expr {
                                ty,
                                span,
                                kind: ExprKind::CallRegex(
                                    RegexOp::Replace,
                                    vec![recv, pat, lowered.remove(0)],
                                ),
                            };
                        }
                        // Sema typed the pattern `*` to admit RegExp;
                        // unbox the string case.
                        let pat = lowered.remove(0);
                        lowered.insert(0, coerce_any_to(pat, Ty::String, span));
                        StrMethod::Replace
                    }
                    "match" | "search" => {
                        let op = if name == "match" {
                            RegexOp::Match
                        } else {
                            RegexOp::Search
                        };
                        let recv = self.expr(receiver);
                        return Expr {
                            ty,
                            span,
                            kind: ExprKind::CallRegex(op, vec![recv, lowered.remove(0)]),
                        };
                    }
                    "toLowerCase" => StrMethod::ToLowerCase,
                    "toUpperCase" => StrMethod::ToUpperCase,
                    "toString" => StrMethod::ToString,
                    // `a.concat(b, c)` is `a + b + c` (String.as:79).
                    "concat" => {
                        let mut acc = self.expr(receiver);
                        for arg in lowered {
                            // Sema typed variadic args `*`; concatenation
                            // stringifies them.
                            let arg = to_string_expr(arg, span);
                            acc = Expr {
                                ty: Ty::String,
                                span,
                                kind: ExprKind::Binary(BinOp::Add, Box::new(acc), Box::new(arg)),
                            };
                        }
                        return acc;
                    }
                    "split" => {
                        // Defaults per String.as:151 (delim/limit untyped).
                        if lowered.is_empty() {
                            lowered.push(str_lit(",", span));
                        }
                        if lowered.len() == 1 {
                            lowered.push(num_lit(4294967295.0, span));
                        }
                        StrMethod::Split
                    }
                    _ => return self.gated_expr(span, ty, "this String method — Phase 7"),
                };
                Expr {
                    ty,
                    span,
                    kind: ExprKind::CallStrMethod(method, Box::new(self.expr(receiver)), lowered),
                }
            }
            sema::Ty::RegExp => {
                let op = match name {
                    "test" => RegexOp::Test,
                    "exec" => RegexOp::Exec,
                    "toString" => RegexOp::ToString,
                    other => unreachable!("sema admitted RegExp.{other}()"),
                };
                let mut operands = vec![self.expr(receiver)];
                operands.append(&mut lowered);
                Expr {
                    ty,
                    span,
                    kind: ExprKind::CallRegex(op, operands),
                }
            }
            sema::Ty::Namespace => {
                // toString() = the canonical URI (ES4 §Namespace).
                debug_assert_eq!(name, "toString");
                Expr {
                    ty,
                    span,
                    kind: ExprKind::NsUri(Box::new(self.expr(receiver))),
                }
            }
            sema::Ty::Socket | sema::Ty::ServerSocket => {
                let op = match name {
                    "write" => SocketOp::Write,
                    "readLine" => SocketOp::ReadLine,
                    "read" => {
                        // Default chunk size (SPECS §6 read(max = 65536)).
                        if lowered.is_empty() {
                            lowered.push(Expr {
                                ty: Ty::Int,
                                span,
                                kind: ExprKind::Int(65536),
                            });
                        }
                        SocketOp::Read
                    }
                    "close" => SocketOp::Close,
                    "accept" => SocketOp::Accept,
                    other => unreachable!("sema admitted Socket.{other}()"),
                };
                let mut operands = vec![self.expr(receiver)];
                operands.append(&mut lowered);
                Expr {
                    ty,
                    span,
                    kind: ExprKind::CallSocket(op, operands),
                }
            }
            sema::Ty::Date => {
                // Index maps per runtime date.rs (avmplus getDateProperty
                // ordering).
                let f = match name {
                    "getTime" | "valueOf" => DateFn::Get(0),
                    "getFullYear" => DateFn::Get(1),
                    "getMonth" => DateFn::Get(2),
                    "getDate" => DateFn::Get(3),
                    "getDay" => DateFn::Get(4),
                    "getHours" => DateFn::Get(5),
                    "getMinutes" => DateFn::Get(6),
                    "getSeconds" => DateFn::Get(7),
                    "getMilliseconds" => DateFn::Get(8),
                    "getUTCFullYear" => DateFn::Get(9),
                    "getUTCMonth" => DateFn::Get(10),
                    "getUTCDate" => DateFn::Get(11),
                    "getUTCDay" => DateFn::Get(12),
                    "getUTCHours" => DateFn::Get(13),
                    "getUTCMinutes" => DateFn::Get(14),
                    "getUTCSeconds" => DateFn::Get(15),
                    "getUTCMilliseconds" => DateFn::Get(16),
                    "getTimezoneOffset" => DateFn::Get(17),
                    "setTime" => DateFn::SetTime,
                    "toString" => DateFn::Format(0),
                    "toDateString" => DateFn::Format(1),
                    "toTimeString" => DateFn::Format(2),
                    "toUTCString" => DateFn::Format(6),
                    other => unreachable!("sema admitted Date.{other}()"),
                };
                let mut operands = vec![self.expr(receiver)];
                operands.append(&mut lowered);
                Expr {
                    ty,
                    span,
                    kind: ExprKind::CallDate(f, operands),
                }
            }
            sema::Ty::Int | sema::Ty::UInt | sema::Ty::Number | sema::Ty::Boolean => {
                if name == "valueOf" {
                    return self.expr(receiver);
                }
                if recv_ty == sema::Ty::Boolean {
                    if name == "toString" {
                        let receiver = self.expr(receiver);
                        return Expr {
                            ty: Ty::String,
                            span,
                            kind: ExprKind::Conv(Conv::ToString, Box::new(receiver)),
                        };
                    }
                    return self.gated_expr(span, ty, "this Boolean method — Phase 7");
                }
                let method = match name {
                    "toString" => {
                        // radix defaults to 10 (Number.as:98); the parameter
                        // is untyped, so sema boxed it — unbox to Number.
                        if lowered.is_empty() {
                            lowered.push(num_lit(10.0, span));
                        }
                        NumMethod::ToString
                    }
                    "toFixed" => {
                        fill_num(&mut lowered, 0, 0.0);
                        NumMethod::ToFixed
                    }
                    _ => return self.gated_expr(span, ty, "this Number method — Phase 7"),
                };
                let lowered = lowered
                    .into_iter()
                    .map(|a| to_number_expr(a, span))
                    .collect();
                // Receiver widens to Number: the runtime implements one
                // numeric method set (int/uint print via the same path).
                let receiver = to_number_expr(self.expr(receiver), span);
                Expr {
                    ty,
                    span,
                    kind: ExprKind::CallNumMethod(method, Box::new(receiver), lowered),
                }
            }
            // Boxed receiver: property lookup + bound call.
            _ => Expr {
                ty,
                span,
                kind: ExprKind::PropCall(Box::new(self.expr(receiver)), name.to_string(), lowered),
            },
        }
    }
}

impl Lowerer<'_> {
    /// Allocates a desugaring temp in the current function.
    fn add_temp(&mut self, ty: Ty) -> LocalId {
        let id = LocalId((self.temp_base + self.temps.len()) as u32);
        self.temps.push(ty);
        id
    }
}

fn local_get(id: LocalId, ty: Ty, span: Span) -> Expr {
    Expr {
        ty,
        span,
        kind: ExprKind::LocalGet(id),
    }
}

/// Splits a sema-mangled namespaced member name `#ns{id}::raw` into
/// (namespace id, raw name); None for ordinary members.
fn split_ns_name(name: &str) -> Option<(u32, String)> {
    let rest = name.strip_prefix("#ns")?;
    let (id, raw) = rest.split_once("::")?;
    Some((id.parse().ok()?, raw.to_string()))
}

/// Unboxes an Any expression into `ty` (ES3 §9 conversions).
fn coerce_any_to(e: Expr, ty: Ty, span: Span) -> Expr {
    let _ = span;
    let conv = match ty {
        Ty::Any => return e,
        Ty::Int => Conv::ToInt,
        Ty::UInt => Conv::ToUInt,
        Ty::Number => Conv::ToNumber,
        Ty::Boolean => Conv::ToBoolean,
        Ty::String => Conv::AnyToString,
        Ty::Object(c) => Conv::AnyToObject(c),
        Ty::Iface(i) => Conv::AnyToIface(i),
        Ty::Array => Conv::AnyToArray,
        Ty::Vector(i) => Conv::AnyToVector(i),
        Ty::RegExp => Conv::AnyToRegExp,
        Ty::Date => Conv::AnyToDate,
        Ty::Socket => Conv::AnyToSocket,
        // `*` → Namespace has no checked coercion in v1 (rare; use a
        // typed variable from the start).
        Ty::Namespace | Ty::Function | Ty::Void => return e,
    };
    Expr {
        ty,
        span,
        kind: ExprKind::Conv(conv, Box::new(e)),
    }
}

fn this_expr(class: u32, span: Span) -> Expr {
    Expr {
        ty: Ty::Object(class),
        span,
        kind: ExprKind::This,
    }
}

/// Whether a checked constructor body contains an explicit `super(...)`
/// call (recursive TAST scan).
fn tast_has_super(stmts: &[sema::TStmt]) -> bool {
    fn in_expr(e: &sema::TExpr) -> bool {
        use sema::TExprKind as E;
        match &e.kind {
            E::SuperCtor(..) => true,
            E::LocalSet(_, v)
            | E::Coerce(_, v)
            | E::Unary(_, v)
            | E::Postfix(_, v)
            | E::Is(v, _)
            | E::As(v, _)
            | E::StaticSet(_, _, v) => in_expr(v),
            E::Binary(_, a, b) | E::Logical(_, a, b) | E::Comma(a, b) => in_expr(a) || in_expr(b),
            E::Conditional(a, b, c) => in_expr(a) || in_expr(b) || in_expr(c),
            E::CallFn(_, args) | E::CallBuiltin(_, args) | E::New(_, args) => {
                args.iter().any(in_expr)
            }
            E::CallMethod(r, _, args)
            | E::CallVirtual { recv: r, args, .. }
            | E::CallIface { recv: r, args, .. }
            | E::CallDirect { recv: r, args, .. } => in_expr(r) || args.iter().any(in_expr),
            E::FieldGet(o, ..) => in_expr(o),
            E::FieldSet(o, _, _, v) => in_expr(o) || in_expr(v),
            E::Member(o, _) => in_expr(o),
            E::MemberSet(o, _, v) => in_expr(o) || in_expr(v),
            E::Index(o, i) => in_expr(o) || in_expr(i),
            E::IndexSet(o, i, v) => in_expr(o) || in_expr(i) || in_expr(v),
            E::CallIndirect(c, args) => in_expr(c) || args.iter().any(in_expr),
            E::Array(els) => els.iter().flatten().any(in_expr),
            E::Object(props) => props.iter().any(|(_, v)| in_expr(v)),
            _ => false,
        }
    }
    fn in_stmt(s: &sema::TStmt) -> bool {
        use sema::TStmtKind as S;
        match &s.kind {
            S::Expr(e) | S::Assign(_, e) | S::Throw { value: e } => in_expr(e),
            S::Block(b) => b.iter().any(in_stmt),
            S::If {
                cond,
                then_branch,
                else_branch,
            } => {
                in_expr(cond)
                    || in_stmt(then_branch)
                    || else_branch.as_ref().is_some_and(|e| in_stmt(e))
            }
            S::While { cond, body } | S::DoWhile { body, cond } => in_expr(cond) || in_stmt(body),
            S::For {
                init,
                cond,
                update,
                body,
            } => {
                init.as_ref().is_some_and(|i| in_stmt(i))
                    || cond.as_ref().is_some_and(in_expr)
                    || update.as_ref().is_some_and(in_expr)
                    || in_stmt(body)
            }
            S::ForIn { object, body, .. } => in_expr(object) || in_stmt(body),
            S::Switch { scrutinee, cases } => {
                in_expr(scrutinee)
                    || cases
                        .iter()
                        .any(|c| c.test.as_ref().is_some_and(in_expr) || c.body.iter().any(in_stmt))
            }
            S::Return { value } => value.as_ref().is_some_and(in_expr),
            S::Try {
                block,
                catches,
                finally,
            } => {
                block.iter().any(in_stmt)
                    || catches.iter().any(|c| c.body.iter().any(in_stmt))
                    || finally.as_ref().is_some_and(|f| f.iter().any(in_stmt))
            }
            S::Labeled { body, .. } => in_stmt(body),
            S::Break { .. } | S::Continue { .. } | S::Empty => false,
        }
    }
    stmts.iter().any(in_stmt)
}

fn str_lit(s: &str, span: Span) -> Expr {
    Expr {
        ty: Ty::String,
        span,
        kind: ExprKind::Str(s.to_string()),
    }
}

fn int_lit(v: i32, span: Span) -> Expr {
    Expr {
        ty: Ty::Int,
        span,
        kind: ExprKind::Int(v),
    }
}

fn num_lit(v: f64, span: Span) -> Expr {
    Expr {
        ty: Ty::Number,
        span,
        kind: ExprKind::Number(v),
    }
}

/// Wraps an expression in ToString unless it already is a String.
fn to_string_expr(e: Expr, span: Span) -> Expr {
    if e.ty == Ty::String {
        return e;
    }
    Expr {
        ty: Ty::String,
        span,
        kind: ExprKind::Conv(Conv::ToString, Box::new(e)),
    }
}

/// Wraps an expression in ToNumber unless it already is a Number.
fn to_number_expr(e: Expr, span: Span) -> Expr {
    if e.ty == Ty::Number {
        return e;
    }
    Expr {
        ty: Ty::Number,
        span,
        kind: ExprKind::Conv(Conv::ToNumber, Box::new(e)),
    }
}
