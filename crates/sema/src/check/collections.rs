//! Array and `Vector.<T>` typing (SPECS §3.10, §4.3, §6 P5 surface) and
//! generic-class instantiation (SPECS §4.2).
//!
//! Generics are **monomorphized**: every distinct argument list creates a
//! fresh class in the registry with its own layout, method bodies, and RTTI
//! — which also delivers reification (`x is Box.<int>` is a real runtime
//! test distinct from `Box.<Number>`). SPECS §4.2 sketched value-type-only
//! monomorphization with uniform boxing for references; full
//! monomorphization is the simpler correct point in that space (documented
//! trade-off: code size).

use ast::ExprKind;
use diagnostics::ErrorCode;
use span::Span;

use super::Checker;
use crate::classes::ClassId;
use crate::tast::*;
use crate::ty::Ty;

impl<'a> Checker<'a> {
    // --- Array ------------------------------------------------------------

    /// Member read on an Array receiver (`length` is the only property).
    pub(crate) fn array_member_read(&mut self, object: TExpr, name: &str, span: Span) -> TExpr {
        if name == "length" {
            return TExpr {
                ty: Ty::UInt,
                span,
                kind: TExprKind::SeqLen(Box::new(object)),
            };
        }
        self.error(
            ErrorCode::UNKNOWN_PROPERTY,
            format!("no property `{name}` on Array"),
            span,
        );
        self.error_expr(span)
    }

    /// `arr.length = n` (§15.4.5.2: truncates or extends with holes).
    pub(crate) fn seq_member_write(
        &mut self,
        object: TExpr,
        name: &str,
        value: TExpr,
        span: Span,
    ) -> TExpr {
        if name == "length" {
            let value = self.coerce(value, Ty::UInt, span);
            return TExpr {
                ty: Ty::UInt,
                span,
                kind: TExprKind::SeqSetLen(Box::new(object), Box::new(value)),
            };
        }
        self.error(
            ErrorCode::UNKNOWN_PROPERTY,
            format!("no writable property `{name}` on this sealed type"),
            span,
        );
        self.error_expr(span)
    }

    /// Array method call (signatures per ES3 §15.4.4 / AS3 Array class).
    pub(crate) fn array_method_call(
        &mut self,
        object: TExpr,
        name: &str,
        args: &'a [ast::Expr],
        span: Span,
    ) -> TExpr {
        use ArrMethod::{Some as Some_, *};
        let (method, ret): (ArrMethod, Ty) = match name {
            "push" => (Push, Ty::UInt),
            "pop" => (Pop, Ty::Any),
            "shift" => (Shift, Ty::Any),
            "unshift" => (Unshift, Ty::UInt),
            "slice" => (Slice, Ty::Array),
            "splice" => (Splice, Ty::Array),
            "indexOf" => (IndexOf, Ty::Int),
            "concat" => (Concat, Ty::Array),
            "join" => (Join, Ty::String),
            "reverse" => (Reverse, Ty::Array),
            "sort" => (Sort, Ty::Array),
            "forEach" => (ForEach, Ty::Void),
            "map" => (Map, Ty::Array),
            "filter" => (Filter, Ty::Array),
            "some" => (Some_, Ty::Boolean),
            "every" => (Every, Ty::Boolean),
            _ => {
                self.error(
                    ErrorCode::UNKNOWN_PROPERTY,
                    format!("no method `{name}` on Array (or it lands in Phase 7)"),
                    span,
                );
                return self.error_expr(span);
            }
        };
        if method == Sort && args.len() > 1 {
            self.error(
                ErrorCode::WRONG_ARG_COUNT,
                "`sort` takes at most one comparator",
                span,
            );
            return self.error_expr(span);
        }
        // Arguments are `*` (Array is untyped) except the typed few.
        let args: Vec<TExpr> = args
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let checked = self.expr(a);
                match (method, i) {
                    (Join, 0) => self.coerce(checked, Ty::String, a.span),
                    (Sort | ForEach | Map | Filter | Some_ | Every, 0) => {
                        self.coerce(checked, Ty::Function, a.span)
                    }
                    (Slice | Splice, 0 | 1) | (IndexOf, 1) => {
                        self.coerce(checked, Ty::Number, a.span)
                    }
                    _ => self.coerce_to_any(checked),
                }
            })
            .collect();
        TExpr {
            ty: ret,
            span,
            kind: TExprKind::CallArr(method, Box::new(object), args),
        }
    }

    // --- Vector.<T> ---------------------------------------------------------

    /// Member read on a Vector receiver.
    pub(crate) fn vector_member_read(
        &mut self,
        object: TExpr,
        inst: u32,
        name: &str,
        span: Span,
    ) -> TExpr {
        if name == "length" {
            return TExpr {
                ty: Ty::UInt,
                span,
                kind: TExprKind::SeqLen(Box::new(object)),
            };
        }
        self.error(
            ErrorCode::UNKNOWN_PROPERTY,
            format!(
                "no property `{name}` on `{}`",
                self.ty_name(Ty::Vector(inst))
            ),
            span,
        );
        self.error_expr(span)
    }

    /// Vector method call: element positions typed as T.
    pub(crate) fn vector_method_call(
        &mut self,
        object: TExpr,
        inst: u32,
        name: &str,
        args: &'a [ast::Expr],
        span: Span,
    ) -> TExpr {
        use VecMethod::*;
        let elem = self.vector_elem(inst);
        let (method, ret): (VecMethod, Ty) = match name {
            "push" => (Push, Ty::UInt),
            "pop" => (Pop, elem),
            "shift" => (Shift, elem),
            "unshift" => (Unshift, Ty::UInt),
            "slice" => (Slice, Ty::Vector(inst)),
            "indexOf" => (IndexOf, Ty::Int),
            "join" => (Join, Ty::String),
            "reverse" => (Reverse, Ty::Vector(inst)),
            _ => {
                self.error(
                    ErrorCode::UNKNOWN_PROPERTY,
                    format!(
                        "no method `{name}` on `{}` (or it lands in Phase 7)",
                        self.ty_name(Ty::Vector(inst))
                    ),
                    span,
                );
                return self.error_expr(span);
            }
        };
        let args: Vec<TExpr> = args
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let checked = self.expr(a);
                match (method, i) {
                    (Push | Unshift, _) | (IndexOf, 0) => self.coerce(checked, elem, a.span),
                    (Slice, 0 | 1) => self.coerce(checked, Ty::Number, a.span),
                    (IndexOf, 1) => self.coerce(checked, Ty::Number, a.span),
                    (Join, 0) => self.coerce(checked, Ty::String, a.span),
                    _ => self.coerce_to_any(checked),
                }
            })
            .collect();
        TExpr {
            ty: ret,
            span,
            kind: TExprKind::CallVec(method, Box::new(object), args),
        }
    }

    /// Typed index read: `arr[i]` → `*`, `vec[i]` → T.
    pub(crate) fn seq_index_read(&mut self, object: TExpr, index: TExpr, span: Span) -> TExpr {
        let elem = match object.ty {
            Ty::Array => Ty::Any,
            Ty::Vector(inst) => self.vector_elem(inst),
            _ => unreachable!("sequence receiver"),
        };
        let index = self.coerce(index, Ty::Number, span);
        TExpr {
            ty: elem,
            span,
            kind: TExprKind::Index(Box::new(object), Box::new(index)),
        }
    }

    /// Typed index write.
    pub(crate) fn seq_index_write(
        &mut self,
        object: TExpr,
        index: TExpr,
        value: TExpr,
        span: Span,
    ) -> TExpr {
        let elem = match object.ty {
            Ty::Array => Ty::Any,
            Ty::Vector(inst) => self.vector_elem(inst),
            _ => unreachable!("sequence receiver"),
        };
        let index = self.coerce(index, Ty::Number, span);
        let value = self.coerce(value, elem, span);
        TExpr {
            ty: elem,
            span,
            kind: TExprKind::IndexSet(Box::new(object), Box::new(index), Box::new(value)),
        }
    }

    /// `new <T>[...]` literal (SPECS §4.3).
    pub(crate) fn vector_literal(
        &mut self,
        elem: &'a ast::TypeRef,
        elements: &'a [ast::Expr],
        span: Span,
    ) -> TExpr {
        let elem_ty = self.resolve_type(elem);
        let Ty::Vector(inst) = self.vector_of(elem_ty) else {
            unreachable!()
        };
        let elements = elements
            .iter()
            .map(|e| {
                let checked = self.expr(e);
                self.coerce(checked, elem_ty, e.span)
            })
            .collect();
        TExpr {
            ty: Ty::Vector(inst),
            span,
            kind: TExprKind::VectorLit(inst, elements),
        }
    }

    // --- generic instantiation (SPECS §4.2) ---------------------------------

    /// Instantiates `template.<args>` into a concrete registry class,
    /// memoized. Bodies are re-checked under the substitution, producing
    /// monomorphic functions.
    pub(crate) fn instantiate_template(
        &mut self,
        tid: usize,
        args: Vec<Ty>,
        span: Span,
    ) -> ClassId {
        if let Some(id) = self.instantiations.get(&(tid, args.clone())) {
            return *id;
        }
        let (name, decl) = {
            let (n, d) = &self.templates[tid];
            (n.clone(), *d)
        };
        let decl: &'a ast::ClassDecl = decl;
        if args.len() != decl.type_params.len() {
            self.error(
                ErrorCode::WRONG_ARG_COUNT,
                format!(
                    "`{name}` takes {} type argument(s), got {}",
                    decl.type_params.len(),
                    args.len()
                ),
                span,
            );
            return crate::classes::OBJECT;
        }
        let display = format!(
            "{name}.<{}>",
            args.iter()
                .map(|&a| self.ty_name(a))
                .collect::<Vec<_>>()
                .join(",")
        );
        let id = ClassId(u32::try_from(self.registry.classes.len()).unwrap());
        self.registry.classes.push(crate::classes::ClassInfo {
            name: display,
            package: Vec::new(),
            parent: None,
            interfaces: Vec::new(),
            is_final: decl.attrs.is_final,
            is_dynamic: decl.attrs.is_dynamic,
            fields: Vec::new(),
            total_slots: 0,
            vtable: Vec::new(),
            ctor: None,
            ctor_sig: Default::default(),
            static_fields: Vec::new(),
            static_methods: Vec::new(),
            span: decl.span,
        });
        // Memoize BEFORE building so self-referential fields resolve.
        self.instantiations.insert((tid, args.clone()), id);

        let map: std::collections::HashMap<String, Ty> = decl
            .type_params
            .iter()
            .cloned()
            .zip(args.iter().copied())
            .collect();
        self.subst.push(map);

        // Parent / interfaces (templates get the single-class slice of
        // build_hierarchy, under substitution).
        let parent = match &decl.extends {
            None => Some(crate::classes::OBJECT),
            Some(t) => match self.resolve_type_allow_void(t) {
                Ty::Class(p) => Some(p),
                _ => {
                    self.error(ErrorCode::NOT_A_TYPE, "`extends` must name a class", t.span);
                    Some(crate::classes::OBJECT)
                }
            },
        };
        self.registry.classes[id.0 as usize].parent = parent;
        let mut set = Vec::new();
        for t in &decl.implements {
            match self.resolve_type_allow_void(t) {
                Ty::Iface(i) => {
                    let mut stack = vec![i];
                    while let Some(cur) = stack.pop() {
                        if !set.contains(&cur) {
                            set.push(cur);
                            stack.extend(&self.registry.ifaces[cur.0 as usize].extends);
                        }
                    }
                }
                _ => self.error(
                    ErrorCode::NOT_A_TYPE,
                    "`implements` must name interfaces",
                    t.span,
                ),
            }
        }
        self.registry.classes[id.0 as usize].interfaces = set;

        self.build_class_members(id, decl);
        self.check_conformance(id, decl);
        // Bodies may be checked while another function is mid-check —
        // save the pieces check_class_bodies clobbers.
        let saved_super = std::mem::take(&mut self.ctor_saw_super);
        self.check_class_bodies(id, decl);
        self.ctor_saw_super = saved_super;
        self.subst.pop();
        id
    }

    /// Resolves an `ApplyType` expression (`Vector.<int>`, `Box.<T>`) used
    /// in type position (`new`, `is`/`as`).
    pub(crate) fn apply_type_to_ty(&mut self, e: &'a ast::Expr) -> Option<Ty> {
        let ExprKind::ApplyType(base, targs) = &e.kind else {
            return None;
        };
        let ExprKind::Ident(name) = &base.kind else {
            return None;
        };
        let args: Vec<Ty> = targs.iter().map(|t| self.resolve_type(t)).collect();
        if name == "Vector" {
            if args.len() != 1 {
                self.error(
                    ErrorCode::WRONG_ARG_COUNT,
                    "Vector takes exactly one type argument",
                    e.span,
                );
                return Some(Ty::Error);
            }
            return Some(self.vector_of(args[0]));
        }
        if let Some(tid) = self.template_index(name) {
            return Some(Ty::Class(self.instantiate_template(tid, args, e.span)));
        }
        None
    }

    pub(crate) fn template_index(&self, name: &str) -> Option<usize> {
        self.templates.iter().position(|(n, _)| n == name)
    }

    /// Instantiates a generic function (SPECS §4.2) for the given type
    /// arguments — monomorphized like generic classes.
    pub(crate) fn instantiate_fn_template(
        &mut self,
        tid: usize,
        args: Vec<Ty>,
        span: Span,
    ) -> Option<crate::tast::FnId> {
        if let Some(id) = self.fn_instantiations.get(&(tid, args.clone())) {
            return Some(*id);
        }
        let (name, decl) = {
            let (n, d) = &self.fn_templates[tid];
            (n.clone(), *d)
        };
        let decl: &'a ast::FunctionDecl = decl;
        if args.len() != decl.type_params.len() {
            self.error(
                ErrorCode::WRONG_ARG_COUNT,
                format!(
                    "`{name}` takes {} type argument(s), got {}",
                    decl.type_params.len(),
                    args.len()
                ),
                span,
            );
            return None;
        }
        let display = format!(
            "{name}.<{}>",
            args.iter()
                .map(|&a| self.ty_name(a))
                .collect::<Vec<_>>()
                .join(",")
        );
        let map: std::collections::HashMap<String, Ty> = decl
            .type_params
            .iter()
            .cloned()
            .zip(args.iter().copied())
            .collect();
        self.subst.push(map);
        let ret = decl
            .return_type
            .as_ref()
            .map(|t| self.resolve_type_allow_void(t))
            .unwrap_or(Ty::Any);
        let id = self.new_function(&display, ret, decl.span);
        self.fn_instantiations.insert((tid, args), id);
        self.set_ret_nullable(id, decl.return_type.as_ref().is_some_and(|t| t.nullable));
        self.register_signature(id, &decl.params);
        self.enter_function(decl, id);
        self.subst.pop();
        Some(id)
    }

    pub(crate) fn fn_template_index(&self, name: &str) -> Option<usize> {
        self.fn_templates.iter().position(|(n, _)| n == name)
    }
}
