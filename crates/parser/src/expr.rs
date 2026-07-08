//! Expression productions.
//!
//! Precedence ladder exactly as avmplus implements it
//! (eval-parse-expr.cpp:244-770), tightest to loosest: primary → postfix →
//! unary → multiplicative → additive → shift → relational (incl. `is` `as`
//! `in` `instanceof`) → equality → `&` → `^` → `|` → `&&` → `||` → `?:` →
//! assignment → comma. No `??` tier: AS3 has none.

use ast::*;
use diagnostics::ErrorCode;
use lexer::TokenKind;
use span::Span;

use crate::Parser;

impl Parser {
    /// Full expression including the comma operator.
    pub(crate) fn expression(&mut self) -> Expr {
        let first = self.assignment_expression();
        if !self.at(&TokenKind::Comma) {
            return first;
        }
        let mut expr = first;
        while self.eat(&TokenKind::Comma) {
            let rhs = self.assignment_expression();
            let span = expr.span.to(rhs.span);
            expr = Expr {
                kind: ExprKind::Comma(Box::new(expr), Box::new(rhs)),
                span,
            };
        }
        expr
    }

    /// Assignment tier: right-associative, validates the target
    /// (avmplus assignmentExpression, eval-parse-expr.cpp:740).
    pub(crate) fn assignment_expression(&mut self) -> Expr {
        let lhs = self.conditional_expression();
        let Some(op) = assign_op(&self.current().kind) else {
            return lhs;
        };
        self.advance();
        self.check_assign_target(&lhs);
        let rhs = self.assignment_expression();
        let span = lhs.span.to(rhs.span);
        Expr {
            kind: ExprKind::Assign(op, Box::new(lhs), Box::new(rhs)),
            span,
        }
    }

    pub(crate) fn check_assign_target(&mut self, target: &Expr) {
        if !matches!(
            target.kind,
            ExprKind::Ident(_) | ExprKind::Member(..) | ExprKind::Index(..)
        ) {
            self.error(
                ErrorCode::INVALID_ASSIGN_TARGET,
                "left side of assignment must be a variable, property, or index",
                target.span,
            );
        }
    }

    fn conditional_expression(&mut self) -> Expr {
        let cond = self.binary_expression(0);
        if !self.eat(&TokenKind::Question) {
            return cond;
        }
        let then_value = self.assignment_expression();
        self.expect(&TokenKind::Colon);
        let else_value = self.assignment_expression();
        let span = cond.span.to(else_value.span);
        Expr {
            kind: ExprKind::Conditional(Box::new(cond), Box::new(then_value), Box::new(else_value)),
            span,
        }
    }

    /// Precedence climbing over the binary tiers.
    fn binary_expression(&mut self, min_level: u8) -> Expr {
        let mut lhs = self.unary_expression();
        loop {
            let Some((op, level)) = binary_op(&self.current().kind) else {
                return lhs;
            };
            // `in` suppressed inside `for` heads (avmplus EFLAG_NoIn).
            if op == BinaryOp::In && self.no_in {
                return lhs;
            }
            if level < min_level {
                return lhs;
            }
            self.advance();
            // For `is`/`as` the right operand is a type name, but it is
            // parsed as an ordinary expression and resolved in sema —
            // matching avmplus, whose relationalExpression takes a plain
            // shiftExpression operand (eval-parse-expr.cpp:638).
            let rhs = self.binary_expression(level + 1);
            let span = lhs.span.to(rhs.span);
            lhs = Expr {
                kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                span,
            };
        }
    }

    fn unary_expression(&mut self) -> Expr {
        use TokenKind::*;
        let start = self.current().span;
        let op = match self.current().kind {
            Delete => Some(UnaryOp::Delete),
            Void => Some(UnaryOp::Void),
            Typeof => Some(UnaryOp::Typeof),
            PlusPlus => Some(UnaryOp::PreInc),
            MinusMinus => Some(UnaryOp::PreDec),
            Plus => Some(UnaryOp::Plus),
            Minus => Some(UnaryOp::Minus),
            BitNot => Some(UnaryOp::BitNot),
            Not => Some(UnaryOp::Not),
            _ => None,
        };
        if let Some(op) = op {
            self.advance();
            let operand = self.unary_expression();
            if matches!(op, UnaryOp::PreInc | UnaryOp::PreDec) {
                self.check_assign_target(&operand);
            }
            let span = start.to(operand.span);
            return Expr {
                kind: ExprKind::Unary(op, Box::new(operand)),
                span,
            };
        }
        self.postfix_expression()
    }

    fn postfix_expression(&mut self) -> Expr {
        let expr = self.call_expression();
        // Postfix ++/-- must be on the same line as the operand
        // (ECMA-262 3rd ed. §7.9.1; avmplus eval-parse-expr.cpp:549).
        let op = match self.current().kind {
            TokenKind::PlusPlus if !self.current().newline_before => PostfixOp::Inc,
            TokenKind::MinusMinus if !self.current().newline_before => PostfixOp::Dec,
            _ => return expr,
        };
        self.check_assign_target(&expr);
        let end = self.advance().span;
        let span = expr.span.to(end);
        Expr {
            kind: ExprKind::Postfix(op, Box::new(expr)),
            span,
        }
    }

    /// Member/call tier: `new`, `.name`, `[index]`, `(args)` chains
    /// (ES3 §11.2 MemberExpression/CallExpression/NewExpression).
    fn call_expression(&mut self) -> Expr {
        let mut expr = if self.at(&TokenKind::New) {
            self.new_expression()
        } else {
            self.primary_expression()
        };
        loop {
            expr = match &self.current().kind {
                TokenKind::Dot => {
                    self.advance();
                    let (name, name_span) = self.expect_ident();
                    let span = expr.span.to(name_span);
                    Expr {
                        kind: ExprKind::Member(Box::new(expr), name),
                        span,
                    }
                }
                TokenKind::LBracket => {
                    self.advance();
                    let index = self.expression();
                    let end = self.expect(&TokenKind::RBracket);
                    let span = expr.span.to(end);
                    Expr {
                        kind: ExprKind::Index(Box::new(expr), Box::new(index)),
                        span,
                    }
                }
                TokenKind::LParen => {
                    let (args, end) = self.arguments();
                    let span = expr.span.to(end);
                    Expr {
                        kind: ExprKind::Call(Box::new(expr), args),
                        span,
                    }
                }
                TokenKind::LeftDotAngle => {
                    self.advance();
                    let mut args = vec![self.type_ref()];
                    while self.eat(&TokenKind::Comma) {
                        args.push(self.type_ref());
                    }
                    self.expect_type_close();
                    let span = expr.span.to(self.tokens[self.pos - 1].span);
                    Expr {
                        kind: ExprKind::ApplyType(Box::new(expr), args),
                        span,
                    }
                }
                TokenKind::DotDot => {
                    let token = self.advance();
                    self.error(
                        ErrorCode::UNSUPPORTED_SYNTAX,
                        "`..` is the E4X descendant operator, which is not part of this language (SPECS §5)",
                        token.span,
                    );
                    expr
                }
                _ => return expr,
            };
        }
    }

    /// `new Callee(args)` with member chains binding tighter than the call:
    /// `new a.b.C(x)` constructs `a.b.C` (ES3 §11.2.2).
    fn new_expression(&mut self) -> Expr {
        let start = self.advance().span; // `new`
        // `new <T>[...]` Vector literal (SPECS §4.3; avmplus
        // vectorInitializer, eval-parse-expr.cpp:214).
        if self.at(&TokenKind::Lt) {
            self.advance();
            let elem = self.type_ref();
            self.expect_type_close();
            self.expect(&TokenKind::LBracket);
            let mut elements = Vec::new();
            if !self.at(&TokenKind::RBracket) {
                loop {
                    elements.push(self.assignment_expression());
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                    if self.at(&TokenKind::RBracket) {
                        break;
                    }
                }
            }
            let end = self.expect(&TokenKind::RBracket);
            return Expr {
                kind: ExprKind::VectorLit { elem, elements },
                span: start.to(end),
            };
        }
        let mut callee = if self.at(&TokenKind::New) {
            self.new_expression()
        } else {
            self.primary_expression()
        };
        loop {
            callee = match &self.current().kind {
                TokenKind::LeftDotAngle => {
                    self.advance();
                    let mut targs = vec![self.type_ref()];
                    while self.eat(&TokenKind::Comma) {
                        targs.push(self.type_ref());
                    }
                    self.expect_type_close();
                    let span = callee.span.to(self.tokens[self.pos - 1].span);
                    Expr {
                        kind: ExprKind::ApplyType(Box::new(callee), targs),
                        span,
                    }
                }
                TokenKind::Dot => {
                    self.advance();
                    let (name, name_span) = self.expect_ident();
                    let span = callee.span.to(name_span);
                    Expr {
                        kind: ExprKind::Member(Box::new(callee), name),
                        span,
                    }
                }
                TokenKind::LBracket => {
                    self.advance();
                    let index = self.expression();
                    let end = self.expect(&TokenKind::RBracket);
                    let span = callee.span.to(end);
                    Expr {
                        kind: ExprKind::Index(Box::new(callee), Box::new(index)),
                        span,
                    }
                }
                _ => break,
            };
        }
        let (args, end) = if self.at(&TokenKind::LParen) {
            self.arguments()
        } else {
            (Vec::new(), callee.span)
        };
        let span = start.to(end);
        Expr {
            kind: ExprKind::New(Box::new(callee), args),
            span,
        }
    }

    fn arguments(&mut self) -> (Vec<Expr>, Span) {
        self.expect(&TokenKind::LParen);
        let mut args = Vec::new();
        if !self.at(&TokenKind::RParen) {
            loop {
                args.push(self.assignment_expression());
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
        }
        let end = self.expect(&TokenKind::RParen);
        (args, end)
    }

    fn primary_expression(&mut self) -> Expr {
        use TokenKind::*;
        let token = self.current().clone();
        let span = token.span;
        let kind = match token.kind {
            Int(v) => {
                self.advance();
                ExprKind::Int(v)
            }
            UInt(v) => {
                self.advance();
                ExprKind::UInt(v)
            }
            Number(v) => {
                self.advance();
                ExprKind::Number(v)
            }
            Str(ref v) => {
                let v = v.clone();
                self.advance();
                ExprKind::Str(v)
            }
            True => {
                self.advance();
                ExprKind::Bool(true)
            }
            False => {
                self.advance();
                ExprKind::Bool(false)
            }
            Null => {
                self.advance();
                ExprKind::Null
            }
            This => {
                self.advance();
                ExprKind::This
            }
            Super => {
                self.advance();
                ExprKind::Super
            }
            Ident(ref name) => {
                let name = name.clone();
                self.advance();
                ExprKind::Ident(name)
            }
            LParen => {
                self.advance();
                let inner = self.expression();
                self.expect(&RParen);
                // Span covers the parens; grouping itself is not a node.
                return Expr {
                    kind: inner.kind,
                    span: span.to(self.tokens[self.pos - 1].span),
                };
            }
            LBracket => return self.array_literal(),
            LBrace => return self.object_literal(),
            Function => {
                let start = self.advance().span;
                let f = self.function_after_keyword(start, false);
                let span = f.span;
                return Expr {
                    kind: ExprKind::Function(Box::new(f)),
                    span,
                };
            }
            _ => {
                self.error(
                    ErrorCode::UNEXPECTED_TOKEN,
                    format!("expected expression, found {}", token.kind.describe()),
                    span,
                );
                // Do not consume: the statement loop / recovery decides.
                ExprKind::Ident(String::from("<error>"))
            }
        };
        Expr { kind, span }
    }

    fn array_literal(&mut self) -> Expr {
        let start = self.expect(&TokenKind::LBracket);
        let mut elements = Vec::new();
        loop {
            if self.at(&TokenKind::RBracket) {
                break;
            }
            if self.eat(&TokenKind::Comma) {
                // Elision: `[a, , b]` (ES3 §11.1.4 sparse arrays).
                elements.push(None);
                continue;
            }
            elements.push(Some(self.assignment_expression()));
            if !self.eat(&TokenKind::Comma) {
                break;
            }
            if self.at(&TokenKind::RBracket) {
                // Trailing comma adds no element.
                break;
            }
            // Next loop iteration handles the element (or further elisions).
            if self.at(&TokenKind::Comma) {
                continue;
            }
        }
        let end = self.expect(&TokenKind::RBracket);
        Expr {
            kind: ExprKind::Array(elements),
            span: start.to(end),
        }
    }

    fn object_literal(&mut self) -> Expr {
        let start = self.expect(&TokenKind::LBrace);
        let mut props = Vec::new();
        if !self.at(&TokenKind::RBrace) {
            loop {
                let name = match &self.current().kind {
                    TokenKind::Ident(name) => {
                        let name = name.clone();
                        self.advance();
                        PropName::Ident(name)
                    }
                    TokenKind::Str(value) => {
                        let value = value.clone();
                        self.advance();
                        PropName::Str(value)
                    }
                    TokenKind::Int(v) => {
                        let v = *v;
                        self.advance();
                        PropName::Number(f64::from(v))
                    }
                    TokenKind::UInt(v) => {
                        let v = *v;
                        self.advance();
                        PropName::Number(f64::from(v))
                    }
                    TokenKind::Number(v) => {
                        let v = *v;
                        self.advance();
                        PropName::Number(v)
                    }
                    other => {
                        let span = self.current().span;
                        self.error(
                            ErrorCode::UNEXPECTED_TOKEN,
                            format!("expected property name, found {}", other.describe()),
                            span,
                        );
                        break;
                    }
                };
                self.expect(&TokenKind::Colon);
                let value = self.assignment_expression();
                props.push((name, value));
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
                if self.at(&TokenKind::RBrace) {
                    break;
                }
            }
        }
        let end = self.expect(&TokenKind::RBrace);
        Expr {
            kind: ExprKind::Object(props),
            span: start.to(end),
        }
    }
}

fn assign_op(kind: &TokenKind) -> Option<AssignOp> {
    use TokenKind::*;
    Some(match kind {
        Assign => AssignOp::Assign,
        PlusAssign => AssignOp::Add,
        MinusAssign => AssignOp::Sub,
        StarAssign => AssignOp::Mul,
        SlashAssign => AssignOp::Div,
        PercentAssign => AssignOp::Rem,
        ShlAssign => AssignOp::Shl,
        ShrAssign => AssignOp::Shr,
        UshrAssign => AssignOp::Ushr,
        BitAndAssign => AssignOp::BitAnd,
        BitOrAssign => AssignOp::BitOr,
        BitXorAssign => AssignOp::BitXor,
        LogAndAssign => AssignOp::LogAnd,
        LogOrAssign => AssignOp::LogOr,
        _ => return None,
    })
}

/// Binary tiers, loosest = 0 (`||`) … tightest = 9 (`* / %`), per avmplus
/// eval-parse-expr.cpp:586-714.
fn binary_op(kind: &TokenKind) -> Option<(BinaryOp, u8)> {
    use TokenKind::*;
    Some(match kind {
        LogOr => (BinaryOp::LogOr, 0),
        LogAnd => (BinaryOp::LogAnd, 1),
        BitOr => (BinaryOp::BitOr, 2),
        BitXor => (BinaryOp::BitXor, 3),
        BitAnd => (BinaryOp::BitAnd, 4),
        Eq => (BinaryOp::Eq, 5),
        Ne => (BinaryOp::Ne, 5),
        StrictEq => (BinaryOp::StrictEq, 5),
        StrictNe => (BinaryOp::StrictNe, 5),
        Lt => (BinaryOp::Lt, 6),
        Gt => (BinaryOp::Gt, 6),
        Le => (BinaryOp::Le, 6),
        Ge => (BinaryOp::Ge, 6),
        In => (BinaryOp::In, 6),
        Instanceof => (BinaryOp::Instanceof, 6),
        Is => (BinaryOp::Is, 6),
        As => (BinaryOp::As, 6),
        Shl => (BinaryOp::Shl, 7),
        Shr => (BinaryOp::Shr, 7),
        Ushr => (BinaryOp::Ushr, 7),
        Plus => (BinaryOp::Add, 8),
        Minus => (BinaryOp::Sub, 8),
        Star => (BinaryOp::Mul, 9),
        Slash => (BinaryOp::Div, 9),
        Percent => (BinaryOp::Rem, 9),
        _ => return None,
    })
}
