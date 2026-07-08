//! Statement and declaration productions
//! (avmplus eval-parse-stmt.cpp / eval-parse.cpp).

use ast::*;
use diagnostics::ErrorCode;
use lexer::TokenKind;
use span::Span;

use crate::Parser;

impl Parser {
    pub(crate) fn statement(&mut self) -> Stmt {
        use TokenKind::*;
        let start = self.current().span;
        match &self.current().kind {
            LBrace => {
                let block = self.block();
                let span = block.span;
                Stmt {
                    kind: StmtKind::Block(block),
                    span,
                }
            }
            Semicolon => {
                self.advance();
                Stmt {
                    kind: StmtKind::Empty,
                    span: start,
                }
            }
            Var | Const => self.var_statement(),
            Function => self.function_declaration(),
            If => self.if_statement(),
            While => self.while_statement(),
            Do => self.do_while_statement(),
            For => self.for_statement(),
            Switch => self.switch_statement(),
            Break | Continue => self.break_or_continue(),
            Return => self.return_statement(),
            Throw => self.throw_statement(),
            Try => self.try_statement(),
            // Later-phase constructs: recognizable, honest diagnostic.
            Class | Interface | Package | Import => {
                let token = self.advance();
                self.error(
                    ErrorCode::NOT_IMPLEMENTED,
                    format!("{} is not implemented until Phase 4", token.kind.describe()),
                    token.span,
                );
                self.recover_to_statement_boundary();
                Stmt {
                    kind: StmtKind::Empty,
                    span: token.span,
                }
            }
            Goto => {
                // Reserved word, no semantics (avmplus reserves it too,
                // generate-keyword-lexer.as:34).
                let token = self.advance();
                self.error(
                    ErrorCode::UNSUPPORTED_SYNTAX,
                    "`goto` is a reserved word with no meaning in AS3",
                    token.span,
                );
                self.recover_to_statement_boundary();
                Stmt {
                    kind: StmtKind::Empty,
                    span: token.span,
                }
            }
            // `label: stmt` — identifier directly followed by `:`.
            Ident(_) if matches!(self.peek_kind(1), Colon) => {
                let (label, label_span) = self.expect_ident();
                self.advance(); // `:`
                let body = self.statement();
                let span = label_span.to(body.span);
                Stmt {
                    kind: StmtKind::Labeled {
                        label,
                        body: Box::new(body),
                    },
                    span,
                }
            }
            _ => self.expression_statement(),
        }
    }

    fn expression_statement(&mut self) -> Stmt {
        let expr = self.expression();
        self.semicolon();
        let span = expr.span;
        Stmt {
            kind: StmtKind::Expr(expr),
            span,
        }
    }

    /// Skips to the next plausible statement start after a hard error.
    fn recover_to_statement_boundary(&mut self) {
        use TokenKind::*;
        loop {
            match self.current().kind {
                Eof | Semicolon | RBrace => {
                    self.eat(&Semicolon);
                    return;
                }
                Var | Const | Function | If | While | Do | For | Switch | Break | Continue
                | Return | Throw | Try | LBrace => return,
                _ => {
                    self.advance();
                }
            }
        }
    }

    pub(crate) fn block(&mut self) -> Block {
        let start = self.expect(&TokenKind::LBrace);
        let mut stmts = Vec::new();
        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
            let before = self.pos;
            stmts.push(self.statement());
            if self.pos == before {
                self.advance();
            }
        }
        let end = self.expect(&TokenKind::RBrace);
        Block {
            stmts,
            span: start.to(end),
        }
    }

    // --- var / const --------------------------------------------------------

    fn var_statement(&mut self) -> Stmt {
        let decl = self.var_declaration();
        self.semicolon();
        let span = decl.span;
        Stmt {
            kind: StmtKind::VarDecl(decl),
            span,
        }
    }

    /// `var`/`const` and its bindings, without the terminating semicolon
    /// (shared with `for` heads).
    pub(crate) fn var_declaration(&mut self) -> VarDecl {
        let keyword = self.advance();
        let is_const = keyword.kind == TokenKind::Const;
        let mut bindings = vec![self.binding()];
        while self.eat(&TokenKind::Comma) {
            bindings.push(self.binding());
        }
        let span = keyword.span.to(bindings.last().expect("nonempty").span);
        VarDecl {
            is_const,
            bindings,
            span,
        }
    }

    fn binding(&mut self) -> Binding {
        let (name, name_span) = self.expect_ident();
        let ty = if self.eat(&TokenKind::Colon) {
            Some(self.type_ref())
        } else {
            None
        };
        let init = if self.eat(&TokenKind::Assign) {
            Some(self.assignment_expression())
        } else {
            None
        };
        let end = init
            .as_ref()
            .map(|e| e.span)
            .or(ty.as_ref().map(|t| t.span))
            .unwrap_or(name_span);
        Binding {
            name,
            ty,
            init,
            span: name_span.to(end),
        }
    }

    // --- functions ---------------------------------------------------------

    fn function_declaration(&mut self) -> Stmt {
        let keyword_span = self.advance().span;
        let decl = self.function_after_keyword(keyword_span, true);
        let span = decl.span;
        Stmt {
            kind: StmtKind::Function(Box::new(decl)),
            span,
        }
    }

    /// Everything after the `function` keyword. `require_name` distinguishes
    /// declarations from (possibly anonymous) function expressions.
    pub(crate) fn function_after_keyword(
        &mut self,
        keyword_span: Span,
        require_name: bool,
    ) -> FunctionDecl {
        // `function get`/`function set` accessors are class-body syntax; the
        // contextual words are plain identifiers here and simply serve as
        // function names at top level (matching avmplus, which only treats
        // them specially when accessors are allowed — eval-parse.cpp:1128).
        let name = if matches!(self.current().kind, TokenKind::Ident(_)) {
            Some(self.expect_ident().0)
        } else {
            if require_name {
                let span = self.current().span;
                self.error(
                    ErrorCode::UNEXPECTED_TOKEN,
                    "function declaration needs a name",
                    span,
                );
            }
            None
        };
        self.expect(&TokenKind::LParen);
        let mut params = Vec::new();
        if !self.at(&TokenKind::RParen) {
            loop {
                params.push(self.parameter());
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect(&TokenKind::RParen);
        let return_type = if self.eat(&TokenKind::Colon) {
            Some(self.type_ref())
        } else {
            None
        };
        let body = self.block();
        let span = keyword_span.to(body.span);
        FunctionDecl {
            name,
            params,
            return_type,
            body,
            span,
        }
    }

    fn parameter(&mut self) -> Param {
        let start = self.current().span;
        // `...rest` — a type annotation is permitted after it (avmplus
        // eval-parse.cpp:1146).
        let rest = self.eat(&TokenKind::Ellipsis);
        let (name, name_span) = self.expect_ident();
        let ty = if self.eat(&TokenKind::Colon) {
            Some(self.type_ref())
        } else {
            None
        };
        let default = if self.eat(&TokenKind::Assign) {
            Some(self.assignment_expression())
        } else {
            None
        };
        let end = default
            .as_ref()
            .map(|e| e.span)
            .or(ty.as_ref().map(|t| t.span))
            .unwrap_or(name_span);
        Param {
            name,
            ty,
            default,
            rest,
            span: start.to(end),
        }
    }

    // --- types -----------------------------------------------------------------

    /// SPECS §9 `typeRef := qualifiedName ('.<' args '>')? '?'?` plus `*` and
    /// `void`. avmplus restricts annotation types to names/`*` the same way
    /// (typeExpression, eval-parse-expr.cpp:19).
    pub(crate) fn type_ref(&mut self) -> TypeRef {
        let start = self.current().span;
        let kind = if self.eat(&TokenKind::Star) {
            TypeRefKind::Any
        } else if self.eat(&TokenKind::Void) {
            TypeRefKind::Void
        } else {
            let (first, _) = self.expect_ident();
            let mut path = vec![first];
            while self.at(&TokenKind::Dot) && matches!(self.peek_kind(1), TokenKind::Ident(_)) {
                self.advance();
                path.push(self.expect_ident().0);
            }
            let mut type_args = Vec::new();
            if self.eat(&TokenKind::LeftDotAngle) {
                loop {
                    type_args.push(self.type_ref());
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect_type_close();
            }
            TypeRefKind::Name { path, type_args }
        };
        // `?` nullable suffix — our extension per SPECS §4.1/§9.
        let nullable = self.eat(&TokenKind::Question);
        let end = self.tokens[self.pos.saturating_sub(1)].span;
        TypeRef {
            kind,
            nullable,
            span: start.to(end),
        }
    }

    // --- control flow -------------------------------------------------------

    fn paren_expression(&mut self) -> Expr {
        self.expect(&TokenKind::LParen);
        let expr = self.expression();
        self.expect(&TokenKind::RParen);
        expr
    }

    fn if_statement(&mut self) -> Stmt {
        let start = self.advance().span;
        let cond = self.paren_expression();
        let then_branch = self.statement();
        let else_branch = if self.eat(&TokenKind::Else) {
            Some(Box::new(self.statement()))
        } else {
            None
        };
        let end = else_branch
            .as_ref()
            .map(|s| s.span)
            .unwrap_or(then_branch.span);
        Stmt {
            kind: StmtKind::If {
                cond,
                then_branch: Box::new(then_branch),
                else_branch,
            },
            span: start.to(end),
        }
    }

    fn while_statement(&mut self) -> Stmt {
        let start = self.advance().span;
        let cond = self.paren_expression();
        let body = self.statement();
        let span = start.to(body.span);
        Stmt {
            kind: StmtKind::While {
                cond,
                body: Box::new(body),
            },
            span,
        }
    }

    fn do_while_statement(&mut self) -> Stmt {
        let start = self.advance().span;
        let body = self.statement();
        self.expect(&TokenKind::While);
        let cond = self.paren_expression();
        self.semicolon();
        let span = start.to(cond.span);
        Stmt {
            kind: StmtKind::DoWhile {
                body: Box::new(body),
                cond,
            },
            span,
        }
    }

    /// `for (init; cond; update)`, `for (x in obj)`, `for each (x in obj)`.
    /// Structure mirrors avmplus forStatement (eval-parse-stmt.cpp:427).
    fn for_statement(&mut self) -> Stmt {
        let start = self.advance().span;
        // `each` is contextual: an identifier compared by value
        // (eval-parse-stmt.cpp:435).
        let is_each = if self.at_contextual("each") {
            self.advance();
            true
        } else {
            false
        };
        self.expect(&TokenKind::LParen);

        // Empty init: `for (; cond; update)`.
        if !is_each && self.at(&TokenKind::Semicolon) {
            self.advance();
            return self.c_style_for_tail(start, None);
        }

        // Parse the head with `in` suppressed so `x in obj` isn't swallowed
        // as a relational expression (avmplus EFLAG_NoIn).
        let head_is_var = self.at(&TokenKind::Var) || self.at(&TokenKind::Const);
        if head_is_var {
            let old = std::mem::replace(&mut self.no_in, true);
            let decl = self.var_declaration();
            self.no_in = old;
            if self.eat(&TokenKind::In) {
                return self.for_in_tail(start, is_each, ForInTarget::VarDecl(decl));
            }
            if is_each {
                self.error(
                    ErrorCode::FOR_EACH_REQUIRES_IN,
                    "`for each` requires an `in` loop",
                    start,
                );
            }
            self.expect(&TokenKind::Semicolon);
            self.c_style_for_tail(start, Some(Box::new(ForInit::VarDecl(decl))))
        } else {
            let old = std::mem::replace(&mut self.no_in, true);
            let expr = self.expression();
            self.no_in = old;
            if self.eat(&TokenKind::In) {
                self.check_assign_target(&expr);
                return self.for_in_tail(start, is_each, ForInTarget::Expr(expr));
            }
            if is_each {
                self.error(
                    ErrorCode::FOR_EACH_REQUIRES_IN,
                    "`for each` requires an `in` loop",
                    start,
                );
            }
            self.expect(&TokenKind::Semicolon);
            self.c_style_for_tail(start, Some(Box::new(ForInit::Expr(expr))))
        }
    }

    fn c_style_for_tail(&mut self, start: Span, init: Option<Box<ForInit>>) -> Stmt {
        let cond = if self.at(&TokenKind::Semicolon) {
            None
        } else {
            Some(self.expression())
        };
        self.expect(&TokenKind::Semicolon);
        let update = if self.at(&TokenKind::RParen) {
            None
        } else {
            Some(self.expression())
        };
        self.expect(&TokenKind::RParen);
        let body = self.statement();
        let span = start.to(body.span);
        Stmt {
            kind: StmtKind::For {
                init,
                cond,
                update,
                body: Box::new(body),
            },
            span,
        }
    }

    fn for_in_tail(&mut self, start: Span, is_each: bool, target: ForInTarget) -> Stmt {
        if let ForInTarget::VarDecl(decl) = &target {
            if decl.bindings.len() > 1 {
                self.error(
                    ErrorCode::UNEXPECTED_TOKEN,
                    "`for..in` allows exactly one loop variable",
                    decl.span,
                );
            }
        }
        let object = self.expression();
        self.expect(&TokenKind::RParen);
        let body = self.statement();
        let span = start.to(body.span);
        Stmt {
            kind: StmtKind::ForIn {
                is_each,
                target,
                object,
                body: Box::new(body),
            },
            span,
        }
    }

    fn switch_statement(&mut self) -> Stmt {
        let start = self.advance().span;
        let scrutinee = self.paren_expression();
        self.expect(&TokenKind::LBrace);
        let mut cases: Vec<SwitchCase> = Vec::new();
        let mut seen_default = false;
        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
            let case_start = self.current().span;
            let test = if self.eat(&TokenKind::Case) {
                let test = self.expression();
                self.expect(&TokenKind::Colon);
                Some(test)
            } else if self.at(&TokenKind::Default) {
                let span = self.advance().span;
                self.expect(&TokenKind::Colon);
                if seen_default {
                    self.error(
                        ErrorCode::UNEXPECTED_TOKEN,
                        "duplicate `default` clause in `switch`",
                        span,
                    );
                }
                seen_default = true;
                None
            } else {
                let span = self.current().span;
                self.error(
                    ErrorCode::UNEXPECTED_TOKEN,
                    format!(
                        "expected `case` or `default`, found {}",
                        self.current().kind.describe()
                    ),
                    span,
                );
                self.recover_to_statement_boundary();
                continue;
            };
            let mut body = Vec::new();
            while !matches!(
                self.current().kind,
                TokenKind::Case | TokenKind::Default | TokenKind::RBrace | TokenKind::Eof
            ) {
                let before = self.pos;
                body.push(self.statement());
                if self.pos == before {
                    self.advance();
                }
            }
            let end = body.last().map(|s| s.span).unwrap_or(case_start);
            cases.push(SwitchCase {
                test,
                body,
                span: case_start.to(end),
            });
        }
        let end = self.expect(&TokenKind::RBrace);
        Stmt {
            kind: StmtKind::Switch { scrutinee, cases },
            span: start.to(end),
        }
    }

    fn break_or_continue(&mut self) -> Stmt {
        let keyword = self.advance();
        let is_break = keyword.kind == TokenKind::Break;
        // Label only on the same line (avmplus breakOrContinueLabel,
        // eval-parse-stmt.cpp:359).
        let label = if self.no_newline() && matches!(self.current().kind, TokenKind::Ident(_)) {
            Some(self.expect_ident().0)
        } else {
            None
        };
        self.semicolon();
        let span = keyword.span;
        Stmt {
            kind: if is_break {
                StmtKind::Break { label }
            } else {
                StmtKind::Continue { label }
            },
            span,
        }
    }

    fn return_statement(&mut self) -> Stmt {
        let start = self.advance().span;
        // Restricted production: expression only on the same line
        // (ECMA-262 3rd ed. §7.9.1; avmplus eval-parse-stmt.cpp:339).
        let value = if self.no_newline() {
            Some(self.expression())
        } else {
            None
        };
        self.semicolon();
        let end = value.as_ref().map(|e| e.span).unwrap_or(start);
        Stmt {
            kind: StmtKind::Return { value },
            span: start.to(end),
        }
    }

    fn throw_statement(&mut self) -> Stmt {
        let start = self.advance().span;
        let value = self.expression();
        self.semicolon();
        let span = start.to(value.span);
        Stmt {
            kind: StmtKind::Throw { value },
            span,
        }
    }

    fn try_statement(&mut self) -> Stmt {
        let start = self.advance().span;
        let block = self.block();
        let mut catches = Vec::new();
        while self.at(&TokenKind::Catch) {
            let catch_start = self.advance().span;
            self.expect(&TokenKind::LParen);
            let (name, _) = self.expect_ident();
            let ty = if self.eat(&TokenKind::Colon) {
                Some(self.type_ref())
            } else {
                None
            };
            self.expect(&TokenKind::RParen);
            let body = self.block();
            let span = catch_start.to(body.span);
            catches.push(CatchClause {
                name,
                ty,
                body,
                span,
            });
        }
        let finally = if self.eat(&TokenKind::Finally) {
            Some(self.block())
        } else {
            None
        };
        if catches.is_empty() && finally.is_none() {
            self.error(
                ErrorCode::UNEXPECTED_TOKEN,
                "`try` needs at least one `catch` or a `finally`",
                start,
            );
        }
        let end = finally
            .as_ref()
            .map(|b| b.span)
            .or(catches.last().map(|c| c.span))
            .unwrap_or(block.span);
        Stmt {
            kind: StmtKind::Try {
                block,
                catches,
                finally,
            },
            span: start.to(end),
        }
    }
}
