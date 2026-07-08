//! Expression typing. Result-type rules cite the AVM2 verifier
//! (`docs/avmplus/core/Verifier.cpp`) — the reference for what each
//! operation statically produces.

use ast::{AssignOp, BinaryOp, ExprKind, UnaryOp};
use diagnostics::ErrorCode;
use span::Span;

use super::{Checker, Symbol};
use crate::builtins::{self, Member, Signature};
use crate::tast::*;
use crate::ty::Ty;

impl Checker<'_> {
    pub(crate) fn expr(&mut self, e: &ast::Expr) -> TExpr {
        let span = e.span;
        match &e.kind {
            ExprKind::Int(v) => mk(Ty::Int, span, TExprKind::Int(*v)),
            ExprKind::UInt(v) => mk(Ty::UInt, span, TExprKind::UInt(*v)),
            ExprKind::Number(v) => mk(Ty::Number, span, TExprKind::Number(*v)),
            ExprKind::Str(v) => mk(Ty::String, span, TExprKind::Str(v.clone())),
            ExprKind::Bool(v) => mk(Ty::Boolean, span, TExprKind::Bool(*v)),
            ExprKind::Null => mk(Ty::Null, span, TExprKind::Null),
            ExprKind::This => self.this_expr(span),
            ExprKind::Super => {
                self.error(
                    ErrorCode::UNEXPECTED_TOKEN,
                    "`super` is only valid as `super(...)` or `super.method(...)`",
                    span,
                );
                self.error_expr(span)
            }
            ExprKind::Ident(name) => self.ident(name, span),
            // Array literal (SPECS §3.10): elements are `*`.
            ExprKind::Array(elements) => {
                let elements = elements
                    .iter()
                    .map(|el| {
                        el.as_ref().map(|el| {
                            let checked = self.expr(el);
                            self.coerce_to_any(checked)
                        })
                    })
                    .collect();
                mk(Ty::Array, span, TExprKind::Array(elements))
            }
            ExprKind::VectorLit { elem, elements } => self.vector_literal(elem, elements, span),
            // Valid in `new`/`is`/`as`/annotation positions (handled
            // there); as a bare value it needs the Class type.
            ExprKind::ApplyType(..) => self.not_implemented(
                span,
                "using a parameterized type as a value requires the Class type — Phase 6",
            ),
            // Object initializer: the Object class lands in P4; `*` until then.
            ExprKind::Object(props) => {
                let props = props
                    .iter()
                    .map(|(name, value)| {
                        let key = match name {
                            ast::PropName::Ident(s) | ast::PropName::Str(s) => s.clone(),
                            ast::PropName::Number(n) => n.to_string(),
                        };
                        let checked = self.expr(value);
                        (key, self.coerce_to_any(checked))
                    })
                    .collect();
                mk(Ty::Any, span, TExprKind::Object(props))
            }
            ExprKind::Function(f) => self.function_expr(f, span),
            ExprKind::Unary(op, operand) => self.unary(*op, operand, span),
            ExprKind::Postfix(op, operand) => {
                // Same typing as prefix: OP_increment → Number,
                // OP_increment_i keeps int (Verifier.cpp:2417-2446).
                let checked = self.expr(operand);
                let ty = if checked.ty.is_numeric() {
                    checked.ty
                } else {
                    Ty::Number
                };
                let operand = self.coerce(checked, ty, span);
                mk(ty, span, TExprKind::Postfix(*op, Box::new(operand)))
            }
            ExprKind::Binary(op, l, r) => self.binary(*op, l, r, span),
            ExprKind::Assign(op, target, value) => self.assign(*op, target, value, span),
            ExprKind::Conditional(c, t, f) => {
                let c = self.expr(c);
                let c = self.coerce_condition(c);
                let t = self.expr(t);
                let f = self.expr(f);
                // Branch-join typing: verifier frame-merge (common base or *).
                let ty = merge_types(t.ty, f.ty);
                let t = self.coerce(t, ty, span);
                let f = self.coerce(f, ty, span);
                mk(
                    ty,
                    span,
                    TExprKind::Conditional(Box::new(c), Box::new(t), Box::new(f)),
                )
            }
            ExprKind::Call(callee, args) => self.call(callee, args, span),
            ExprKind::New(callee, args) => self.new_expr(callee, args, span),
            ExprKind::Member(object, name) => {
                // Static access: unshadowed class name as receiver.
                if let ExprKind::Ident(recv) = &object.kind {
                    if !self.is_shadowed(recv) {
                        if let Some(class) = self.ident_as_class(recv) {
                            return self.static_read(class, name, span);
                        }
                    }
                }
                let object = self.expr(object);
                self.member_read(object, name, span)
            }
            ExprKind::Index(object, index) => {
                let object = self.expr(object);
                let index = self.expr(index);
                self.index_read(object, index, span)
            }
            ExprKind::Comma(l, r) => {
                let l = self.expr(l);
                let r = self.expr(r);
                let ty = r.ty;
                mk(ty, span, TExprKind::Comma(Box::new(l), Box::new(r)))
            }
        }
    }

    fn not_implemented(&mut self, span: Span, msg: &str) -> TExpr {
        self.error(ErrorCode::NOT_IMPLEMENTED, msg, span);
        self.error_expr(span)
    }

    fn ident(&mut self, name: &str, span: Span) -> TExpr {
        match self.lookup(name) {
            Some(Symbol::Local { id, fn_depth }) => {
                self.check_capture(name, fn_depth, span);
                let (ty, _) = self.local_info(id, fn_depth);
                mk(ty, span, TExprKind::LocalGet(id))
            }
            Some(Symbol::Fn(id)) => mk(Ty::Function, span, TExprKind::FnRef(id)),
            Some(Symbol::Builtin(b)) => mk(Ty::Function, span, TExprKind::BuiltinRef(b)),
            Some(Symbol::Const(ty)) => {
                let kind = match name {
                    "NaN" => TExprKind::Number(f64::NAN),
                    "Infinity" => TExprKind::Number(f64::INFINITY),
                    _ => TExprKind::Undefined,
                };
                mk(ty, span, kind)
            }
            None => {
                // Unqualified class members / statics of the enclosing class.
                if let Some(member) = self.implicit_member(name, span) {
                    return member;
                }
                if self.ident_as_type(name).is_some() {
                    // Type name as a value: the Class type is P6.
                    return self.not_implemented(
                        span,
                        "using a type as a value requires the Class type — Phase 6",
                    );
                }
                self.error(
                    ErrorCode::UNRESOLVED_NAME,
                    format!("cannot find `{name}` in this scope"),
                    span,
                );
                self.error_expr(span)
            }
        }
    }

    // --- operators -----------------------------------------------------------

    fn unary(&mut self, op: UnaryOp, operand: &ast::Expr, span: Span) -> TExpr {
        let checked = self.expr(operand);
        match op {
            // OP_not → BOOLEAN_TYPE, operand ToBoolean (Verifier.cpp:2311).
            UnaryOp::Not => {
                let operand = self.coerce_condition(checked);
                mk(Ty::Boolean, span, TExprKind::Unary(op, Box::new(operand)))
            }
            // OP_bitnot → INT_TYPE, operand ToInt32 (Verifier.cpp:2494).
            UnaryOp::BitNot => {
                let operand = self.coerce(checked, Ty::Int, span);
                mk(Ty::Int, span, TExprKind::Unary(op, Box::new(operand)))
            }
            // OP_unplus → NUMBER_TYPE (Verifier.cpp:1712).
            UnaryOp::Plus => {
                let operand = self.coerce(checked, Ty::Number, span);
                mk(Ty::Number, span, TExprKind::Unary(op, Box::new(operand)))
            }
            // OP_negate → NUMBER (Verifier.cpp:2411); OP_negate_i keeps int
            // (2458). Negated uint leaves uint range → Number.
            UnaryOp::Minus => {
                let ty = if checked.ty == Ty::Int {
                    Ty::Int
                } else {
                    Ty::Number
                };
                let operand = self.coerce(checked, ty, span);
                mk(ty, span, TExprKind::Unary(op, Box::new(operand)))
            }
            // OP_typeof → STRING_TYPE (Verifier.cpp:2500).
            UnaryOp::Typeof => {
                let operand = self.coerce_to_any(checked);
                mk(Ty::String, span, TExprKind::Unary(op, Box::new(operand)))
            }
            // `void e` evaluates and yields undefined (ES3 §11.4.2).
            UnaryOp::Void => {
                let operand = self.coerce_to_any(checked);
                mk(Ty::Any, span, TExprKind::Unary(op, Box::new(operand)))
            }
            // OP_deleteproperty → BOOLEAN_TYPE (Verifier.cpp:1579).
            UnaryOp::Delete => {
                if !matches!(checked.kind, TExprKind::Member(..) | TExprKind::Index(..)) {
                    self.error(
                        ErrorCode::UNSUPPORTED_SYNTAX,
                        "`delete` operates on a property (`obj.name` or `obj[key]`)",
                        span,
                    );
                }
                mk(Ty::Boolean, span, TExprKind::Unary(op, Box::new(checked)))
            }
            // OP_increment → NUMBER; OP_increment_i keeps int locals int
            // (Verifier.cpp:2417-2446).
            UnaryOp::PreInc | UnaryOp::PreDec => {
                let ty = if checked.ty.is_numeric() {
                    checked.ty
                } else {
                    Ty::Number
                };
                let operand = self.coerce(checked, ty, span);
                mk(ty, span, TExprKind::Unary(op, Box::new(operand)))
            }
        }
    }

    fn binary(&mut self, op: BinaryOp, l: &ast::Expr, r: &ast::Expr, span: Span) -> TExpr {
        use BinaryOp::*;
        // `is`/`as`: RHS is a type name, resolved statically in P2 (class
        // values arrive P4).
        if op == Is || op == As {
            let lhs = self.expr(l);
            let lhs = self.coerce_to_any(lhs);
            let target = self.type_operand(r);
            return if op == Is {
                // OP_istype → BOOLEAN_TYPE (Verifier.cpp:1742).
                mk(Ty::Boolean, span, TExprKind::Is(Box::new(lhs), target))
            } else {
                // OP_astype: result is the target type, but `*` when the
                // target is a machine type — `as` yields null on failure and
                // int/uint/Number/Boolean cannot hold null
                // (Verifier.cpp:1601-1605).
                let ty = match target {
                    Ty::Int | Ty::UInt | Ty::Number | Ty::Boolean => Ty::Any,
                    t => t,
                };
                mk(ty, span, TExprKind::As(Box::new(lhs), target))
            };
        }
        if op == Instanceof {
            // Parsed but deprecated in favor of `is` (SPECS §3.9).
            self.warn("`instanceof` is deprecated; use `is`", span);
        }
        let lhs = self.expr(l);
        let rhs = self.expr(r);
        self.binary_typed(op, lhs, rhs, span)
    }

    /// Types a binary operation whose operands are already checked (shared
    /// with compound assignment).
    fn binary_typed(&mut self, op: BinaryOp, lhs: TExpr, rhs: TExpr, span: Span) -> TExpr {
        use BinaryOp::*;
        match op {
            // Logical ops keep operand values; the result is the verifier's
            // branch-join of the operand types.
            LogAnd | LogOr => {
                let ty = merge_types(lhs.ty, rhs.ty);
                let lhs = self.coerce(lhs, ty, span);
                let rhs = self.coerce(rhs, ty, span);
                mk(
                    ty,
                    span,
                    TExprKind::Logical(op, Box::new(lhs), Box::new(rhs)),
                )
            }
            // OP_add: String operand → STRING (Verifier.cpp:2326); both
            // numeric → NUMBER (2331); otherwise unknown → OBJECT/`*` (2357).
            Add => {
                if lhs.ty == Ty::String || rhs.ty == Ty::String {
                    let lhs = self.concat_operand(lhs);
                    let rhs = self.concat_operand(rhs);
                    mk(
                        Ty::String,
                        span,
                        TExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                    )
                } else if lhs.ty.is_numeric() && rhs.ty.is_numeric() {
                    let lhs = self.coerce(lhs, Ty::Number, span);
                    let rhs = self.coerce(rhs, Ty::Number, span);
                    mk(
                        Ty::Number,
                        span,
                        TExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                    )
                } else {
                    let lhs = self.coerce_to_any(lhs);
                    let rhs = self.coerce_to_any(rhs);
                    mk(
                        Ty::Any,
                        span,
                        TExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                    )
                }
            }
            // OP_subtract/multiply/divide/modulo → NUMBER_TYPE
            // (Verifier.cpp:2367-2408).
            Sub | Mul | Div | Rem => {
                let lhs = self.coerce(lhs, Ty::Number, span);
                let rhs = self.coerce(rhs, Ty::Number, span);
                mk(
                    Ty::Number,
                    span,
                    TExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                )
            }
            // OP_bitand/bitor/bitxor/lshift/rshift → INT_TYPE
            // (Verifier.cpp:2464-2484).
            BitAnd | BitOr | BitXor | Shl | Shr => {
                let lhs = self.coerce(lhs, Ty::Int, span);
                let rhs = self.coerce(rhs, Ty::Int, span);
                mk(
                    Ty::Int,
                    span,
                    TExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                )
            }
            // OP_urshift → UINT_TYPE (Verifier.cpp:2486).
            Ushr => {
                let lhs = self.coerce(lhs, Ty::UInt, span);
                let rhs = self.coerce(rhs, Ty::Int, span);
                mk(
                    Ty::UInt,
                    span,
                    TExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                )
            }
            // Comparisons → BOOLEAN; one numeric side coerces the other to
            // Number (Verifier.cpp:2264-2293).
            Lt | Gt | Le | Ge => {
                let (lhs, rhs) = if lhs.ty.is_numeric() || rhs.ty.is_numeric() {
                    let lhs = self.coerce(lhs, Ty::Number, span);
                    let rhs = self.coerce(rhs, Ty::Number, span);
                    (lhs, rhs)
                } else {
                    (lhs, rhs)
                };
                mk(
                    Ty::Boolean,
                    span,
                    TExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                )
            }
            // OP_equals/strictequals/instanceof/in → BOOLEAN
            // (Verifier.cpp:2296-2309).
            Eq | Ne | StrictEq | StrictNe | Instanceof | In => mk(
                Ty::Boolean,
                span,
                TExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
            ),
            Is | As => unreachable!("handled in binary()"),
        }
    }

    /// Coerces a `+` operand for string concatenation (ES3 §11.6.1 ToString
    /// on the non-string side).
    fn concat_operand(&mut self, e: TExpr) -> TExpr {
        if e.ty == Ty::String || e.ty == Ty::Error {
            return e;
        }
        TExpr {
            ty: Ty::String,
            span: e.span,
            kind: TExprKind::Coerce(Coercion::ToString, Box::new(e)),
        }
    }

    /// Resolves the RHS of `is`/`as` to a core type, class, or interface.
    fn type_operand(&mut self, e: &ast::Expr) -> Ty {
        if let Some(ty) = self.apply_type_to_ty(e) {
            return ty;
        }
        if let ExprKind::Ident(name) = &e.kind {
            if !self.is_shadowed(name) {
                if let Some(ty) = self.ident_as_type(name) {
                    return ty;
                }
            }
        }
        self.error(
            ErrorCode::NOT_A_TYPE,
            "the right side of `is`/`as` must name a type",
            e.span,
        );
        Ty::Error
    }

    // --- assignment -------------------------------------------------------------

    fn assign(&mut self, op: AssignOp, target: &ast::Expr, value: &ast::Expr, span: Span) -> TExpr {
        // Compound assignment on properties needs receiver temps (P4's
        // lowering); locals only for now.
        if op != AssignOp::Assign && !matches!(target.kind, ExprKind::Ident(_)) {
            return self.not_implemented(
                span,
                "compound assignment to properties is not implemented until Phase 4",
            );
        }
        match &target.kind {
            ExprKind::Ident(name) => {
                let Some(sym) = self.lookup(name) else {
                    // Unqualified field/static of the enclosing class.
                    let checked = self.expr(value);
                    if let Some(result) = self.implicit_member_write(name, checked, span) {
                        return result;
                    }
                    self.error(
                        ErrorCode::UNRESOLVED_NAME,
                        format!("cannot find `{name}` in this scope"),
                        target.span,
                    );
                    return self.error_expr(span);
                };
                let (id, ty) = match sym {
                    Symbol::Local { id, fn_depth } => {
                        self.check_capture(name, fn_depth, target.span);
                        let (ty, is_const) = self.local_info(id, fn_depth);
                        if is_const {
                            self.error(
                                ErrorCode::ASSIGN_TO_CONST,
                                format!("cannot assign to const `{name}`"),
                                span,
                            );
                        }
                        (id, ty)
                    }
                    _ => {
                        self.error(
                            ErrorCode::INVALID_ASSIGN_TARGET,
                            format!("`{name}` is not assignable"),
                            span,
                        );
                        self.expr(value);
                        return self.error_expr(span);
                    }
                };
                let rhs = self.compound_rhs(op, ty, id, value, span);
                let rhs = self.coerce(rhs, ty, span);
                let fn_index = *self.fn_stack.last().expect("fn");
                let nullable_slot = self.functions[fn_index]
                    .locals
                    .get(id.0 as usize)
                    .is_some_and(|l| l.nullable);
                self.check_null_flow(&rhs, ty, nullable_slot, "variable", span);
                self.update_narrow_on_assign(id, self.expr_nullable(&rhs));
                mk(ty, span, TExprKind::LocalSet(id, Box::new(rhs)))
            }
            ExprKind::Member(object, name) => {
                // Static member write.
                if let ExprKind::Ident(recv) = &object.kind {
                    if !self.is_shadowed(recv) {
                        if let Some(class) = self.ident_as_class(recv) {
                            let checked = self.expr(value);
                            return self.static_write(class, name, checked, span);
                        }
                    }
                }
                let object = self.expr(object);
                match object.ty {
                    Ty::Class(class) => {
                        let checked = self.expr(value);
                        return self.class_member_write(object, class, name, checked, span);
                    }
                    Ty::Iface(iface) => {
                        let checked = self.expr(value);
                        return self.iface_member_write(object, iface, name, checked, span);
                    }
                    Ty::Array | Ty::Vector(_) => {
                        let checked = self.expr(value);
                        return self.seq_member_write(object, name, checked, span);
                    }
                    _ => {}
                }
                let value_checked = self.expr(value);
                if object.ty == Ty::Any || object.ty == Ty::Error {
                    let value_checked = self.coerce_to_any(value_checked);
                    let ty = value_checked.ty;
                    mk(
                        ty,
                        span,
                        TExprKind::MemberSet(
                            Box::new(object),
                            name.clone(),
                            Box::new(value_checked),
                        ),
                    )
                } else {
                    // Core-type members are methods/read-only props; sealed
                    // classes reject unknown or read-only writes (SPECS §3.2).
                    self.error(
                        ErrorCode::UNKNOWN_PROPERTY,
                        format!(
                            "cannot write property `{name}` on sealed type `{}`",
                            object.ty
                        ),
                        span,
                    );
                    self.error_expr(span)
                }
            }
            ExprKind::Index(object, index) => {
                let object = self.expr(object);
                let index = self.expr(index);
                let value_checked = self.expr(value);
                if matches!(object.ty, Ty::Array | Ty::Vector(_)) {
                    return self.seq_index_write(object, index, value_checked, span);
                }
                if object.ty != Ty::Any && object.ty != Ty::Error {
                    self.error(
                        ErrorCode::UNKNOWN_PROPERTY,
                        format!("cannot index-assign on sealed type `{}`", object.ty),
                        span,
                    );
                    return self.error_expr(span);
                }
                let index = self.coerce_to_any(index);
                let value_checked = self.coerce_to_any(value_checked);
                mk(
                    Ty::Any,
                    span,
                    TExprKind::IndexSet(Box::new(object), Box::new(index), Box::new(value_checked)),
                )
            }
            _ => {
                // Parser already rejected other targets.
                self.expr(value);
                self.error_expr(span)
            }
        }
    }

    /// Builds the RHS for `x op= v` as `x op v` (plain `=` passes `v`
    /// through).
    fn compound_rhs(
        &mut self,
        op: AssignOp,
        target_ty: Ty,
        target: LocalId,
        value: &ast::Expr,
        span: Span,
    ) -> TExpr {
        let bin_op = match op {
            AssignOp::Assign => return self.expr(value),
            AssignOp::Add => BinaryOp::Add,
            AssignOp::Sub => BinaryOp::Sub,
            AssignOp::Mul => BinaryOp::Mul,
            AssignOp::Div => BinaryOp::Div,
            AssignOp::Rem => BinaryOp::Rem,
            AssignOp::Shl => BinaryOp::Shl,
            AssignOp::Shr => BinaryOp::Shr,
            AssignOp::Ushr => BinaryOp::Ushr,
            AssignOp::BitAnd => BinaryOp::BitAnd,
            AssignOp::BitOr => BinaryOp::BitOr,
            AssignOp::BitXor => BinaryOp::BitXor,
            AssignOp::LogAnd => BinaryOp::LogAnd,
            AssignOp::LogOr => BinaryOp::LogOr,
        };
        let current = mk(target_ty, span, TExprKind::LocalGet(target));
        let rhs = self.expr(value);
        self.binary_typed(bin_op, current, rhs, span)
    }

    // --- calls ------------------------------------------------------------------

    fn call(&mut self, callee: &ast::Expr, args: &[ast::Expr], span: Span) -> TExpr {
        // `super(...)` constructor chain.
        if matches!(callee.kind, ExprKind::Super) {
            return self.super_call(None, args, span);
        }
        // Direct references get checked calls; everything else is an
        // indirect `Function`/`*` call (unchecked, returns `*` — AS3's
        // Function carries no signature).
        if let ExprKind::Ident(name) = &callee.kind {
            match self.lookup(name) {
                Some(Symbol::Fn(id)) => {
                    let checked = self.check_args_fn(id, args, span);
                    let ret = self.fn_return(id);
                    return mk(ret, span, TExprKind::CallFn(id, checked));
                }
                Some(Symbol::Builtin(b)) => {
                    let sig = b.signature();
                    let checked = self.check_args(&sig, b.name(), args, span);
                    return mk(sig.ret, span, TExprKind::CallBuiltin(b, checked));
                }
                Some(_) => {}
                None => {
                    // Unqualified method/static call in class context.
                    if let Some(result) = self.implicit_method_call(name, args, span) {
                        return result;
                    }
                    // `Type(expr)` cast (ES4 draft: calling a type converts).
                    if let Some(target) = self.ident_as_type(name) {
                        return self.cast_call(target, args, span);
                    }
                }
            }
        }
        if let ExprKind::Member(object, method) = &callee.kind {
            // `super.m(...)` — statically bound.
            if matches!(object.kind, ExprKind::Super) {
                return self.super_call(Some(method), args, span);
            }
            // Static method call: unshadowed class name receiver.
            if let ExprKind::Ident(recv) = &object.kind {
                if !self.is_shadowed(recv) {
                    if let Some(class) = self.ident_as_class(recv) {
                        return self.static_call(class, method, args, span);
                    }
                }
            }
        }
        if let ExprKind::Member(object, method) = &callee.kind {
            let object = self.expr(object);
            self.check_null_deref(&object, span);
            // Collections dispatch.
            if object.ty == Ty::Array {
                return self.array_method_call(object, method, args, span);
            }
            if let Ty::Vector(inst) = object.ty {
                return self.vector_method_call(object, inst, method, args, span);
            }
            // Class / interface dispatch.
            if let Ty::Class(class) = object.ty {
                return self.class_method_call(object, class, method, args, span);
            }
            if let Ty::Iface(iface) = object.ty {
                return self.iface_method_call(object, iface, method, args, span);
            }
            if object.ty != Ty::Any && object.ty != Ty::Error {
                return match builtins::member(object.ty, method) {
                    Some(Member::Method(sig)) => {
                        let checked = self.check_args(&sig, method, args, span);
                        let ret = sig.ret;
                        mk(
                            ret,
                            span,
                            TExprKind::CallMethod(Box::new(object), method.clone(), checked),
                        )
                    }
                    Some(Member::Property(ty)) => {
                        if ty == Ty::Function || ty == Ty::Any {
                            let args = self.args_to_any(args);
                            let receiver = mk(
                                ty,
                                span,
                                TExprKind::Member(Box::new(object), method.clone()),
                            );
                            mk(
                                Ty::Any,
                                span,
                                TExprKind::CallIndirect(Box::new(receiver), args),
                            )
                        } else {
                            self.error(
                                ErrorCode::NOT_CALLABLE,
                                format!("property `{method}` of type `{ty}` is not callable"),
                                span,
                            );
                            self.error_expr(span)
                        }
                    }
                    None => {
                        self.error(
                            ErrorCode::UNKNOWN_PROPERTY,
                            format!("no method `{method}` on sealed type `{}`", object.ty),
                            span,
                        );
                        self.error_expr(span)
                    }
                };
            }
            // Dynamic receiver: method call through `*`.
            let args = self.args_to_any(args);
            return mk(
                Ty::Any,
                span,
                TExprKind::CallMethod(Box::new(object), method.clone(), args),
            );
        }
        let callee_checked = self.expr(callee);
        match callee_checked.ty {
            Ty::Function | Ty::Any | Ty::Error => {
                let args = self.args_to_any(args);
                mk(
                    Ty::Any,
                    span,
                    TExprKind::CallIndirect(Box::new(callee_checked), args),
                )
            }
            ty => {
                self.error(
                    ErrorCode::NOT_CALLABLE,
                    format!("value of type `{ty}` is not callable"),
                    span,
                );
                self.error_expr(span)
            }
        }
    }

    fn args_to_any(&mut self, args: &[ast::Expr]) -> Vec<TExpr> {
        args.iter()
            .map(|a| {
                let checked = self.expr(a);
                self.coerce_to_any(checked)
            })
            .collect()
    }

    fn check_args(
        &mut self,
        sig: &Signature,
        name: &str,
        args: &[ast::Expr],
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
                        self.check_null_flow(&coerced, ty, false, "parameter", a.span);
                        coerced
                    }
                    None => self.coerce_to_any(checked), // variadic tail
                }
            })
            .collect()
    }

    fn check_args_fn(&mut self, id: FnId, args: &[ast::Expr], span: Span) -> Vec<TExpr> {
        let (params, required, variadic, name) = self.fn_sig_parts(id);
        self.arity(&name, args.len(), required, params.len(), variadic, span);
        args.iter()
            .enumerate()
            .map(|(i, a)| {
                let checked = self.expr(a);
                match params.get(i) {
                    Some(&ty) => {
                        let coerced = self.coerce(checked, ty, a.span);
                        let nullable = self.functions[id.0 as usize]
                            .locals
                            .get(i)
                            .is_some_and(|l| l.nullable);
                        self.check_null_flow(&coerced, ty, nullable, "parameter", a.span);
                        coerced
                    }
                    None => self.coerce_to_any(checked),
                }
            })
            .collect()
    }

    pub(super) fn arity(
        &mut self,
        name: &str,
        given: usize,
        required: usize,
        max: usize,
        variadic: bool,
        span: Span,
    ) {
        if given < required {
            self.error(
                ErrorCode::WRONG_ARG_COUNT,
                format!("`{name}` needs at least {required} argument(s), got {given}"),
                span,
            );
        } else if !variadic && given > max {
            self.error(
                ErrorCode::WRONG_ARG_COUNT,
                format!("`{name}` takes at most {max} argument(s), got {given}"),
                span,
            );
        }
    }

    // --- members ----------------------------------------------------------------

    fn member_read(&mut self, object: TExpr, name: &str, span: Span) -> TExpr {
        self.check_null_deref(&object, span);
        match object.ty {
            Ty::Class(class) => return self.class_member_read(object, class, name, span),
            Ty::Iface(iface) => return self.iface_member_read(object, iface, name, span),
            Ty::Array => return self.array_member_read(object, name, span),
            Ty::Vector(inst) => return self.vector_member_read(object, inst, name, span),
            _ => {}
        }
        match object.ty {
            Ty::Any | Ty::Error => {
                let ty = object.ty;
                mk(
                    if ty == Ty::Error { Ty::Error } else { Ty::Any },
                    span,
                    TExprKind::Member(Box::new(object), name.to_string()),
                )
            }
            Ty::Null | Ty::Void => {
                self.error(
                    ErrorCode::UNKNOWN_PROPERTY,
                    format!("cannot read property `{name}` of `{}`", object.ty),
                    span,
                );
                self.error_expr(span)
            }
            receiver => match builtins::member(receiver, name) {
                Some(Member::Property(ty)) => mk(
                    ty,
                    span,
                    TExprKind::Member(Box::new(object), name.to_string()),
                ),
                // Method extracted as a value: a bound method closure
                // (SPECS §3.7) — typed Function.
                Some(Member::Method(_)) => mk(
                    Ty::Function,
                    span,
                    TExprKind::Member(Box::new(object), name.to_string()),
                ),
                None => {
                    // Sealed: unknown members are compile errors (SPECS §3.2).
                    self.error(
                        ErrorCode::UNKNOWN_PROPERTY,
                        format!("no property `{name}` on sealed type `{receiver}`"),
                        span,
                    );
                    self.error_expr(span)
                }
            },
        }
    }

    fn index_read(&mut self, object: TExpr, index: TExpr, span: Span) -> TExpr {
        self.check_null_deref(&object, span);
        if matches!(object.ty, Ty::Array | Ty::Vector(_)) {
            return self.seq_index_read(object, index, span);
        }
        if object.ty != Ty::Any && object.ty != Ty::Error {
            self.error(
                ErrorCode::UNKNOWN_PROPERTY,
                format!(
                    "cannot index sealed type `{}` (dynamic access needs `*`)",
                    object.ty
                ),
                span,
            );
            return self.error_expr(span);
        }
        let index = self.coerce_to_any(index);
        mk(
            Ty::Any,
            span,
            TExprKind::Index(Box::new(object), Box::new(index)),
        )
    }

    fn function_expr(&mut self, f: &ast::FunctionDecl, span: Span) -> TExpr {
        let name = f.name.clone().unwrap_or_else(|| "<anonymous>".into());
        let ret = f
            .return_type
            .as_ref()
            .map(|t| self.resolve_type_allow_void(t))
            .unwrap_or(Ty::Any);
        let id = self.new_function(&name, ret, span);
        self.register_signature(id, &f.params);
        self.enter_function(f, id);
        mk(Ty::Function, span, TExprKind::FnRef(id))
    }

    pub(crate) fn coerce_to_any(&mut self, e: TExpr) -> TExpr {
        if e.ty == Ty::Any || e.ty == Ty::Error || e.ty == Ty::Null {
            return e;
        }
        if e.ty == Ty::Void {
            let span = e.span;
            return self.coerce(e, Ty::Any, span); // reports E0309
        }
        TExpr {
            ty: Ty::Any,
            span: e.span,
            kind: TExprKind::Coerce(Coercion::ToAny, Box::new(e)),
        }
    }
}

fn mk(ty: Ty, span: Span, kind: TExprKind) -> TExpr {
    TExpr { ty, span, kind }
}

/// The verifier's join of two branch types: identical stays, numeric pairs
/// widen to Number, object pairs join at Object, anything else erases to
/// `*` (frame-merge behavior).
fn merge_types(a: Ty, b: Ty) -> Ty {
    if a == b {
        return a;
    }
    if a == Ty::Error || b == Ty::Error {
        return Ty::Error;
    }
    if a.is_numeric() && b.is_numeric() {
        return Ty::Number;
    }
    // A precise least-common-ancestor would be better; Object is always
    // sound for two object-like values (null included).
    let object_like = |t: Ty| matches!(t, Ty::Class(_) | Ty::Iface(_) | Ty::Null);
    if object_like(a) && object_like(b) {
        return Ty::Class(crate::classes::OBJECT);
    }
    Ty::Any
}
