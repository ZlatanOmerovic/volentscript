//! The type checker: resolution, typing, coercion insertion.
//!
//! Typing judgments follow the AVM2 verifier's static result types
//! (`docs/avmplus/core/Verifier.cpp`, cited per rule) on the ECMA-262 3rd
//! ed. conversion semantics (§9). Where the strict-mode compiler table is
//! not present in `docs/` (see P2 report), the conservative choice is taken
//! and marked `DOC GAP`.

mod collections;
mod decl;
mod expr;
mod expr_class;
mod null;

use std::collections::HashMap;

use diagnostics::{Diagnostic, ErrorCode, Severity};
use span::Span;

pub(crate) use decl::TypeSym;

use crate::builtins::{self, BuiltinFn};
use crate::classes::{ClassId, Registry};
use crate::tast::*;
use crate::ty::Ty;

/// Class context of the function currently being checked.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MethodCtx {
    pub(crate) class: ClassId,
    pub(crate) is_static: bool,
    pub(crate) is_ctor: bool,
}

/// Result of checking: a typed program when no errors occurred, plus all
/// diagnostics (warnings survive success).
#[derive(Debug)]
pub struct CheckOutcome {
    /// Typed program; `None` when errors were reported.
    pub program: Option<TProgram>,
    /// Errors and warnings, in source order.
    pub diagnostics: Vec<Diagnostic>,
}

/// Type-checks a parsed program.
pub fn check(program: &ast::Program) -> CheckOutcome {
    let mut checker = Checker::new();
    checker.check_script(program);
    checker
        .diagnostics
        .sort_by_key(|d| d.span.map(|s| (s.start, s.end)));
    let failed = checker
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error);
    CheckOutcome {
        program: (!failed).then_some(TProgram {
            functions: checker.functions,
            registry: checker.registry,
            vectors: checker.vector_insts,
        }),
        diagnostics: checker.diagnostics,
    }
}

/// What a name resolves to.
#[derive(Debug, Clone, Copy)]
enum Symbol {
    /// Local slot in function `fn_depth` (index into the scope stack's
    /// function list, 0 = script).
    Local { id: LocalId, fn_depth: usize },
    /// A declared function.
    Fn(FnId),
    /// A builtin global function.
    Builtin(BuiltinFn),
    /// A builtin global constant (NaN, Infinity, undefined).
    Const(Ty),
}

/// A function's externally visible signature (checked before bodies so
/// mutual recursion resolves).
#[derive(Debug, Clone)]
struct FnSig {
    params: Vec<ParamSig>,
    required: usize,
    variadic: bool,
    ret: Ty,
}

#[derive(Debug, Clone)]
struct ParamSig {
    ty: Ty,
}

#[derive(Debug, Default)]
struct Scope {
    symbols: HashMap<String, Symbol>,
}

/// Loop/switch nesting entry for break/continue validation.
#[derive(Debug)]
struct JumpCtx {
    label: Option<String>,
    is_loop: bool,
}

#[derive(Default)]
pub(crate) struct Checker<'a> {
    pub(crate) diagnostics: Vec<Diagnostic>,
    pub(crate) functions: Vec<TFunction>,
    signatures: Vec<FnSig>,
    scopes: Vec<Scope>,
    /// Function nesting: index into `functions` per active function frame.
    fn_stack: Vec<usize>,
    jumps: Vec<JumpCtx>,
    labels: Vec<String>,
    /// Class/interface registry (SPECS §3.4/§3.5).
    pub(crate) registry: Registry,
    /// Simple and qualified type names → registry ids.
    pub(crate) type_names: HashMap<String, TypeSym>,
    /// Class context per function frame (`None` = plain function).
    pub(crate) method_ctx: Vec<Option<MethodCtx>>,
    /// Set when the constructor being checked contains a `super(...)` call.
    pub(crate) ctor_saw_super: bool,
    /// Generic class templates (SPECS §4.2): name → declaration.
    pub(crate) templates: Vec<(String, &'a ast::ClassDecl)>,
    /// Memoized monomorphic instantiations: (template, args) → class.
    pub(crate) instantiations: HashMap<(usize, Vec<Ty>), ClassId>,
    /// Active type-parameter substitutions (innermost last).
    pub(crate) subst: Vec<HashMap<String, Ty>>,
    /// `Vector.<T>` instantiations: index = the `Ty::Vector` payload.
    pub(crate) vector_insts: Vec<Ty>,
    /// Locals proven non-null in the current flow (SPECS §4.1 narrowing);
    /// one set per active narrowing scope.
    pub(crate) narrowed: Vec<std::collections::HashSet<LocalId>>,
    /// `T?` return flags parallel to `functions`.
    pub(crate) fn_ret_nullable_flags: Vec<bool>,
}

impl<'a> Checker<'a> {
    fn new() -> Checker<'a> {
        Checker::default()
    }

    /// Interns a `Vector.<elem>` instantiation (SPECS §4.3 — reified:
    /// distinct element types are distinct runtime types).
    pub(crate) fn vector_of(&mut self, elem: Ty) -> Ty {
        if let Some(i) = self.vector_insts.iter().position(|&e| e == elem) {
            return Ty::Vector(i as u32);
        }
        self.vector_insts.push(elem);
        Ty::Vector((self.vector_insts.len() - 1) as u32)
    }

    /// Element type of an interned vector.
    pub(crate) fn vector_elem(&self, inst: u32) -> Ty {
        self.vector_insts[inst as usize]
    }
    fn check_script(&mut self, program: &'a ast::Program) {
        self.registry = Registry::with_object_root();
        // The script body is function 0 with no params (SPECS §7 entry).
        let script = self.new_function("<script>", Ty::Void, program.span);
        debug_assert_eq!(script, SCRIPT_FN);
        self.scopes.push(Scope::default());
        self.fn_stack.push(0);
        self.method_ctx.push(None);

        // Pass A/B: types first so the script body can reference classes.
        let mut classes = Vec::new();
        let mut ifaces = Vec::new();
        self.collect_types(&program.directives, &[], &mut classes, &mut ifaces);
        self.build_hierarchy(&classes, &ifaces);
        // Pass C: the script body (hoists script-level functions/vars into
        // the global scope so method bodies can see them).
        let body = self.check_function_body(script, &[], &program.directives, program.span);
        self.functions[0].body = body;
        // Pass D: field/static initializers and method bodies.
        for (id, decl) in &classes {
            self.check_class_bodies(*id, decl);
        }
        self.method_ctx.pop();
        self.fn_stack.pop();
        self.scopes.pop();
    }

    /// Human-readable type name for diagnostics.
    pub(crate) fn ty_name(&self, ty: Ty) -> String {
        match ty {
            Ty::Class(id) => self.registry.classes[id.0 as usize].qualified(),
            Ty::Iface(id) => self.registry.ifaces[id.0 as usize].name.clone(),
            Ty::Vector(inst) => format!("Vector.<{}>", self.ty_name(self.vector_elem(inst))),
            other => other.to_string(),
        }
    }

    fn new_function(&mut self, name: &str, return_ty: Ty, span: Span) -> FnId {
        let id = FnId(u32::try_from(self.functions.len()).expect("too many functions"));
        self.functions.push(TFunction {
            name: name.to_string(),
            method_of: None,
            return_ty,
            locals: Vec::new(),
            param_count: 0,
            body: Vec::new(),
            span,
        });
        self.signatures.push(FnSig {
            params: Vec::new(),
            required: 0,
            variadic: false,
            ret: return_ty,
        });
        self.fn_ret_nullable_flags.push(false);
        id
    }

    /// Checks one function frame: params + hoisting + nested functions +
    /// body. Caller has pushed the scope and fn_stack entries.
    fn check_function_body(
        &mut self,
        id: FnId,
        params: &[ast::Param],
        stmts: &[ast::Stmt],
        span: Span,
    ) -> Vec<TStmt> {
        let fn_index = id.0 as usize;

        // Parameters become the first locals.
        let mut required = params.len();
        let mut seen_default = false;
        for (i, p) in params.iter().enumerate() {
            let ty =
                p.ty.as_ref()
                    .map(|t| self.resolve_type(t))
                    .unwrap_or(Ty::Any);
            if p.rest {
                // `...rest` binds an Array (SPECS §6).
                required = required.min(i);
                self.signatures[fn_index].variadic = true;
            } else if p.default.is_some() {
                if !seen_default {
                    required = i;
                }
                seen_default = true;
            } else if seen_default {
                self.error(
                    ErrorCode::UNEXPECTED_TOKEN,
                    "required parameter cannot follow a defaulted parameter",
                    p.span,
                );
            }
            let default = p.default.as_ref().map(|d| {
                let checked = self.expr(d);
                self.coerce(checked, ty, d.span)
            });
            let local = LocalId(u32::try_from(self.functions[fn_index].locals.len()).unwrap());
            self.functions[fn_index].locals.push(Local {
                name: p.name.clone(),
                ty: if p.rest { Ty::Array } else { ty },
                nullable: p.ty.as_ref().is_some_and(|t| t.nullable),
                is_const: false,
                default,
                is_rest: p.rest,
            });
            self.declare(
                &p.name,
                Symbol::Local {
                    id: local,
                    fn_depth: self.fn_stack.len() - 1,
                },
                p.span,
            );
        }
        self.functions[fn_index].param_count = params.len();
        if !self.signatures[fn_index].variadic {
            self.signatures[fn_index].required = required;
        }
        self.signatures[fn_index].params = self.functions[fn_index]
            .locals
            .iter()
            .take(params.len())
            .filter(|l| !l.is_rest)
            .map(|l| ParamSig { ty: l.ty })
            .collect();

        // Hoist vars and nested function declarations (AS3 `var` is
        // function-scoped; declarations are visible before their statement,
        // ECMA-262 3rd ed. §10.1.3).
        let mut nested: Vec<&ast::FunctionDecl> = Vec::new();
        self.hoist(fn_index, stmts, &mut nested);

        // Register nested function signatures before checking any body so
        // mutual references resolve.
        let mut nested_ids = Vec::new();
        for f in &nested {
            let name = f.name.clone().unwrap_or_else(|| "<anonymous>".into());
            let ret = f
                .return_type
                .as_ref()
                .map(|t| self.resolve_type_allow_void(t))
                .unwrap_or(Ty::Any);
            let nested_id = self.new_function(&name, ret, f.span);
            self.set_ret_nullable(
                nested_id,
                f.return_type.as_ref().is_some_and(|t| t.nullable),
            );
            self.register_signature(nested_id, &f.params);
            if let Some(n) = &f.name {
                self.declare(n, Symbol::Fn(nested_id), f.span);
            }
            nested_ids.push(nested_id);
        }
        for (f, nested_id) in nested.iter().zip(nested_ids) {
            self.enter_function(f, nested_id);
        }

        // Check the body.
        let body: Vec<TStmt> = stmts.iter().map(|s| self.stmt(s)).collect();

        // Missing-return analysis (ASC's "function does not return a value"):
        // a non-void, non-`*` return type requires every path to return.
        let ret = self.functions[fn_index].return_ty;
        if ret != Ty::Void && ret != Ty::Any && completes_normally(&body) {
            self.error(
                ErrorCode::MISSING_RETURN,
                format!(
                    "function `{}` must return `{ret}` on all paths",
                    self.functions[fn_index].name
                ),
                span,
            );
        }
        body
    }

    /// Pre-computes a function's checked signature from its parameter list
    /// (types only — bodies come later).
    fn register_signature(&mut self, id: FnId, params: &[ast::Param]) {
        let mut sig_params = Vec::new();
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
            let ty =
                p.ty.as_ref()
                    .map(|t| self.resolve_type(t))
                    .unwrap_or(Ty::Any);
            sig_params.push(ParamSig { ty });
        }
        let sig = &mut self.signatures[id.0 as usize];
        sig.params = sig_params;
        sig.required = required;
        sig.variadic = variadic;
    }

    /// Checks a nested function in its own scope frame.
    /// Registers the `T?` return flag once the return type is known.
    pub(crate) fn set_ret_nullable(&mut self, id: FnId, nullable: bool) {
        self.fn_ret_nullable_flags[id.0 as usize] = nullable;
    }

    /// Updates narrowing state after an assignment to a local.
    pub(crate) fn update_narrow_on_assign(&mut self, id: LocalId, value_nullable: bool) {
        if value_nullable {
            for set in &mut self.narrowed {
                set.remove(&id);
            }
        } else if let Some(top) = self.narrowed.last_mut() {
            top.insert(id);
        }
    }

    fn enter_function(&mut self, f: &ast::FunctionDecl, id: FnId) {
        self.scopes.push(Scope::default());
        self.fn_stack.push(id.0 as usize);
        let saved_jumps = std::mem::take(&mut self.jumps);
        let saved_labels = std::mem::take(&mut self.labels);
        let saved_narrow = std::mem::take(&mut self.narrowed);
        self.set_ret_nullable(id, f.return_type.as_ref().is_some_and(|t| t.nullable));
        let body = self.check_function_body(id, &f.params, &f.body.stmts, f.span);
        self.functions[id.0 as usize].body = body;
        self.narrowed = saved_narrow;
        self.labels = saved_labels;
        self.jumps = saved_jumps;
        self.fn_stack.pop();
        self.scopes.pop();
    }

    /// Collects hoisted declarations: `var`/`const` bindings anywhere in the
    /// function (not crossing nested function boundaries) and directly
    /// declared functions.
    fn hoist<'b>(
        &mut self,
        fn_index: usize,
        stmts: &'b [ast::Stmt],
        nested: &mut Vec<&'b ast::FunctionDecl>,
    ) {
        use ast::StmtKind::*;
        for stmt in stmts {
            match &stmt.kind {
                VarDecl(decl) => self.hoist_var(fn_index, decl),
                Function(f) => nested.push(f),
                Block(b) => self.hoist(fn_index, &b.stmts, nested),
                If {
                    then_branch,
                    else_branch,
                    ..
                } => {
                    self.hoist(fn_index, std::slice::from_ref(then_branch), nested);
                    if let Some(e) = else_branch {
                        self.hoist(fn_index, std::slice::from_ref(e), nested);
                    }
                }
                While { body, .. } | DoWhile { body, .. } | Labeled { body, .. } => {
                    self.hoist(fn_index, std::slice::from_ref(body), nested);
                }
                For { init, body, .. } => {
                    if let Some(init) = init {
                        if let ast::ForInit::VarDecl(decl) = init.as_ref() {
                            self.hoist_var(fn_index, decl);
                        }
                    }
                    self.hoist(fn_index, std::slice::from_ref(body), nested);
                }
                ForIn { target, body, .. } => {
                    if let ast::ForInTarget::VarDecl(decl) = target {
                        self.hoist_var(fn_index, decl);
                    }
                    self.hoist(fn_index, std::slice::from_ref(body), nested);
                }
                Switch { cases, .. } => {
                    for case in cases {
                        self.hoist(fn_index, &case.body, nested);
                    }
                }
                Try {
                    block,
                    catches,
                    finally,
                } => {
                    self.hoist(fn_index, &block.stmts, nested);
                    for c in catches {
                        self.hoist(fn_index, &c.body.stmts, nested);
                    }
                    if let Some(f) = finally {
                        self.hoist(fn_index, &f.stmts, nested);
                    }
                }
                // Package-level functions/vars hoist like top-level ones;
                // class/interface members do not hoist here.
                Package { body, .. } => self.hoist(fn_index, body, nested),
                Class(_) | Interface(_) | Import { .. } => {}
                Expr(_) | Break { .. } | Continue { .. } | Return { .. } | Throw { .. } | Empty => {
                }
            }
        }
    }

    fn hoist_var(&mut self, fn_index: usize, decl: &ast::VarDecl) {
        let fn_depth = self.fn_stack.len() - 1;
        for b in &decl.bindings {
            let ty =
                b.ty.as_ref()
                    .map(|t| self.resolve_type(t))
                    .unwrap_or(Ty::Any);
            // Redeclaration: same type is tolerated (AS3 function-scoped
            // `var`), a different type conflicts (ASC "conflicting
            // definition").
            if let Some(Symbol::Local { id, .. }) = self.lookup_current_fn(&b.name) {
                let existing = self.functions[fn_index].locals[id.0 as usize].ty;
                if existing != ty {
                    self.error(
                        ErrorCode::CONFLICTING_DECL,
                        format!(
                            "`{}` is already declared as `{existing}` in this function",
                            b.name
                        ),
                        b.span,
                    );
                }
                continue;
            }
            let local = LocalId(u32::try_from(self.functions[fn_index].locals.len()).unwrap());
            self.functions[fn_index].locals.push(Local {
                name: b.name.clone(),
                ty,
                nullable: b.ty.as_ref().is_some_and(|t| t.nullable),
                is_const: decl.is_const,
                default: None,
                is_rest: false,
            });
            self.declare(
                &b.name,
                Symbol::Local {
                    id: local,
                    fn_depth,
                },
                b.span,
            );
        }
    }

    // --- scopes -------------------------------------------------------------

    fn declare(&mut self, name: &str, symbol: Symbol, span: Span) {
        let scope = self.scopes.last_mut().expect("scope");
        if scope.symbols.contains_key(name) && !matches!(symbol, Symbol::Local { .. }) {
            self.error(
                ErrorCode::CONFLICTING_DECL,
                format!("`{name}` is already declared in this scope"),
                span,
            );
            return;
        }
        scope.symbols.insert(name.to_string(), symbol);
    }

    fn lookup(&self, name: &str) -> Option<Symbol> {
        for scope in self.scopes.iter().rev() {
            if let Some(s) = scope.symbols.get(name) {
                return Some(*s);
            }
        }
        if let Some(b) = BuiltinFn::lookup(name) {
            return Some(Symbol::Builtin(b));
        }
        builtins::global_const(name).map(Symbol::Const)
    }

    fn lookup_current_fn(&self, name: &str) -> Option<Symbol> {
        self.scopes
            .last()
            .and_then(|s| s.symbols.get(name))
            .copied()
    }

    // --- types ---------------------------------------------------------------

    fn resolve_type(&mut self, t: &ast::TypeRef) -> Ty {
        let ty = self.resolve_type_allow_void(t);
        if ty == Ty::Void {
            self.error(
                ErrorCode::VOID_VALUE,
                "`void` is only valid as a return type",
                t.span,
            );
            return Ty::Error;
        }
        ty
    }

    fn resolve_type_allow_void(&mut self, t: &ast::TypeRef) -> Ty {
        // `T?` (SPECS §4.1) is parsed and recorded; enforcement is P5 —
        // nullable and plain types are identical until then.
        match &t.kind {
            ast::TypeRefKind::Any => Ty::Any,
            ast::TypeRefKind::Void => Ty::Void,
            ast::TypeRefKind::Name { path, type_args } => {
                if !type_args.is_empty() {
                    let args: Vec<Ty> = type_args.iter().map(|a| self.resolve_type(a)).collect();
                    if path.as_slice() == ["Vector"] {
                        if args.len() != 1 {
                            self.error(
                                ErrorCode::WRONG_ARG_COUNT,
                                "Vector takes exactly one type argument",
                                t.span,
                            );
                            return Ty::Error;
                        }
                        return self.vector_of(args[0]);
                    }
                    if let [single] = path.as_slice() {
                        if let Some(tid) = self.template_index(single) {
                            return Ty::Class(self.instantiate_template(tid, args, t.span));
                        }
                    }
                    self.error(
                        ErrorCode::UNRESOLVED_NAME,
                        format!("unknown generic type `{}`", path.join(".")),
                        t.span,
                    );
                    return Ty::Error;
                }
                if let [single] = path.as_slice() {
                    // Active type parameters shadow everything (SPECS §4.2).
                    for frame in self.subst.iter().rev() {
                        if let Some(&ty) = frame.get(single) {
                            return ty;
                        }
                    }
                    if let Some(ty) = builtins::type_name(single) {
                        return ty;
                    }
                    if single == "Array" {
                        return Ty::Array;
                    }
                    if self.template_index(single).is_some() {
                        self.error(
                            ErrorCode::WRONG_ARG_COUNT,
                            format!("generic class `{single}` needs type arguments"),
                            t.span,
                        );
                        return Ty::Error;
                    }
                }
                match self.type_names.get(&path.join(".")) {
                    Some(TypeSym::Class(id)) => return Ty::Class(*id),
                    Some(TypeSym::Iface(id)) => return Ty::Iface(*id),
                    None => {}
                }
                // `Object` is the implicit root class (SPECS §3.10).
                if path.as_slice() == ["Object"] {
                    return Ty::Class(crate::classes::OBJECT);
                }
                self.error(
                    ErrorCode::UNRESOLVED_NAME,
                    format!("unknown type `{}`", path.join(".")),
                    t.span,
                );
                Ty::Error
            }
        }
    }

    // --- coercion -------------------------------------------------------------

    /// Whether `from` implicitly converts to `to`, and how. `None` = illegal.
    ///
    /// Rules: int/uint/Number mutually implicit (all numeric-family,
    /// Verifier.cpp coerce_i/u/d 1663-1692); `*` ↔ T implicit both ways
    /// (coerce_a 1654); null only into reference types — machine types
    /// cannot hold null (Verifier.cpp:1604 isMachineType). String↔Number is
    /// NOT implicit (String is not numeric-family). DOC GAP: numeric→Boolean
    /// in assignment is rejected here (conservative; the ASC strict-mode
    /// table is absent from docs/ — flagged in the P2 report).
    fn conversion(&self, from: Ty, to: Ty) -> Option<Option<Coercion>> {
        use Ty::*;
        if from == to || from == Error || to == Error {
            return Some(None);
        }
        match (from, to) {
            (_, Any) => Some(Some(Coercion::ToAny)),
            (Any, Int) => Some(Some(Coercion::ToInt)),
            (Any, UInt) => Some(Some(Coercion::ToUInt)),
            (Any, Number) => Some(Some(Coercion::ToNumber)),
            (Any, Boolean) => Some(Some(Coercion::ToBoolean)),
            (Any, _) => Some(Some(Coercion::FromAny)),
            (Int | UInt | Number, Int) => Some(Some(Coercion::ToInt)),
            (Int | UInt | Number, UInt) => Some(Some(Coercion::ToUInt)),
            (Int | UInt | Number, Number) => Some(Some(Coercion::ToNumber)),
            (Null, String | Function | Class(_) | Iface(_)) => Some(None),
            // Widening reference conversions are representation no-ops
            // (SPECS §4.5 nominal subtyping). Narrowing needs `as`/a cast
            // (ASC error 1118 territory).
            (Class(a), Class(b)) if self.registry.is_subclass(a, b) => Some(None),
            (Class(a), Iface(i)) if self.registry.implements(a, i) => Some(None),
            (Iface(a), Iface(b)) if self.registry.iface_extends(a, b) => Some(None),
            // Every interface value is an Object.
            (Iface(_), Class(c)) if c == crate::classes::OBJECT => Some(None),
            _ => None,
        }
    }

    /// Coerces `expr` to `to`, inserting a node or reporting E0302.
    fn coerce(&mut self, expr: TExpr, to: Ty, span: Span) -> TExpr {
        if expr.ty == Ty::Void {
            self.error(
                ErrorCode::VOID_VALUE,
                "a void expression has no value",
                span,
            );
            return TExpr {
                ty: Ty::Error,
                span,
                kind: TExprKind::Error,
            };
        }
        match self.conversion(expr.ty, to) {
            Some(None) => expr,
            Some(Some(c)) => TExpr {
                ty: to,
                span: expr.span,
                kind: TExprKind::Coerce(c, Box::new(expr)),
            },
            None => {
                self.error(
                    ErrorCode::INCOMPATIBLE_TYPES,
                    format!(
                        "cannot implicitly convert `{}` to `{}`",
                        self.ty_name(expr.ty),
                        self.ty_name(to)
                    ),
                    span,
                );
                TExpr {
                    ty: Ty::Error,
                    span,
                    kind: TExprKind::Error,
                }
            }
        }
    }

    /// Condition context: any non-void type converts (ECMA-262 3rd ed. §9.2
    /// ToBoolean is total).
    fn coerce_condition(&mut self, expr: TExpr) -> TExpr {
        if expr.ty == Ty::Boolean || expr.ty == Ty::Error {
            return expr;
        }
        if expr.ty == Ty::Void {
            let span = expr.span;
            return self.coerce(expr, Ty::Boolean, span); // reports E0309
        }
        TExpr {
            ty: Ty::Boolean,
            span: expr.span,
            kind: TExprKind::Coerce(Coercion::ToBoolean, Box::new(expr)),
        }
    }

    pub(crate) fn error(&mut self, code: ErrorCode, message: impl Into<String>, span: Span) {
        self.diagnostics
            .push(Diagnostic::error(code, message).with_span(span));
    }

    fn warn(&mut self, message: impl Into<String>, span: Span) {
        self.diagnostics
            .push(Diagnostic::warning(message).with_span(span));
    }

    /// Signature parts for a declared function: (param types, required,
    /// variadic, name).
    fn fn_sig_parts(&self, id: FnId) -> (Vec<Ty>, usize, bool, String) {
        let sig = &self.signatures[id.0 as usize];
        (
            sig.params.iter().map(|p| p.ty).collect(),
            sig.required,
            sig.variadic,
            self.functions[id.0 as usize].name.clone(),
        )
    }

    fn fn_return(&self, id: FnId) -> Ty {
        self.signatures[id.0 as usize].ret
    }

    /// Type and constness of a local, given its resolution.
    fn local_info(&self, id: LocalId, fn_depth: usize) -> (Ty, bool) {
        let fn_index = self.fn_stack[fn_depth];
        let local = &self.functions[fn_index].locals[id.0 as usize];
        (local.ty, local.is_const)
    }

    fn error_expr(&mut self, span: Span) -> TExpr {
        TExpr {
            ty: Ty::Error,
            span,
            kind: TExprKind::Error,
        }
    }

    // --- statements ------------------------------------------------------------

    fn stmt(&mut self, stmt: &ast::Stmt) -> TStmt {
        use ast::StmtKind as S;
        let span = stmt.span;
        let kind = match &stmt.kind {
            S::Expr(e) => TStmtKind::Expr(self.expr(e)),
            S::VarDecl(decl) => return self.var_decl_stmt(decl, span),
            // Hoisted; the declaration site itself is inert.
            S::Function(_) => TStmtKind::Empty,
            // Types were processed in passes A–C; imports resolve within
            // the single compilation unit already.
            S::Class(_) | S::Interface(_) | S::Import { .. } => TStmtKind::Empty,
            // Package bodies execute like top-level code (declarations were
            // collected in pass A).
            S::Package { body, .. } => {
                TStmtKind::Block(body.iter().map(|s| self.stmt(s)).collect())
            }
            S::Block(b) => TStmtKind::Block(b.stmts.iter().map(|s| self.stmt(s)).collect()),
            S::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let cond = self.expr(cond);
                let cond = self.coerce_condition(cond);
                let (when_true, when_false) = Self::narrowing_of(&cond);
                self.narrowed.push(when_true.iter().copied().collect());
                let then_checked = Box::new(self.stmt(then_branch));
                self.narrowed.pop();
                self.narrowed.push(when_false.iter().copied().collect());
                let else_checked = else_branch.as_ref().map(|e| Box::new(self.stmt(e)));
                self.narrowed.pop();
                // Early-exit narrowing: `if (x == null) return;` proves x
                // non-null afterwards (and symmetrically).
                let then_exits = !stmt_completes(&then_checked);
                let else_exits = else_checked.as_ref().is_some_and(|e| !stmt_completes(e));
                if then_exits {
                    if let Some(top) = self.narrowed.last_mut() {
                        top.extend(when_false.iter().copied());
                    }
                }
                if else_exits {
                    if let Some(top) = self.narrowed.last_mut() {
                        top.extend(when_true.iter().copied());
                    }
                }
                TStmtKind::If {
                    cond,
                    then_branch: then_checked,
                    else_branch: else_checked,
                }
            }
            S::While { cond, body } => {
                let cond = self.expr(cond);
                let cond = self.coerce_condition(cond);
                let (when_true, _) = Self::narrowing_of(&cond);
                self.narrowed.push(when_true.into_iter().collect());
                let body = Box::new(self.loop_body(body, None));
                self.narrowed.pop();
                TStmtKind::While { cond, body }
            }
            S::DoWhile { body, cond } => {
                let body = Box::new(self.loop_body(body, None));
                let cond = self.expr(cond);
                let cond = self.coerce_condition(cond);
                TStmtKind::DoWhile { body, cond }
            }
            S::For {
                init,
                cond,
                update,
                body,
            } => {
                let init = init.as_ref().map(|i| {
                    Box::new(match i.as_ref() {
                        ast::ForInit::VarDecl(d) => self.var_decl_stmt(d, d.span),
                        ast::ForInit::Expr(e) => TStmt {
                            span: e.span,
                            kind: TStmtKind::Expr(self.expr(e)),
                        },
                    })
                });
                let cond = cond.as_ref().map(|c| {
                    let c2 = self.expr(c);
                    self.coerce_condition(c2)
                });
                let update = update.as_ref().map(|u| self.expr(u));
                TStmtKind::For {
                    init,
                    cond,
                    update,
                    body: Box::new(self.loop_body(body, None)),
                }
            }
            S::ForIn {
                is_each,
                target,
                object,
                body,
            } => {
                let object = self.expr(object);
                let target = self.for_in_target(target);
                TStmtKind::ForIn {
                    is_each: *is_each,
                    target,
                    object,
                    body: Box::new(self.loop_body(body, None)),
                }
            }
            S::Switch { scrutinee, cases } => {
                let scrutinee = self.expr(scrutinee);
                self.jumps.push(JumpCtx {
                    label: None,
                    is_loop: false,
                });
                let cases = cases
                    .iter()
                    .map(|c| TCase {
                        // `switch` compares with strict equality — any test
                        // type is legal against any scrutinee.
                        test: c.test.as_ref().map(|t| self.expr(t)),
                        body: c.body.iter().map(|s| self.stmt(s)).collect(),
                    })
                    .collect();
                self.jumps.pop();
                TStmtKind::Switch { scrutinee, cases }
            }
            S::Break { label } => {
                self.check_jump(label.as_deref(), false, span);
                TStmtKind::Break {
                    label: label.clone(),
                }
            }
            S::Continue { label } => {
                self.check_jump(label.as_deref(), true, span);
                TStmtKind::Continue {
                    label: label.clone(),
                }
            }
            S::Return { value } => {
                let fn_index = *self.fn_stack.last().expect("fn");
                let ret = self.functions[fn_index].return_ty;
                let value = match value {
                    Some(v) => {
                        let checked = self.expr(v);
                        if ret == Ty::Void {
                            self.error(
                                ErrorCode::INCOMPATIBLE_TYPES,
                                "a void function cannot return a value",
                                v.span,
                            );
                            None
                        } else {
                            let coerced = self.coerce(checked, ret, v.span);
                            let ret_nullable = self.fn_ret_nullable_flags[fn_index];
                            self.check_null_flow(
                                &coerced,
                                ret,
                                ret_nullable,
                                "return type",
                                v.span,
                            );
                            Some(coerced)
                        }
                    }
                    None => {
                        if ret != Ty::Void && ret != Ty::Any {
                            self.error(
                                ErrorCode::MISSING_RETURN,
                                format!("this function must return `{ret}`"),
                                span,
                            );
                        }
                        None
                    }
                };
                TStmtKind::Return { value }
            }
            S::Throw { value } => TStmtKind::Throw {
                value: {
                    let v = self.expr(value);
                    self.coerce(v, Ty::Any, value.span)
                },
            },
            S::Try {
                block,
                catches,
                finally,
            } => {
                let block = block.stmts.iter().map(|s| self.stmt(s)).collect();
                let catches = catches
                    .iter()
                    .map(|c| {
                        let ty =
                            c.ty.as_ref()
                                .map(|t| self.resolve_type(t))
                                .unwrap_or(Ty::Any);
                        let fn_index = *self.fn_stack.last().expect("fn");
                        let binding =
                            LocalId(u32::try_from(self.functions[fn_index].locals.len()).unwrap());
                        self.functions[fn_index].locals.push(Local {
                            name: c.name.clone(),
                            ty,
                            nullable: false, // thrown values are non-null
                            is_const: false,
                            default: None,
                            is_rest: false,
                        });
                        self.declare(
                            &c.name,
                            Symbol::Local {
                                id: binding,
                                fn_depth: self.fn_stack.len() - 1,
                            },
                            c.span,
                        );
                        TCatch {
                            binding,
                            body: c.body.stmts.iter().map(|s| self.stmt(s)).collect(),
                        }
                    })
                    .collect();
                let finally = finally
                    .as_ref()
                    .map(|f| f.stmts.iter().map(|s| self.stmt(s)).collect());
                TStmtKind::Try {
                    block,
                    catches,
                    finally,
                }
            }
            S::Labeled { label, body } => {
                self.labels.push(label.clone());
                let body = if is_loop_stmt(body) {
                    self.loop_body(body, Some(label.clone()))
                } else {
                    self.jumps.push(JumpCtx {
                        label: Some(label.clone()),
                        is_loop: false,
                    });
                    let b = self.stmt(body);
                    self.jumps.pop();
                    b
                };
                self.labels.pop();
                TStmtKind::Labeled {
                    label: label.clone(),
                    body: Box::new(body),
                }
            }
            S::Empty => TStmtKind::Empty,
        };
        TStmt { kind, span }
    }

    fn loop_body(&mut self, body: &ast::Stmt, label: Option<String>) -> TStmt {
        self.jumps.push(JumpCtx {
            label,
            is_loop: true,
        });
        let b = self.stmt(body);
        self.jumps.pop();
        b
    }

    fn check_jump(&mut self, label: Option<&str>, is_continue: bool, span: Span) {
        let ok = match label {
            None => self
                .jumps
                .iter()
                .any(|j| if is_continue { j.is_loop } else { true }),
            Some(l) => self
                .jumps
                .iter()
                .any(|j| j.label.as_deref() == Some(l) && (!is_continue || j.is_loop)),
        };
        if !ok {
            let what = if is_continue { "continue" } else { "break" };
            let msg = match label {
                Some(l) => format!("`{what} {l}`: no enclosing loop is labeled `{l}`"),
                None => format!(
                    "`{what}` outside of a loop{}",
                    if is_continue { "" } else { " or switch" }
                ),
            };
            self.error(ErrorCode::BAD_JUMP, msg, span);
        }
    }

    fn var_decl_stmt(&mut self, decl: &ast::VarDecl, span: Span) -> TStmt {
        // Bindings were hoisted; here we only check initializers.
        let mut inits = Vec::new();
        for b in &decl.bindings {
            let Some(Symbol::Local { id, .. }) = self.lookup_current_fn(&b.name) else {
                continue; // hoist already reported the problem
            };
            let fn_index = *self.fn_stack.last().expect("fn");
            let ty = self.functions[fn_index].locals[id.0 as usize].ty;
            if let Some(init) = &b.init {
                let checked = self.expr(init);
                let coerced = self.coerce(checked, ty, init.span);
                let fn_index = *self.fn_stack.last().expect("fn");
                let nullable_slot = self.functions[fn_index].locals[id.0 as usize].nullable;
                self.check_null_flow(&coerced, ty, nullable_slot, "variable", init.span);
                self.update_narrow_on_assign(id, self.expr_nullable(&coerced));
                inits.push(TStmt {
                    span: b.span,
                    kind: TStmtKind::Assign(id, coerced),
                });
            } else if decl.is_const {
                self.error(
                    ErrorCode::ASSIGN_TO_CONST,
                    format!("const `{}` needs an initializer", b.name),
                    b.span,
                );
            }
        }
        match inits.len() {
            0 => TStmt {
                span,
                kind: TStmtKind::Empty,
            },
            1 => inits.pop().expect("len checked"),
            _ => TStmt {
                span,
                kind: TStmtKind::Block(inits),
            },
        }
    }

    fn for_in_target(&mut self, target: &ast::ForInTarget) -> LocalId {
        match target {
            ast::ForInTarget::VarDecl(decl) => {
                let b = &decl.bindings[0];
                match self.lookup_current_fn(&b.name) {
                    Some(Symbol::Local { id, .. }) => id,
                    _ => LocalId(0),
                }
            }
            ast::ForInTarget::Expr(e) => match &e.kind {
                ast::ExprKind::Ident(name) => match self.lookup(name) {
                    Some(Symbol::Local { id, fn_depth }) => {
                        self.check_capture(name, fn_depth, e.span);
                        id
                    }
                    _ => {
                        self.error(
                            ErrorCode::UNRESOLVED_NAME,
                            format!("cannot find `{name}` in this scope"),
                            e.span,
                        );
                        LocalId(0)
                    }
                },
                _ => {
                    self.error(
                        ErrorCode::NOT_IMPLEMENTED,
                        "only simple variables are supported as `for..in` targets until Phase 4",
                        e.span,
                    );
                    LocalId(0)
                }
            },
        }
    }

    fn check_capture(&mut self, name: &str, fn_depth: usize, span: Span) {
        if fn_depth != self.fn_stack.len() - 1 {
            self.error(
                ErrorCode::NOT_IMPLEMENTED,
                format!("`{name}` is declared in an enclosing function; closures are not implemented until Phase 6"),
                span,
            );
        }
    }
}

/// True if executing `stmts` can fall off the end (conservative:
/// loops/switches are assumed to complete; If/Try/Return/Throw are precise).
fn completes_normally(stmts: &[TStmt]) -> bool {
    // A sequence completes iff every statement does (after the first
    // non-completing statement the rest is unreachable anyway).
    stmts.iter().all(stmt_completes)
}

fn stmt_completes(stmt: &TStmt) -> bool {
    match &stmt.kind {
        TStmtKind::Return { .. } | TStmtKind::Throw { .. } => false,
        TStmtKind::Block(b) => completes_normally(b),
        TStmtKind::If {
            then_branch,
            else_branch: Some(e),
            ..
        } => stmt_completes(then_branch) || stmt_completes(e),
        // No else: the false path always falls through.
        TStmtKind::If { .. } => true,
        TStmtKind::Try {
            block,
            catches,
            finally,
        } => {
            let try_completes =
                completes_normally(block) || catches.iter().any(|c| completes_normally(&c.body));
            let finally_completes = finally.as_ref().is_none_or(|f| completes_normally(f));
            try_completes && finally_completes
        }
        TStmtKind::Labeled { body, .. } => stmt_completes(body),
        // A switch exits normally only by (a) an unlabeled break in a case
        // body, (b) the final clause completing (fall-through chains end
        // there), or (c) no matching case when there is no default.
        TStmtKind::Switch { cases, .. } => {
            let has_default = cases.iter().any(|c| c.test.is_none());
            let last_completes = cases.last().is_none_or(|c| completes_normally(&c.body));
            !has_default || last_completes || cases.iter().any(|c| has_direct_break(&c.body))
        }
        // `do..while` runs its body at least once: a body that never
        // completes (and contains no break to this loop) never falls out.
        TStmtKind::DoWhile { body, .. } => {
            stmt_completes(body) || has_direct_break(std::slice::from_ref(body))
        }
        _ => true,
    }
}

/// Whether an unlabeled `break` occurs that would bind to the *enclosing*
/// breakable construct — i.e. not nested inside an inner loop or switch.
/// Labeled breaks are ignored (they bind their label). Missing a break here
/// only suppresses a missing-return diagnostic (false negative), never
/// creates a spurious one.
fn has_direct_break(stmts: &[TStmt]) -> bool {
    stmts.iter().any(|s| match &s.kind {
        TStmtKind::Break { label: None } => true,
        TStmtKind::Block(b) => has_direct_break(b),
        TStmtKind::If {
            then_branch,
            else_branch,
            ..
        } => {
            has_direct_break(std::slice::from_ref(then_branch))
                || else_branch
                    .as_ref()
                    .is_some_and(|e| has_direct_break(std::slice::from_ref(e)))
        }
        TStmtKind::Try {
            block,
            catches,
            finally,
        } => {
            has_direct_break(block)
                || catches.iter().any(|c| has_direct_break(&c.body))
                || finally.as_ref().is_some_and(|f| has_direct_break(f))
        }
        TStmtKind::Labeled { body, .. } => has_direct_break(std::slice::from_ref(body)),
        // Inner loops/switches capture unlabeled breaks.
        _ => false,
    })
}

fn is_loop_stmt(stmt: &ast::Stmt) -> bool {
    matches!(
        stmt.kind,
        ast::StmtKind::While { .. }
            | ast::StmtKind::DoWhile { .. }
            | ast::StmtKind::For { .. }
            | ast::StmtKind::ForIn { .. }
    )
}
