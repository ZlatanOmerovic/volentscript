//! Class/interface declaration processing: collection, hierarchy
//! resolution, layout + vtable construction, override enforcement
//! (SPECS §3.4 — `override` is mandatory), and interface conformance
//! (SPECS §3.5).

use ast::{MemberKind, SigKind, Visibility};
use diagnostics::ErrorCode;
use span::Span;

use super::{Checker, MethodCtx};
use crate::classes::*;
use crate::tast::{FnId, TExpr};
use crate::ty::Ty;

/// What a top-level type name refers to.
#[derive(Debug, Clone, Copy)]
pub(crate) enum TypeSym {
    Class(ClassId),
    Iface(IfaceId),
}

impl Checker {
    /// Pass A: register every class/interface name in the file (recursing
    /// into packages) so forward references resolve.
    pub(crate) fn collect_types<'a>(
        &mut self,
        stmts: &'a [ast::Stmt],
        package: &[String],
        classes: &mut Vec<(ClassId, &'a ast::ClassDecl)>,
        ifaces: &mut Vec<(IfaceId, &'a ast::InterfaceDecl)>,
    ) {
        for stmt in stmts {
            match &stmt.kind {
                ast::StmtKind::Package { path, body } => {
                    if !package.is_empty() {
                        self.error(
                            ErrorCode::UNEXPECTED_TOKEN,
                            "packages cannot nest",
                            stmt.span,
                        );
                    }
                    self.collect_types(body, path, classes, ifaces);
                }
                ast::StmtKind::Class(decl) => {
                    let id = ClassId(u32::try_from(self.registry.classes.len()).unwrap());
                    self.registry.classes.push(ClassInfo {
                        name: decl.name.clone(),
                        package: package.to_vec(),
                        parent: None, // resolved in pass B
                        interfaces: Vec::new(),
                        is_final: decl.attrs.is_final,
                        is_dynamic: decl.attrs.is_dynamic,
                        fields: Vec::new(),
                        total_slots: 0,
                        vtable: Vec::new(),
                        ctor: None,
                        ctor_sig: Sig::default(),
                        static_fields: Vec::new(),
                        static_methods: Vec::new(),
                        span: decl.span,
                    });
                    self.register_type_name(&decl.name, package, TypeSym::Class(id), decl.span);
                    classes.push((id, decl));
                }
                ast::StmtKind::Interface(decl) => {
                    let id = IfaceId(u32::try_from(self.registry.ifaces.len()).unwrap());
                    self.registry.ifaces.push(IfaceInfo {
                        name: decl.name.clone(),
                        package: package.to_vec(),
                        extends: Vec::new(),
                        methods: Vec::new(),
                        span: decl.span,
                    });
                    self.register_type_name(&decl.name, package, TypeSym::Iface(id), decl.span);
                    ifaces.push((id, decl));
                }
                _ => {}
            }
        }
    }

    fn register_type_name(&mut self, name: &str, package: &[String], sym: TypeSym, span: Span) {
        if self.type_names.contains_key(name) {
            self.error(
                ErrorCode::CONFLICTING_DECL,
                format!("type `{name}` is already declared"),
                span,
            );
            return;
        }
        self.type_names.insert(name.to_string(), sym);
        if !package.is_empty() {
            let qualified = format!("{}.{name}", package.join("."));
            self.type_names.insert(qualified, sym);
        }
    }

    /// Pass B: resolve `extends`/`implements`, reject cycles, then build
    /// interface method lists and class layouts in dependency order.
    pub(crate) fn build_hierarchy(
        &mut self,
        classes: &[(ClassId, &ast::ClassDecl)],
        ifaces: &[(IfaceId, &ast::InterfaceDecl)],
    ) {
        // Interfaces: resolve extends, then flatten (DFS; cycle -> error).
        for (id, decl) in ifaces {
            let extends = decl
                .extends
                .iter()
                .filter_map(|t| match self.resolve_type_sym(t) {
                    Some(TypeSym::Iface(i)) => Some(i),
                    _ => {
                        self.error(
                            ErrorCode::NOT_A_TYPE,
                            "an interface can only extend interfaces",
                            t.span,
                        );
                        None
                    }
                })
                .collect();
            self.registry.ifaces[id.0 as usize].extends = extends;
        }
        for (id, decl) in ifaces {
            if self.iface_has_cycle(*id, &mut vec![]) {
                self.error(
                    ErrorCode::CONFLICTING_DECL,
                    format!("interface `{}` extends itself", decl.name),
                    decl.span,
                );
                self.registry.ifaces[id.0 as usize].extends = Vec::new();
            }
        }
        for (id, decl) in ifaces {
            let methods = self.build_iface_methods(*id, decl);
            self.registry.ifaces[id.0 as usize].methods = methods;
        }

        // Classes: resolve parents.
        for (id, decl) in classes {
            let parent = match &decl.extends {
                None => Some(OBJECT),
                Some(t) => match self.resolve_type_sym(t) {
                    Some(TypeSym::Class(p)) => {
                        if self.registry.classes[p.0 as usize].is_final {
                            self.error(
                                ErrorCode::CONFLICTING_DECL,
                                format!(
                                    "cannot extend final class `{}`",
                                    self.registry.classes[p.0 as usize].name
                                ),
                                t.span,
                            );
                        }
                        Some(p)
                    }
                    _ => {
                        self.error(ErrorCode::NOT_A_TYPE, "`extends` must name a class", t.span);
                        Some(OBJECT)
                    }
                },
            };
            self.registry.classes[id.0 as usize].parent = parent;
        }
        // Cycle check (a cycle means the chain never reaches Object).
        for (id, decl) in classes {
            let mut seen = vec![*id];
            let mut cur = self.registry.classes[id.0 as usize].parent;
            while let Some(p) = cur {
                if seen.contains(&p) {
                    self.error(
                        ErrorCode::CONFLICTING_DECL,
                        format!("class `{}` inherits from itself", decl.name),
                        decl.span,
                    );
                    self.registry.classes[id.0 as usize].parent = Some(OBJECT);
                    break;
                }
                seen.push(p);
                cur = self.registry.classes[p.0 as usize].parent;
            }
        }

        // Interfaces implemented: direct + their extends-closure.
        for (id, decl) in classes {
            let mut set: Vec<IfaceId> = Vec::new();
            for t in &decl.implements {
                match self.resolve_type_sym(t) {
                    Some(TypeSym::Iface(i)) => {
                        let mut stack = vec![i];
                        while let Some(cur) = stack.pop() {
                            if !set.contains(&cur) {
                                set.push(cur);
                                stack.extend(&self.registry.ifaces[cur.0 as usize].extends);
                            }
                        }
                    }
                    _ => {
                        self.error(
                            ErrorCode::NOT_A_TYPE,
                            "`implements` must name interfaces",
                            t.span,
                        );
                    }
                }
            }
            self.registry.classes[id.0 as usize].interfaces = set;
        }

        // Layout + members, parents before children.
        let order = self.topo_order(classes);
        for idx in order {
            let (id, decl) = classes[idx];
            self.build_class_members(id, decl);
        }
        for (id, decl) in classes {
            self.check_conformance(*id, decl);
        }
    }

    fn iface_has_cycle(&self, id: IfaceId, path: &mut Vec<IfaceId>) -> bool {
        if path.contains(&id) {
            return true;
        }
        path.push(id);
        let extends = self.registry.ifaces[id.0 as usize].extends.clone();
        let cycle = extends.iter().any(|&e| self.iface_has_cycle(e, path));
        path.pop();
        cycle
    }

    fn build_iface_methods(&mut self, id: IfaceId, decl: &ast::InterfaceDecl) -> Vec<IfaceMethod> {
        // Inherited first (dispatch-table order), then own.
        let mut methods: Vec<IfaceMethod> = Vec::new();
        for &e in self.registry.ifaces[id.0 as usize].extends.clone().iter() {
            for m in &self.registry.ifaces[e.0 as usize].methods {
                if !methods.iter().any(|x| x.name == m.name && x.kind == m.kind) {
                    methods.push(m.clone());
                }
            }
        }
        for m in &decl.members {
            let kind = match m.kind {
                SigKind::Method => VKind::Method,
                SigKind::Getter => VKind::Getter,
                SigKind::Setter => VKind::Setter,
            };
            let sig = self.build_sig(&m.params, m.return_type.as_ref(), kind);
            if let Some(existing) = methods.iter().find(|x| x.name == m.name && x.kind == kind) {
                if !existing.sig.matches(&sig) {
                    self.error(
                        ErrorCode::CONFLICTING_DECL,
                        format!("`{}` conflicts with an inherited signature", m.name),
                        m.span,
                    );
                }
                continue;
            }
            methods.push(IfaceMethod {
                name: m.name.clone(),
                kind,
                sig,
            });
        }
        methods
    }

    /// Builds a checked signature from parameter syntax (types only; bodies
    /// are handled when the method is checked).
    fn build_sig(&mut self, params: &[ast::Param], ret: Option<&ast::TypeRef>, kind: VKind) -> Sig {
        let mut tys = Vec::new();
        let mut required = params.len();
        let mut variadic = false;
        for (i, p) in params.iter().enumerate() {
            if p.rest {
                required = required.min(i);
                variadic = true;
                continue;
            }
            if p.default.is_some() {
                required = required.min(i);
            }
            tys.push(
                p.ty.as_ref()
                    .map(|t| self.resolve_type(t))
                    .unwrap_or(Ty::Any),
            );
        }
        let ret = match kind {
            VKind::Setter => Ty::Void,
            _ => ret
                .map(|t| self.resolve_type_allow_void(t))
                .unwrap_or(Ty::Any),
        };
        Sig {
            params: tys,
            required,
            variadic,
            ret,
        }
    }

    fn topo_order(&self, classes: &[(ClassId, &ast::ClassDecl)]) -> Vec<usize> {
        // Parents before children; cycles were already broken.
        let mut order: Vec<usize> = (0..classes.len()).collect();
        order.sort_by_key(|&i| {
            let mut depth = 0usize;
            let mut cur = self.registry.classes[classes[i].0.0 as usize].parent;
            while let Some(p) = cur {
                depth += 1;
                cur = self.registry.classes[p.0 as usize].parent;
            }
            depth
        });
        order
    }

    /// Pass B2 (per class, parents first): slots, vtable, statics,
    /// constructor signature. Bodies are checked later (pass C).
    fn build_class_members(&mut self, id: ClassId, decl: &ast::ClassDecl) {
        let parent = self.registry.classes[id.0 as usize]
            .parent
            .unwrap_or(OBJECT);
        let mut vtable = self.registry.classes[parent.0 as usize].vtable.clone();
        let mut slot = self.registry.classes[parent.0 as usize].total_slots;
        let mut fields = Vec::new();
        let mut static_fields = Vec::new();
        let mut static_methods: Vec<StaticMethod> = Vec::new();
        let mut ctor: Option<FnId> = None;
        let mut ctor_sig = Sig::default();

        for member in &decl.members {
            let visibility = member.attrs.visibility.unwrap_or(Visibility::Internal);
            match &member.kind {
                MemberKind::Field(var) => {
                    for b in &var.bindings {
                        let ty =
                            b.ty.as_ref()
                                .map(|t| self.resolve_type(t))
                                .unwrap_or(Ty::Any);
                        let duplicate = fields.iter().any(|f: &FieldInfo| f.name == b.name)
                            || static_fields.iter().any(|f: &StaticField| f.name == b.name)
                            || self.registry.find_field(parent, &b.name).is_some();
                        if duplicate {
                            self.error(
                                ErrorCode::CONFLICTING_DECL,
                                format!("`{}` is already declared in this class", b.name),
                                b.span,
                            );
                            continue;
                        }
                        if member.attrs.is_static {
                            static_fields.push(StaticField {
                                name: b.name.clone(),
                                ty,
                                is_const: var.is_const,
                                visibility,
                                init: None, // checked in pass C
                                index: static_fields.len(),
                            });
                        } else {
                            fields.push(FieldInfo {
                                name: b.name.clone(),
                                ty,
                                is_const: var.is_const,
                                visibility,
                                init: None,
                                slot,
                                defined_in: id,
                            });
                            slot += 1;
                        }
                    }
                }
                MemberKind::Method(f) | MemberKind::Getter(f) | MemberKind::Setter(f) => {
                    let kind = match &member.kind {
                        MemberKind::Method(_) => VKind::Method,
                        MemberKind::Getter(_) => VKind::Getter,
                        _ => VKind::Setter,
                    };
                    let name = f.name.clone().unwrap_or_default();
                    let sig = self.build_sig(&f.params, f.return_type.as_ref(), kind);
                    // Constructor: method named like the class (SPECS §3.4).
                    if kind == VKind::Method && name == decl.name && !member.attrs.is_static {
                        let fn_id = self.new_function_for_method(&name, Ty::Void, id, f.span);
                        ctor = Some(fn_id);
                        ctor_sig = Sig {
                            ret: Ty::Void,
                            ..sig
                        };
                        continue;
                    }
                    if member.attrs.is_static {
                        let fn_id = self.new_function_for_static(&name, sig.ret, f.span);
                        static_methods.push(StaticMethod {
                            name,
                            sig,
                            fn_id,
                            visibility,
                        });
                        continue;
                    }
                    let fn_id = self.new_function_for_method(&name, sig.ret, id, f.span);
                    // Override handling (SPECS §3.4: `override` mandatory).
                    if let Some(pos) = vtable.iter().position(|m| m.name == name && m.kind == kind)
                    {
                        let existing = &vtable[pos];
                        if !member.attrs.is_override {
                            self.error(
                                ErrorCode::CONFLICTING_DECL,
                                format!(
                                    "`{name}` overrides an inherited member and must be marked `override`"
                                ),
                                member.span,
                            );
                        }
                        if existing.is_final {
                            self.error(
                                ErrorCode::CONFLICTING_DECL,
                                format!("`{name}` cannot override a final member"),
                                member.span,
                            );
                        }
                        if !existing.sig.matches(&sig) {
                            self.error(
                                ErrorCode::INCOMPATIBLE_TYPES,
                                format!(
                                    "override of `{name}` must match the inherited signature exactly"
                                ),
                                member.span,
                            );
                        }
                        let introduced_in = existing.introduced_in;
                        vtable[pos] = VMethod {
                            name,
                            kind,
                            sig,
                            fn_id,
                            is_final: member.attrs.is_final,
                            visibility: existing.visibility,
                            introduced_in,
                        };
                    } else {
                        if member.attrs.is_override {
                            self.error(
                                ErrorCode::CONFLICTING_DECL,
                                format!("`{name}` is marked `override` but overrides nothing"),
                                member.span,
                            );
                        }
                        vtable.push(VMethod {
                            name,
                            kind,
                            sig,
                            fn_id,
                            is_final: member.attrs.is_final,
                            visibility,
                            introduced_in: id,
                        });
                    }
                }
            }
        }

        let info = &mut self.registry.classes[id.0 as usize];
        info.fields = fields;
        info.total_slots = slot;
        info.vtable = vtable;
        info.ctor = ctor;
        info.ctor_sig = ctor_sig;
        info.static_fields = static_fields;
        info.static_methods = static_methods;
    }

    /// Interface conformance (SPECS §3.5): every flattened interface method
    /// must exist publicly with an exactly matching signature.
    fn check_conformance(&mut self, id: ClassId, decl: &ast::ClassDecl) {
        for &iface in self.registry.classes[id.0 as usize]
            .interfaces
            .clone()
            .iter()
        {
            for m in self.registry.ifaces[iface.0 as usize].methods.clone() {
                match self.registry.find_vmethod(id, &m.name, m.kind) {
                    Some((_, v)) if v.sig.matches(&m.sig) => {
                        if v.visibility != Visibility::Public {
                            self.error(
                                ErrorCode::CONFLICTING_DECL,
                                format!(
                                    "`{}` implements an interface method and must be public",
                                    m.name
                                ),
                                decl.span,
                            );
                        }
                    }
                    Some(_) => {
                        self.error(
                            ErrorCode::INCOMPATIBLE_TYPES,
                            format!(
                                "`{}` does not match the signature required by interface `{}`",
                                m.name, self.registry.ifaces[iface.0 as usize].name
                            ),
                            decl.span,
                        );
                    }
                    None => {
                        self.error(
                            ErrorCode::CONFLICTING_DECL,
                            format!(
                                "class `{}` is missing `{}` required by interface `{}`",
                                decl.name, m.name, self.registry.ifaces[iface.0 as usize].name
                            ),
                            decl.span,
                        );
                    }
                }
            }
        }
    }

    /// Pass C: check field/static initializers and method bodies.
    pub(crate) fn check_class_bodies(&mut self, id: ClassId, decl: &ast::ClassDecl) {
        let class_name = decl.name.clone();
        for member in &decl.members {
            match &member.kind {
                MemberKind::Field(var) => {
                    for b in &var.bindings {
                        let Some(init) = &b.init else {
                            if var.is_const && !member.attrs.is_static {
                                // const instance fields may instead be
                                // assigned in the constructor (checked at
                                // the assignment site).
                            } else if var.is_const {
                                self.error(
                                    ErrorCode::ASSIGN_TO_CONST,
                                    format!("static const `{}` needs an initializer", b.name),
                                    b.span,
                                );
                            }
                            continue;
                        };
                        let checked = self.check_initializer(id, member.attrs.is_static, init);
                        if member.attrs.is_static {
                            if let Some(f) = self.registry.classes[id.0 as usize]
                                .static_fields
                                .iter()
                                .position(|f| f.name == b.name)
                            {
                                self.registry.classes[id.0 as usize].static_fields[f].init =
                                    Some(checked);
                            }
                        } else if let Some(f) = self.registry.classes[id.0 as usize]
                            .fields
                            .iter()
                            .position(|f| f.name == b.name)
                        {
                            self.registry.classes[id.0 as usize].fields[f].init = Some(checked);
                        }
                    }
                }
                MemberKind::Method(f) | MemberKind::Getter(f) | MemberKind::Setter(f) => {
                    let kind = match &member.kind {
                        MemberKind::Method(_) => VKind::Method,
                        MemberKind::Getter(_) => VKind::Getter,
                        _ => VKind::Setter,
                    };
                    let name = f.name.clone().unwrap_or_default();
                    let is_ctor =
                        kind == VKind::Method && name == class_name && !member.attrs.is_static;
                    let fn_id = if is_ctor {
                        self.registry.classes[id.0 as usize].ctor
                    } else if member.attrs.is_static {
                        self.registry.find_static_method(id, &name).map(|m| m.fn_id)
                    } else {
                        // The vtable entry for a member declared in this
                        // class holds this class's body (parents were built
                        // first; overrides replaced in place).
                        self.registry
                            .find_vmethod(id, &name, kind)
                            .map(|(_, v)| v.fn_id)
                    };
                    let Some(fn_id) = fn_id else { continue };
                    self.check_method_body(
                        fn_id,
                        f,
                        MethodCtx {
                            class: id,
                            is_static: member.attrs.is_static,
                            is_ctor,
                        },
                    );
                }
            }
        }
        // Constructor super-chain rule: if the parent constructor requires
        // arguments, an explicit super(...) call is mandatory.
        let parent = self.registry.classes[id.0 as usize]
            .parent
            .unwrap_or(OBJECT);
        let parent_required = self.registry.classes[parent.0 as usize].ctor_sig.required;
        if parent_required > 0 {
            let has_ctor_with_super =
                self.registry.classes[id.0 as usize].ctor.is_some() && self.ctor_saw_super;
            if !has_ctor_with_super {
                self.error(
                    ErrorCode::WRONG_ARG_COUNT,
                    format!(
                        "`{class_name}` must call super(...) — the base constructor requires arguments"
                    ),
                    decl.span,
                );
            }
        }
        self.ctor_saw_super = false;
    }

    fn check_initializer(&mut self, class: ClassId, is_static: bool, init: &ast::Expr) -> TExpr {
        self.method_ctx.push(Some(MethodCtx {
            class,
            is_static,
            is_ctor: false,
        }));
        let checked = self.expr(init);
        self.method_ctx.pop();
        checked
    }

    /// Registers a function slot for an instance method body.
    fn new_function_for_method(&mut self, name: &str, ret: Ty, class: ClassId, span: Span) -> FnId {
        let id = self.new_function(name, ret, span);
        self.functions[id.0 as usize].method_of = Some(class);
        id
    }

    fn new_function_for_static(&mut self, name: &str, ret: Ty, span: Span) -> FnId {
        self.new_function(name, ret, span)
    }

    /// Checks one method/accessor/constructor body in class context.
    fn check_method_body(&mut self, fn_id: FnId, f: &ast::FunctionDecl, ctx: MethodCtx) {
        self.register_signature(fn_id, &f.params);
        self.scopes.push(Default::default());
        self.fn_stack.push(fn_id.0 as usize);
        self.method_ctx.push(Some(ctx));
        let saved_jumps = std::mem::take(&mut self.jumps);
        let saved_labels = std::mem::take(&mut self.labels);
        let body = self.check_function_body(fn_id, &f.params, &f.body.stmts, f.span);
        self.functions[fn_id.0 as usize].body = body;
        self.labels = saved_labels;
        self.jumps = saved_jumps;
        self.method_ctx.pop();
        self.fn_stack.pop();
        self.scopes.pop();
    }

    /// Resolves a type reference to a class/interface symbol, if it is one.
    pub(crate) fn resolve_type_sym(&mut self, t: &ast::TypeRef) -> Option<TypeSym> {
        if let ast::TypeRefKind::Name { path, type_args } = &t.kind {
            if !type_args.is_empty() {
                return None;
            }
            return self.type_names.get(&path.join(".")).copied();
        }
        None
    }
}
