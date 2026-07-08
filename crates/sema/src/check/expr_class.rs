//! Class-aware expression typing: `this`/`super`, member access with
//! access control, virtual/interface/static dispatch, `new`, and explicit
//! `Type(expr)` casts.

use ast::{ExprKind, Visibility};
use diagnostics::ErrorCode;
use span::Span;

use super::{Checker, MethodCtx, TypeSym};
use crate::classes::{ClassId, IfaceId, OBJECT, Sig, VKind};
use crate::tast::*;
use crate::ty::Ty;

/// Resolution of a member name on a class.
pub(crate) enum ClassMember {
    Field {
        defined_in: ClassId,
        slot: usize,
        ty: Ty,
        is_const: bool,
    },
    Accessor {
        getter: Option<(usize, Ty)>,
        setter: Option<(usize, Ty)>,
    },
    Method {
        vslot: usize,
        sig: Sig,
    },
}

impl<'a> Checker<'a> {
    pub(crate) fn current_method(&self) -> Option<MethodCtx> {
        self.method_ctx.last().copied().flatten()
    }

    fn current_package(&self) -> Vec<String> {
        self.current_method()
            .map(|m| self.registry.classes[m.class.0 as usize].package.clone())
            .unwrap_or_default()
    }

    /// Access-control check (SPECS §3.4/§3.6).
    fn check_visible(
        &mut self,
        name: &str,
        visibility: Visibility,
        defined_in: ClassId,
        span: Span,
    ) {
        let ok = match visibility {
            Visibility::Public => true,
            Visibility::Private => self.current_method().is_some_and(|m| m.class == defined_in),
            Visibility::Protected => self
                .current_method()
                .is_some_and(|m| self.registry.is_subclass(m.class, defined_in)),
            Visibility::Internal => {
                self.current_package() == self.registry.classes[defined_in.0 as usize].package
            }
        };
        if !ok {
            self.error(
                ErrorCode::UNKNOWN_PROPERTY,
                format!(
                    "`{name}` is {} to `{}`",
                    match visibility {
                        Visibility::Private => "private",
                        Visibility::Protected => "protected",
                        Visibility::Internal => "internal",
                        Visibility::Public => unreachable!(),
                    },
                    self.registry.classes[defined_in.0 as usize].name
                ),
                span,
            );
        }
    }

    /// Finds an instance member (field / paired accessors / method).
    /// Resolves a raw member name against `use namespace` state: an
    /// exact (or already-qualified) match wins; otherwise the open
    /// namespaces are searched, and multiple candidates are an ambiguity
    /// error (ES4 draft multiname lookup, statically resolved).
    pub(crate) fn effective_member_name(
        &mut self,
        class: ClassId,
        raw: &str,
        span: Span,
    ) -> String {
        if raw.starts_with("#ns") || self.member_exists(class, raw) {
            return raw.to_string();
        }
        let open: Vec<u32> = self
            .scopes
            .iter()
            .flat_map(|s| s.open_ns.iter().copied())
            .collect();
        let mut hits: Vec<String> = Vec::new();
        for id in open {
            let mangled = format!("#ns{id}::{raw}");
            if self.member_exists(class, &mangled) && !hits.contains(&mangled) {
                hits.push(mangled);
            }
        }
        match hits.len() {
            0 => raw.to_string(),
            1 => hits.pop().expect("hit"),
            _ => {
                self.error(
                    ErrorCode::UNRESOLVED_NAME,
                    format!(
                        "`{raw}` is ambiguous: it exists in {} open namespaces — qualify it (`ns::{raw}`)",
                        hits.len()
                    ),
                    span,
                );
                raw.to_string()
            }
        }
    }

    fn member_exists(&self, class: ClassId, name: &str) -> bool {
        self.registry.find_field(class, name).is_some()
            || [VKind::Getter, VKind::Setter, VKind::Method]
                .iter()
                .any(|&k| self.registry.find_vmethod(class, name, k).is_some())
    }

    /// Mangles `ns::name` at a use site (None + error if `ns` unknown).
    pub(crate) fn qualify(&mut self, ns: &str, name: &str, span: Span) -> Option<String> {
        match self.namespaces.get(ns).copied() {
            Some(id) => Some(format!("#ns{id}::{name}")),
            None => {
                self.error(
                    ErrorCode::UNRESOLVED_NAME,
                    format!("unknown namespace `{ns}`"),
                    span,
                );
                None
            }
        }
    }

    pub(crate) fn resolve_member(&self, class: ClassId, name: &str) -> Option<ClassMember> {
        if let Some(f) = self.registry.find_field(class, name) {
            return Some(ClassMember::Field {
                defined_in: f.defined_in,
                slot: f.slot,
                ty: f.ty,
                is_const: f.is_const,
            });
        }
        let getter = self
            .registry
            .find_vmethod(class, name, VKind::Getter)
            .map(|(i, v)| (i, v.sig.ret));
        let setter = self
            .registry
            .find_vmethod(class, name, VKind::Setter)
            .map(|(i, v)| (i, v.sig.params.first().copied().unwrap_or(Ty::Any)));
        if getter.is_some() || setter.is_some() {
            return Some(ClassMember::Accessor { getter, setter });
        }
        self.registry
            .find_vmethod(class, name, VKind::Method)
            .map(|(i, v)| ClassMember::Method {
                vslot: i,
                sig: v.sig.clone(),
            })
    }

    fn member_visibility(&self, class: ClassId, name: &str) -> Option<(Visibility, ClassId)> {
        if let Some(f) = self.registry.find_field(class, name) {
            return Some((f.visibility, f.defined_in));
        }
        for kind in [VKind::Getter, VKind::Setter, VKind::Method] {
            if let Some((_, v)) = self.registry.find_vmethod(class, name, kind) {
                return Some((v.visibility, v.introduced_in));
            }
        }
        None
    }

    /// `this` (SPECS §3.4); only meaningful inside instance members.
    pub(crate) fn this_expr(&mut self, span: Span) -> TExpr {
        match self.current_method() {
            Some(m) if !m.is_static => TExpr {
                ty: Ty::Class(m.class),
                span,
                kind: TExprKind::This,
            },
            Some(_) => {
                self.error(
                    ErrorCode::UNRESOLVED_NAME,
                    "`this` is not available in a static member",
                    span,
                );
                self.error_expr(span)
            }
            None => {
                self.error(
                    ErrorCode::UNRESOLVED_NAME,
                    "`this` is only available inside class members (closures capture it in Phase 6)",
                    span,
                );
                self.error_expr(span)
            }
        }
    }

    /// Instance member read: field load, getter call, or method extraction.
    pub(crate) fn class_member_read(
        &mut self,
        object: TExpr,
        class: ClassId,
        name: &str,
        span: Span,
    ) -> TExpr {
        let name = &self.effective_member_name(class, name, span);
        if let Some((vis, def)) = self.member_visibility(class, name) {
            self.check_visible(name, vis, def, span);
        }
        match self.resolve_member(class, name) {
            Some(ClassMember::Field {
                defined_in,
                slot,
                ty,
                ..
            }) => TExpr {
                ty,
                span,
                kind: TExprKind::FieldGet(Box::new(object), defined_in, slot),
            },
            Some(ClassMember::Accessor { getter, setter }) => match getter {
                Some((vslot, ty)) => TExpr {
                    ty,
                    span,
                    kind: TExprKind::CallVirtual {
                        recv: Box::new(object),
                        class,
                        vslot,
                        args: Vec::new(),
                    },
                },
                None => {
                    let _ = setter;
                    self.error(
                        ErrorCode::UNKNOWN_PROPERTY,
                        format!("`{name}` is write-only (no getter)"),
                        span,
                    );
                    self.error_expr(span)
                }
            },
            Some(ClassMember::Method { vslot, .. }) => {
                // Extracting a method yields a closure permanently bound to
                // the receiver (SPECS §3.7 — no `this`-loss footgun).
                TExpr {
                    ty: Ty::Function,
                    span,
                    kind: TExprKind::BoundMethod(Box::new(object), class, vslot),
                }
            }
            None => {
                // Dynamic classes accept unknown members as expandos
                // (SPECS §3.2) — typed `*`, resolved at runtime.
                if self.registry.classes[class.0 as usize].is_dynamic {
                    let boxed = self.coerce_to_any(object);
                    return TExpr {
                        ty: Ty::Any,
                        span,
                        kind: TExprKind::Member(Box::new(boxed), name.to_string()),
                    };
                }
                self.error(
                    ErrorCode::UNKNOWN_PROPERTY,
                    format!(
                        "no property `{name}` on sealed class `{}` (SPECS §3.2)",
                        self.registry.classes[class.0 as usize].name
                    ),
                    span,
                );
                self.error_expr(span)
            }
        }
    }

    /// Instance member write: field store or setter call.
    pub(crate) fn class_member_write(
        &mut self,
        object: TExpr,
        class: ClassId,
        name: &str,
        value: TExpr,
        span: Span,
    ) -> TExpr {
        let name = &self.effective_member_name(class, name, span);
        if let Some((vis, def)) = self.member_visibility(class, name) {
            self.check_visible(name, vis, def, span);
        }
        match self.resolve_member(class, name) {
            Some(ClassMember::Field {
                defined_in,
                slot,
                ty,
                is_const,
            }) => {
                {
                    let nullable = self
                        .registry
                        .field_by_slot(defined_in, slot)
                        .is_some_and(|f| f.nullable);
                    // Evaluate flow before the const check reports.
                    let _ = nullable;
                }
                if is_const {
                    // const fields assign only inside the defining class's
                    // constructor (AS3 allows constructor initialization).
                    let in_own_ctor = self
                        .current_method()
                        .is_some_and(|m| m.is_ctor && m.class == defined_in);
                    if !in_own_ctor {
                        self.error(
                            ErrorCode::ASSIGN_TO_CONST,
                            format!("const field `{name}` can only be set in the constructor"),
                            span,
                        );
                    }
                }
                let value = self.coerce(value, ty, span);
                let nullable = self
                    .registry
                    .field_by_slot(defined_in, slot)
                    .is_some_and(|f| f.nullable);
                self.check_null_flow(&value, ty, nullable, "field", span);
                TExpr {
                    ty,
                    span,
                    kind: TExprKind::FieldSet(Box::new(object), defined_in, slot, Box::new(value)),
                }
            }
            Some(ClassMember::Accessor { setter, .. }) => match setter {
                Some((vslot, param_ty)) => {
                    let value = self.coerce(value, param_ty, span);
                    let nullable = self.registry.classes[class.0 as usize].vtable[vslot]
                        .sig
                        .params_nullable
                        .first()
                        .copied()
                        .unwrap_or(false);
                    self.check_null_flow(&value, param_ty, nullable, "property", span);
                    TExpr {
                        ty: param_ty,
                        span,
                        kind: TExprKind::CallVirtual {
                            recv: Box::new(object),
                            class,
                            vslot,
                            args: vec![value],
                        },
                    }
                }
                None => {
                    self.error(
                        ErrorCode::UNKNOWN_PROPERTY,
                        format!("`{name}` is read-only (no setter)"),
                        span,
                    );
                    self.error_expr(span)
                }
            },
            Some(ClassMember::Method { .. }) => {
                self.error(
                    ErrorCode::UNKNOWN_PROPERTY,
                    format!("cannot assign to method `{name}`"),
                    span,
                );
                self.error_expr(span)
            }
            None => {
                if self.registry.classes[class.0 as usize].is_dynamic {
                    let boxed = self.coerce_to_any(object);
                    let value = self.coerce_to_any(value);
                    let ty = value.ty;
                    return TExpr {
                        ty,
                        span,
                        kind: TExprKind::MemberSet(
                            Box::new(boxed),
                            name.to_string(),
                            Box::new(value),
                        ),
                    };
                }
                self.error(
                    ErrorCode::UNKNOWN_PROPERTY,
                    format!(
                        "no property `{name}` on sealed class `{}`",
                        self.registry.classes[class.0 as usize].name
                    ),
                    span,
                );
                self.error_expr(span)
            }
        }
    }

    /// Interface member read (getter) — methods extract in P6.
    pub(crate) fn iface_member_read(
        &mut self,
        object: TExpr,
        iface: IfaceId,
        name: &str,
        span: Span,
    ) -> TExpr {
        if let Some((islot, m)) = self.registry.find_iface_method(iface, name, VKind::Getter) {
            let ty = m.sig.ret;
            return TExpr {
                ty,
                span,
                kind: TExprKind::CallIface {
                    recv: Box::new(object),
                    iface,
                    islot,
                    args: Vec::new(),
                },
            };
        }
        if self
            .registry
            .find_iface_method(iface, name, VKind::Method)
            .is_some()
        {
            self.error(
                ErrorCode::NOT_IMPLEMENTED,
                "method closures (extracting a method as a value) — Phase 6",
                span,
            );
            return self.error_expr(span);
        }
        self.error(
            ErrorCode::UNKNOWN_PROPERTY,
            format!(
                "no property `{name}` on interface `{}`",
                self.registry.ifaces[iface.0 as usize].name
            ),
            span,
        );
        self.error_expr(span)
    }

    /// Interface member write (setter).
    pub(crate) fn iface_member_write(
        &mut self,
        object: TExpr,
        iface: IfaceId,
        name: &str,
        value: TExpr,
        span: Span,
    ) -> TExpr {
        if let Some((islot, m)) = self.registry.find_iface_method(iface, name, VKind::Setter) {
            let ty = m.sig.params.first().copied().unwrap_or(Ty::Any);
            let value = self.coerce(value, ty, span);
            return TExpr {
                ty,
                span,
                kind: TExprKind::CallIface {
                    recv: Box::new(object),
                    iface,
                    islot,
                    args: vec![value],
                },
            };
        }
        self.error(
            ErrorCode::UNKNOWN_PROPERTY,
            format!(
                "no writable property `{name}` on interface `{}`",
                self.registry.ifaces[iface.0 as usize].name
            ),
            span,
        );
        self.error_expr(span)
    }

    /// Method call on a class-typed receiver.
    pub(crate) fn class_method_call(
        &mut self,
        object: TExpr,
        class: ClassId,
        name: &str,
        args: &'a [ast::Expr],
        span: Span,
    ) -> TExpr {
        let name = &self.effective_member_name(class, name, span);
        if let Some((vis, def)) = self.member_visibility(class, name) {
            self.check_visible(name, vis, def, span);
        }
        match self.resolve_member(class, name) {
            Some(ClassMember::Method { vslot, sig }) => {
                let args = self.check_sig_args(&sig, name, args, span);
                TExpr {
                    ty: sig.ret,
                    span,
                    kind: TExprKind::CallVirtual {
                        recv: Box::new(object),
                        class,
                        vslot,
                        args,
                    },
                }
            }
            Some(ClassMember::Field { ty, .. }) if ty == Ty::Function || ty == Ty::Any => {
                self.error(
                    ErrorCode::NOT_IMPLEMENTED,
                    "calling function-valued fields — Phase 6",
                    span,
                );
                self.error_expr(span)
            }
            Some(_) => {
                self.error(
                    ErrorCode::NOT_CALLABLE,
                    format!("`{name}` is not a method"),
                    span,
                );
                self.error_expr(span)
            }
            None => {
                self.error(
                    ErrorCode::UNKNOWN_PROPERTY,
                    format!(
                        "no method `{name}` on sealed class `{}`",
                        self.registry.classes[class.0 as usize].name
                    ),
                    span,
                );
                self.error_expr(span)
            }
        }
    }

    /// Method call on an interface-typed receiver.
    pub(crate) fn iface_method_call(
        &mut self,
        object: TExpr,
        iface: IfaceId,
        name: &str,
        args: &'a [ast::Expr],
        span: Span,
    ) -> TExpr {
        let Some((islot, m)) = self.registry.find_iface_method(iface, name, VKind::Method) else {
            self.error(
                ErrorCode::UNKNOWN_PROPERTY,
                format!(
                    "no method `{name}` on interface `{}`",
                    self.registry.ifaces[iface.0 as usize].name
                ),
                span,
            );
            return self.error_expr(span);
        };
        let sig = m.sig.clone();
        let args = self.check_sig_args(&sig, name, args, span);
        TExpr {
            ty: sig.ret,
            span,
            kind: TExprKind::CallIface {
                recv: Box::new(object),
                iface,
                islot,
                args,
            },
        }
    }

    /// Checks arguments against a class/interface signature.
    pub(crate) fn check_sig_args(
        &mut self,
        sig: &Sig,
        name: &str,
        args: &'a [ast::Expr],
        span: Span,
    ) -> Vec<TExpr> {
        self.arity(
            name,
            args.len(),
            sig.required,
            sig.params.len(),
            sig.variadic,
            span,
        );
        args.iter()
            .enumerate()
            .map(|(i, a)| {
                let checked = self.expr(a);
                match sig.params.get(i) {
                    Some(&ty) => {
                        let coerced = self.coerce(checked, ty, a.span);
                        let nullable = sig.params_nullable.get(i).copied().unwrap_or(false);
                        self.check_null_flow(&coerced, ty, nullable, "parameter", a.span);
                        coerced
                    }
                    None => self.coerce_to_any(checked),
                }
            })
            .collect()
    }

    /// `new C(args)` (SPECS §3.4); also `new Vector.<T>()`, `new Array(...)`
    /// and `new Box.<T>(args)`.
    pub(crate) fn new_expr(
        &mut self,
        callee: &'a ast::Expr,
        args: &'a [ast::Expr],
        span: Span,
    ) -> TExpr {
        // Parameterized targets.
        if matches!(callee.kind, ExprKind::ApplyType(..)) {
            match self.apply_type_to_ty(callee) {
                Some(Ty::Vector(inst)) => {
                    if !args.is_empty() {
                        self.error(
                            ErrorCode::NOT_IMPLEMENTED,
                            "sized Vector constructors — Phase 7 (use a literal)",
                            span,
                        );
                    }
                    return TExpr {
                        ty: Ty::Vector(inst),
                        span,
                        kind: TExprKind::VectorLit(inst, Vec::new()),
                    };
                }
                Some(Ty::Class(class)) => {
                    let sig = self.registry.classes[class.0 as usize].ctor_sig.clone();
                    let name = self.registry.classes[class.0 as usize].name.clone();
                    let args = self.check_sig_args(&sig, &name, args, span);
                    return TExpr {
                        ty: Ty::Class(class),
                        span,
                        kind: TExprKind::New(class, args),
                    };
                }
                _ => {
                    self.error(ErrorCode::NOT_A_TYPE, "cannot construct this type", span);
                    return self.error_expr(span);
                }
            }
        }
        // `new RegExp(pattern, flags)` (§15.10.4; both args Strings, the
        // second optional). Bad patterns throw SyntaxError at runtime.
        if let ExprKind::Ident(name) = &callee.kind
            && name == "RegExp"
            && !self.is_shadowed(name)
        {
            if args.is_empty() || args.len() > 2 {
                self.error(
                    ErrorCode::WRONG_ARG_COUNT,
                    "RegExp(pattern:String, flags:String = \"\") takes 1-2 arguments",
                    span,
                );
                for a in args {
                    self.expr(a);
                }
                return self.error_expr(span);
            }
            let checked: Vec<TExpr> = args
                .iter()
                .map(|a| {
                    let e = self.expr(a);
                    let sp = e.span;
                    self.coerce(e, Ty::String, sp)
                })
                .collect();
            return TExpr {
                ty: Ty::RegExp,
                span,
                kind: TExprKind::NewRegExp(checked),
            };
        }
        // `new Namespace(uri)` (ES4 first-class namespaces, SPECS §5).
        if let ExprKind::Ident(name) = &callee.kind
            && name == "Namespace"
            && !self.is_shadowed(name)
        {
            if args.len() != 1 {
                self.error(
                    ErrorCode::WRONG_ARG_COUNT,
                    "Namespace(uri:String) takes exactly 1 argument",
                    span,
                );
                for a in args {
                    self.expr(a);
                }
                return self.error_expr(span);
            }
            let uri = self.expr(&args[0]);
            let sp = uri.span;
            let uri = self.coerce(uri, Ty::String, sp);
            return TExpr {
                ty: Ty::Namespace,
                span,
                kind: TExprKind::NewNamespace(Box::new(uri)),
            };
        }
        // `new Date(...)` (§15.9.3): 0-7 Number components. String
        // parsing (`new Date("...")`) is backlog.
        if let ExprKind::Ident(name) = &callee.kind
            && name == "Date"
            && !self.is_shadowed(name)
        {
            if args.len() > 7 {
                self.error(
                        ErrorCode::WRONG_ARG_COUNT,
                        "Date takes at most 7 arguments (year, month, date, hours, minutes, seconds, ms)",
                        span,
                    );
            }
            let checked: Vec<TExpr> = args
                .iter()
                .take(7)
                .map(|a| {
                    let e = self.expr(a);
                    let sp = e.span;
                    if e.ty == Ty::String {
                        self.error(
                            ErrorCode::NOT_IMPLEMENTED,
                            "Date string parsing — use new Date(millis) or components (backlog)",
                            sp,
                        );
                        return self.error_expr(sp);
                    }
                    self.coerce(e, Ty::Number, sp)
                })
                .collect();
            return TExpr {
                ty: Ty::Date,
                span,
                kind: TExprKind::NewDate(checked),
            };
        }
        // `new Array(...)` — literal-equivalent.
        if let ExprKind::Ident(name) = &callee.kind
            && name == "Array"
            && !self.is_shadowed(name)
        {
            let elements = args
                .iter()
                .map(|a| {
                    let checked = self.expr(a);
                    Some(self.coerce_to_any(checked))
                })
                .collect();
            return TExpr {
                ty: Ty::Array,
                span,
                kind: TExprKind::Array(elements),
            };
        }
        let Some(class) = self.type_expr_to_class(callee) else {
            self.error(
                ErrorCode::NOT_A_TYPE,
                "`new` needs a class (interfaces and non-types are not constructible)",
                callee.span,
            );
            for a in args {
                self.expr(a);
            }
            return self.error_expr(span);
        };
        let sig = self.registry.classes[class.0 as usize].ctor_sig.clone();
        let name = self.registry.classes[class.0 as usize].name.clone();
        let args = self.check_sig_args(&sig, &name, args, span);
        TExpr {
            ty: Ty::Class(class),
            span,
            kind: TExprKind::New(class, args),
        }
    }

    /// Resolves an expression used as a class reference (`new X`, casts).
    pub(crate) fn type_expr_to_class(&mut self, e: &'a ast::Expr) -> Option<ClassId> {
        let path = flatten_path(e)?;
        match self.type_names.get(&path).copied() {
            Some(TypeSym::Class(id)) => Some(id),
            _ if path == "Object" => Some(OBJECT),
            _ => None,
        }
    }

    /// Static member read `C.name`.
    pub(crate) fn static_read(&mut self, class: ClassId, name: &str, span: Span) -> TExpr {
        if let Some(f) = self.registry.find_static_field(class, name) {
            let (ty, index, vis) = (f.ty, f.index, f.visibility);
            self.check_visible(name, vis, class, span);
            return TExpr {
                ty,
                span,
                kind: TExprKind::StaticGet(class, index),
            };
        }
        if let Some(m) = self.registry.find_static_method(class, name) {
            // Static methods carry no `this`: a plain function value.
            let fn_id = m.fn_id;
            return TExpr {
                ty: Ty::Function,
                span,
                kind: TExprKind::FnRef(fn_id),
            };
        }
        self.error(
            ErrorCode::UNKNOWN_PROPERTY,
            format!(
                "no static `{name}` on class `{}`",
                self.registry.classes[class.0 as usize].name
            ),
            span,
        );
        self.error_expr(span)
    }

    /// Static member write `C.name = v`.
    pub(crate) fn static_write(
        &mut self,
        class: ClassId,
        name: &str,
        value: TExpr,
        span: Span,
    ) -> TExpr {
        if let Some(f) = self.registry.find_static_field(class, name) {
            let (ty, index, is_const, vis) = (f.ty, f.index, f.is_const, f.visibility);
            self.check_visible(name, vis, class, span);
            if is_const {
                self.error(
                    ErrorCode::ASSIGN_TO_CONST,
                    format!("cannot assign to static const `{name}`"),
                    span,
                );
            }
            let value = self.coerce(value, ty, span);
            let nullable = self.registry.classes[class.0 as usize].static_fields[index].nullable;
            self.check_null_flow(&value, ty, nullable, "static field", span);
            return TExpr {
                ty,
                span,
                kind: TExprKind::StaticSet(class, index, Box::new(value)),
            };
        }
        self.error(
            ErrorCode::UNKNOWN_PROPERTY,
            format!(
                "no static `{name}` on class `{}`",
                self.registry.classes[class.0 as usize].name
            ),
            span,
        );
        self.error_expr(span)
    }

    /// Static method call `C.m(args)`.
    pub(crate) fn static_call(
        &mut self,
        class: ClassId,
        name: &str,
        args: &'a [ast::Expr],
        span: Span,
    ) -> TExpr {
        let Some(m) = self.registry.find_static_method(class, name) else {
            // Fall back to a static-field-as-function? Phase 6. Report.
            self.error(
                ErrorCode::UNKNOWN_PROPERTY,
                format!(
                    "no static method `{name}` on class `{}`",
                    self.registry.classes[class.0 as usize].name
                ),
                span,
            );
            return self.error_expr(span);
        };
        let (sig, fn_id, vis) = (m.sig.clone(), m.fn_id, m.visibility);
        self.check_visible(name, vis, class, span);
        let args = self.check_sig_args(&sig, name, args, span);
        TExpr {
            ty: sig.ret,
            span,
            kind: TExprKind::CallFn(fn_id, args),
        }
    }

    /// `super(args)` / `super.m(args)`.
    pub(crate) fn super_call(
        &mut self,
        method: Option<&str>,
        args: &'a [ast::Expr],
        span: Span,
    ) -> TExpr {
        let Some(ctx) = self.current_method() else {
            self.error(
                ErrorCode::UNRESOLVED_NAME,
                "`super` is only available inside class members",
                span,
            );
            return self.error_expr(span);
        };
        let parent = self.registry.classes[ctx.class.0 as usize]
            .parent
            .unwrap_or(OBJECT);
        match method {
            None => {
                if !ctx.is_ctor {
                    self.error(
                        ErrorCode::UNEXPECTED_TOKEN,
                        "`super(...)` is only valid inside a constructor",
                        span,
                    );
                    return self.error_expr(span);
                }
                self.ctor_saw_super = true;
                let sig = self.registry.classes[parent.0 as usize].ctor_sig.clone();
                let args = self.check_sig_args(&sig, "super", args, span);
                TExpr {
                    ty: Ty::Void,
                    span,
                    kind: TExprKind::SuperCtor(parent, args),
                }
            }
            Some(name) => {
                // Statically bound to the parent's current implementation
                // (avm2overview.pdf callsupervoid semantics).
                let Some((_, v)) = self.registry.find_vmethod(parent, name, VKind::Method) else {
                    self.error(
                        ErrorCode::UNKNOWN_PROPERTY,
                        format!("no method `{name}` on the superclass"),
                        span,
                    );
                    return self.error_expr(span);
                };
                let (sig, fn_id) = (v.sig.clone(), v.fn_id);
                let args = self.check_sig_args(&sig, name, args, span);
                let this = self.this_expr(span);
                TExpr {
                    ty: sig.ret,
                    span,
                    kind: TExprKind::CallDirect {
                        fn_id,
                        recv: Box::new(this),
                        args,
                    },
                }
            }
        }
    }

    /// Explicit cast `Type(expr)` (ES4 draft: calling a type converts).
    /// Core types use the ES3 §9 conversions; classes use checked coercion
    /// (throws on mismatch, unlike `as` which yields null).
    pub(crate) fn cast_call(&mut self, target: Ty, args: &'a [ast::Expr], span: Span) -> TExpr {
        if args.len() != 1 {
            self.error(
                ErrorCode::WRONG_ARG_COUNT,
                "a type cast takes exactly one argument",
                span,
            );
        }
        let Some(arg) = args.first() else {
            return self.error_expr(span);
        };
        let checked = self.expr(arg);
        match target {
            Ty::Int | Ty::UInt | Ty::Number | Ty::Boolean | Ty::String => {
                let coercion = match target {
                    Ty::Int => Coercion::ToInt,
                    Ty::UInt => Coercion::ToUInt,
                    Ty::Number => Coercion::ToNumber,
                    Ty::Boolean => Coercion::ToBoolean,
                    _ => Coercion::ToString,
                };
                if checked.ty == target {
                    return checked;
                }
                TExpr {
                    ty: target,
                    span,
                    kind: TExprKind::Coerce(coercion, Box::new(checked)),
                }
            }
            Ty::Class(_) | Ty::Iface(_) => {
                // Upcasts are free; anything else is a runtime-checked
                // coercion through `*`.
                if self.conversion(checked.ty, target) == Some(None) {
                    return checked;
                }
                let boxed = self.coerce_to_any(checked);
                TExpr {
                    ty: target,
                    span,
                    kind: TExprKind::Coerce(Coercion::FromAny, Box::new(boxed)),
                }
            }
            _ => {
                self.error(ErrorCode::NOT_A_TYPE, "cannot cast to this type", span);
                self.error_expr(span)
            }
        }
    }

    /// Resolution for a bare identifier that is an implicit `this` member
    /// or a static of the enclosing class.
    pub(crate) fn implicit_member(&mut self, name: &str, span: Span) -> Option<TExpr> {
        let ctx = self.current_method()?;
        // Statics of the enclosing class are visible unqualified.
        if self.registry.find_static_field(ctx.class, name).is_some() {
            return Some(self.static_read(ctx.class, name, span));
        }
        if !ctx.is_static && self.resolve_member(ctx.class, name).is_some() {
            let this = self.this_expr(span);
            return Some(self.class_member_read(this, ctx.class, name, span));
        }
        None
    }

    /// Implicit-member assignment target.
    pub(crate) fn implicit_member_write(
        &mut self,
        name: &str,
        value: TExpr,
        span: Span,
    ) -> Option<TExpr> {
        let ctx = self.current_method()?;
        if self.registry.find_static_field(ctx.class, name).is_some() {
            return Some(self.static_write(ctx.class, name, value, span));
        }
        if !ctx.is_static && self.resolve_member(ctx.class, name).is_some() {
            let this = self.this_expr(span);
            return Some(self.class_member_write(this, ctx.class, name, value, span));
        }
        None
    }

    /// Implicit-member / unqualified static / same-class method call.
    pub(crate) fn implicit_method_call(
        &mut self,
        name: &str,
        args: &'a [ast::Expr],
        span: Span,
    ) -> Option<TExpr> {
        let ctx = self.current_method()?;
        if self.registry.find_static_method(ctx.class, name).is_some() {
            return Some(self.static_call(ctx.class, name, args, span));
        }
        if !ctx.is_static {
            if let Some(ClassMember::Method { .. }) = self.resolve_member(ctx.class, name) {
                let this = self.this_expr(span);
                return Some(self.class_method_call(this, ctx.class, name, args, span));
            }
            if let Some(ClassMember::Accessor { .. }) = self.resolve_member(ctx.class, name) {
                // Calling an accessor value — Phase 6 function values.
                self.error(
                    ErrorCode::NOT_IMPLEMENTED,
                    "calling accessor values — Phase 6",
                    span,
                );
                return Some(self.error_expr(span));
            }
        }
        None
    }

    /// Native static member read (Math.PI etc.).
    pub(crate) fn native_static_read(
        &mut self,
        class: &str,
        name: &str,
        span: Span,
    ) -> Option<TExpr> {
        if !crate::builtins::is_native_class(class) {
            return None;
        }
        if let Some(consts) = crate::builtins::native_consts(class)
            && let Some(c) = consts.iter().find(|c| c.name == name)
        {
            return Some(TExpr {
                ty: Ty::Number,
                span,
                kind: TExprKind::Number(c.value),
            });
        }
        if let Some(methods) = crate::builtins::native_methods(class)
            && methods.iter().any(|m| m.name == name)
        {
            self.error(
                ErrorCode::NOT_IMPLEMENTED,
                "native static methods as values — Phase 8",
                span,
            );
            return Some(self.error_expr(span));
        }
        self.error(
            ErrorCode::UNKNOWN_PROPERTY,
            format!("no static `{name}` on `{class}`"),
            span,
        );
        Some(self.error_expr(span))
    }

    /// Native static call (Math.sqrt(x), System.exit(0), ...).
    pub(crate) fn native_static_call(
        &mut self,
        class: &str,
        name: &str,
        args: &'a [ast::Expr],
        span: Span,
    ) -> Option<TExpr> {
        let methods = crate::builtins::native_methods(class)?;
        let Some(m) = methods.iter().find(|m| m.name == name) else {
            self.error(
                ErrorCode::UNKNOWN_PROPERTY,
                format!("no static method `{name}` on `{class}`"),
                span,
            );
            return Some(self.error_expr(span));
        };
        self.arity(
            name,
            args.len(),
            m.sig.required,
            m.sig.params.len(),
            m.sig.variadic,
            span,
        );
        let checked: Vec<TExpr> = args
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let c = self.expr(a);
                // Variadic tails share the last declared type (Math.min).
                let ty = m
                    .sig
                    .params
                    .get(i)
                    .or(m.sig.params.last())
                    .copied()
                    .unwrap_or(Ty::Any);
                self.coerce(c, ty, a.span)
            })
            .collect();
        // Native String returns may be null (File.read, System.getenv).
        Some(TExpr {
            ty: m.sig.ret,
            span,
            kind: TExprKind::CallNative(m.func, checked),
        })
    }

    /// Resolves a bare identifier as a class reference (static receiver).
    pub(crate) fn ident_as_class(&self, name: &str) -> Option<ClassId> {
        match self.type_names.get(name).copied() {
            Some(TypeSym::Class(id)) => Some(id),
            _ if name == "Object" => Some(OBJECT),
            _ => None,
        }
    }

    /// Resolves a bare identifier as any type (for `is`/`as`/casts).
    pub(crate) fn ident_as_type(&self, name: &str) -> Option<Ty> {
        if let Some(ty) = crate::builtins::type_name(name) {
            return Some(ty);
        }
        if name == "Array" {
            return Some(Ty::Array);
        }
        match self.type_names.get(name).copied() {
            Some(TypeSym::Class(id)) => Some(Ty::Class(id)),
            Some(TypeSym::Iface(id)) => Some(Ty::Iface(id)),
            None if name == "Object" => Some(Ty::Class(OBJECT)),
            None => None,
        }
    }
}

/// Flattens `a.b.C` member chains into a dotted path (for type lookup).
fn flatten_path(e: &ast::Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Ident(name) => Some(name.clone()),
        ExprKind::Member(obj, name) => Some(format!("{}.{name}", flatten_path(obj)?)),
        _ => None,
    }
}

impl<'a> Checker<'a> {
    /// Whether a bare name is shadowed by a local/function symbol (locals
    /// shadow type names and members).
    pub(crate) fn is_shadowed(&self, name: &str) -> bool {
        self.scopes
            .iter()
            .rev()
            .any(|scope| scope.symbols.contains_key(name))
    }
}
