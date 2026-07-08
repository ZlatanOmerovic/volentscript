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
    let mut lo = Lowerer {
        program,
        diagnostics: Vec::new(),
    };
    let functions = program
        .functions
        .iter()
        .map(|f| lo.function(f))
        .collect::<Vec<_>>();
    if lo.diagnostics.is_empty() {
        Ok(Program { functions })
    } else {
        lo.diagnostics
            .sort_by_key(|d| d.span.map(|s| (s.start, s.end)));
        Err(lo.diagnostics)
    }
}

struct Lowerer<'a> {
    program: &'a sema::TProgram,
    diagnostics: Vec<Diagnostic>,
}

impl Lowerer<'_> {
    fn function(&mut self, f: &sema::TFunction) -> Function {
        let locals: Vec<Ty> = f.locals.iter().map(|l| self.ty(l.ty, f.span)).collect();
        let param_defaults = f
            .locals
            .iter()
            .take(f.param_count)
            .map(|l| {
                if l.is_rest {
                    self.gate(f.span, "rest parameters need Array — Phase 5");
                    None
                } else {
                    l.default.as_ref().map(|d| self.expr(d))
                }
            })
            .collect();
        Function {
            name: f.name.clone(),
            ret: self.ty(f.return_ty, f.span),
            locals,
            param_count: f.param_count,
            param_defaults,
            body: f.body.iter().map(|s| self.stmt(s)).collect(),
            span: f.span,
        }
    }

    fn ty(&mut self, ty: sema::Ty, span: Span) -> Ty {
        match ty {
            sema::Ty::Int => Ty::Int,
            sema::Ty::UInt => Ty::UInt,
            sema::Ty::Number => Ty::Number,
            sema::Ty::Boolean => Ty::Boolean,
            // A bare `null` only reaches MIR in String context (other
            // reference types are later phases); it is the null String.
            sema::Ty::String | sema::Ty::Null => Ty::String,
            sema::Ty::Void => Ty::Void,
            sema::Ty::Any => Ty::Any,
            sema::Ty::Function => {
                self.gate(span, "Function values need closures — Phase 6");
                Ty::Any
            }
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
            S::ForIn { object, .. } => {
                self.gate(object.span, "`for..in`/`for each..in` iteration — Phase 6");
                Stmt::Empty
            }
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
            S::Throw { value } => {
                self.gate(value.span, "`throw`/`try` exceptions — Phase 6");
                Stmt::Empty
            }
            S::Try { .. } => {
                self.gate(stmt.span, "`try`/`catch`/`finally` exceptions — Phase 6");
                Stmt::Empty
            }
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
            E::Bool(v) => ExprKind::Bool(*v),
            E::Null => ExprKind::Null,
            E::Undefined => ExprKind::Undefined,
            E::LocalGet(id) => ExprKind::LocalGet(LocalId(id.0)),
            E::LocalSet(id, v) => ExprKind::LocalSet(LocalId(id.0), Box::new(self.expr(v))),
            E::CallFn(id, args) => {
                let mut lowered: Vec<Expr> = args.iter().map(|a| self.expr(a)).collect();
                // Fill omitted defaulted arguments at the callsite (AVM2
                // fills from the method's option list; same effect).
                let callee = &self.program.functions[id.0 as usize];
                for i in lowered.len()..callee.param_count {
                    if let Some(default) = &callee.locals[i].default {
                        lowered.push(self.expr(default));
                    }
                }
                ExprKind::CallFn(FnId(id.0), lowered)
            }
            E::CallBuiltin(b, args) => self.builtin_call(*b, args, span),
            E::CallMethod(receiver, name, args) => {
                return self.method_call(receiver, name, args, ty, span);
            }
            E::Member(receiver, name) => {
                if receiver.ty == sema::Ty::String && name == "length" {
                    ExprKind::StrLen(Box::new(self.expr(receiver)))
                } else {
                    return self.gated_expr(
                        span,
                        ty,
                        "dynamic property access needs the object model — Phase 4",
                    );
                }
            }
            E::MemberSet(..) | E::IndexSet(..) | E::Index(..) => {
                return self.gated_expr(
                    span,
                    ty,
                    "dynamic property access needs the object model — Phase 4",
                );
            }
            E::Array(_) => {
                return self.gated_expr(span, ty, "Array literals need the Array class — Phase 5");
            }
            E::Object(_) => {
                return self.gated_expr(
                    span,
                    ty,
                    "object literals need the object model — Phase 4",
                );
            }
            E::FnRef(_) | E::BuiltinRef(_) => {
                return self.gated_expr(span, ty, "functions as values need closures — Phase 6");
            }
            E::CallIndirect(..) => {
                return self.gated_expr(
                    span,
                    ty,
                    "calls through Function/`*` values need closures — Phase 6",
                );
            }
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
                        _ => {
                            return self.gated_expr(
                                span,
                                ty,
                                "checked `*` coercion to this type — Phase 4",
                            );
                        }
                    },
                };
                ExprKind::Conv(conv, Box::new(self.expr(v)))
            }
            E::Comma(l, r) => ExprKind::Comma(Box::new(self.expr(l)), Box::new(self.expr(r))),
            E::Error => unreachable!("error expr survived sema"),
        };
        Expr { ty, span, kind }
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
        self.gated_expr(
            span,
            ty,
            "++/-- on non-local or non-numeric targets — Phase 4",
        )
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
                        return self.gated_expr(span, ty, "`split` returns Array — Phase 5");
                    }
                    _ => return self.gated_expr(span, ty, "this String method — Phase 7"),
                };
                Expr {
                    ty,
                    span,
                    kind: ExprKind::CallStrMethod(method, Box::new(self.expr(receiver)), lowered),
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
            _ => self.gated_expr(
                span,
                ty,
                "dynamic method calls need the object model — Phase 4",
            ),
        }
    }
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
