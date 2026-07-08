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
            Package => self.package_decl(),
            Import => self.import_decl(),
            Namespace => self.namespace_decl(),
            Use => self.use_namespace(),
            // Declarations, possibly preceded by modifiers.
            Class | Interface | Public | Private | Protected | Internal | Final | Dynamic
            | Native | Override | Static => self.attributed_declaration(),
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
        // Generic type parameters: `function firstOf.<T>(...)` (SPECS §4.2).
        let mut type_params = Vec::new();
        if self.eat(&TokenKind::LeftDotAngle) {
            loop {
                type_params.push(self.expect_ident().0);
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect_type_close();
        }
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
            type_params,
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

    // --- packages, classes, interfaces ------------------------------------------

    /// `package a.b { directives }` (SPECS §9 packageDecl).
    fn package_decl(&mut self) -> Stmt {
        let start = self.advance().span; // `package`
        let mut path = Vec::new();
        if matches!(self.current().kind, TokenKind::Ident(_)) {
            path.push(self.expect_ident().0);
            while self.at(&TokenKind::Dot) {
                self.advance();
                path.push(self.expect_ident().0);
            }
        }
        self.expect(&TokenKind::LBrace);
        let mut body = Vec::new();
        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
            let before = self.pos;
            body.push(self.statement());
            if self.pos == before {
                self.advance();
            }
        }
        let end = self.expect(&TokenKind::RBrace);
        Stmt {
            kind: StmtKind::Package { path, body },
            span: start.to(end),
        }
    }

    fn import_decl(&mut self) -> Stmt {
        let start = self.advance().span; // `import`
        let mut path = vec![self.expect_ident().0];
        let mut wildcard = false;
        while self.eat(&TokenKind::Dot) {
            if self.eat(&TokenKind::Star) {
                wildcard = true;
                break;
            }
            path.push(self.expect_ident().0);
        }
        self.semicolon();
        Stmt {
            kind: StmtKind::Import { path, wildcard },
            span: start,
        }
    }

    /// `namespace n;` / `namespace n = "uri";` (avmplus
    /// eval-parse.cpp:788: the initializer must be a string literal or
    /// another namespace identifier; only strings are supported in v1).
    fn namespace_decl(&mut self) -> Stmt {
        let start = self.advance().span; // `namespace`
        let (name, _) = self.expect_ident();
        let mut uri = None;
        if self.eat(&TokenKind::Assign) {
            match self.current().kind.clone() {
                TokenKind::Str(v) => {
                    self.advance();
                    uri = Some(v);
                }
                _ => {
                    let span = self.current().span;
                    self.error(
                        ErrorCode::UNEXPECTED_TOKEN,
                        "a namespace initializer must be a string literal",
                        span,
                    );
                    self.recover_to_statement_boundary();
                }
            }
        }
        self.semicolon();
        Stmt {
            kind: StmtKind::NamespaceDecl { name, uri },
            span: start,
        }
    }

    /// `use namespace n;` (avmplus eval-parse-stmt.cpp:252).
    fn use_namespace(&mut self) -> Stmt {
        let start = self.advance().span; // `use`
        if !self.eat(&TokenKind::Namespace) {
            let span = self.current().span;
            self.error(
                ErrorCode::UNEXPECTED_TOKEN,
                "`use` must be followed by `namespace` (SYNTAXERR_ILLEGAL_USE)",
                span,
            );
        }
        let (name, _) = self.expect_ident();
        self.semicolon();
        Stmt {
            kind: StmtKind::UseNamespace(name),
            span: start,
        }
    }

    /// Collects modifier keywords (avmplus dispatches these the same way,
    /// eval-parse.cpp:205), then parses the declaration they precede.
    fn attributed_declaration(&mut self) -> Stmt {
        use TokenKind::*;
        let start = self.current().span;
        let mut attrs = Attributes::default();
        loop {
            let vis = match self.current().kind {
                Public => Some(Visibility::Public),
                Private => Some(Visibility::Private),
                Protected => Some(Visibility::Protected),
                Internal => Some(Visibility::Internal),
                _ => None,
            };
            if let Some(v) = vis {
                if attrs.visibility.is_some() {
                    let span = self.current().span;
                    self.error(
                        ErrorCode::UNEXPECTED_TOKEN,
                        "only one access modifier is allowed",
                        span,
                    );
                }
                attrs.visibility = Some(v);
                self.advance();
                continue;
            }
            match self.current().kind {
                Static => attrs.is_static = true,
                Final => attrs.is_final = true,
                Override => attrs.is_override = true,
                Dynamic => attrs.is_dynamic = true,
                Native => attrs.is_native = true,
                _ => break,
            }
            self.advance();
        }
        match self.current().kind {
            Class => self.class_decl(start, attrs),
            Interface => self.interface_decl(start, attrs),
            // Modifiers on functions/vars at package level.
            Function => {
                let stmt = self.function_declaration();
                Stmt {
                    span: start.to(stmt.span),
                    ..stmt
                }
            }
            Var | Const => self.var_statement(),
            _ => {
                let span = self.current().span;
                self.error(
                    ErrorCode::UNEXPECTED_TOKEN,
                    format!(
                        "expected a declaration after modifiers, found {}",
                        self.current().kind.describe()
                    ),
                    span,
                );
                self.recover_to_statement_boundary();
                Stmt {
                    kind: StmtKind::Empty,
                    span,
                }
            }
        }
    }

    /// `class C extends B implements I, J { members }` (SPECS §9).
    fn class_decl(&mut self, start: Span, attrs: Attributes) -> Stmt {
        self.advance(); // `class`
        let (name, _) = self.expect_ident();
        // Generic type parameters: `class Box.<T, U>` (SPECS §4.2).
        let mut type_params = Vec::new();
        if self.eat(&TokenKind::LeftDotAngle) {
            loop {
                type_params.push(self.expect_ident().0);
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect_type_close();
        }
        let extends = if self.eat(&TokenKind::Extends) {
            Some(self.type_ref())
        } else {
            None
        };
        let mut implements = Vec::new();
        if self.eat(&TokenKind::Implements) {
            loop {
                implements.push(self.type_ref());
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect(&TokenKind::LBrace);
        let mut members = Vec::new();
        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
            let before = self.pos;
            if let Some(m) = self.class_member() {
                members.push(m);
            }
            if self.pos == before {
                self.advance();
            }
        }
        let end = self.expect(&TokenKind::RBrace);
        let span = start.to(end);
        Stmt {
            kind: StmtKind::Class(Box::new(ClassDecl {
                attrs,
                name,
                type_params,
                extends,
                implements,
                members,
                span,
            })),
            span,
        }
    }

    fn class_member(&mut self) -> Option<Member> {
        use TokenKind::*;
        if self.eat(&Semicolon) {
            return None;
        }
        let start = self.current().span;
        let mut attrs = Attributes::default();
        loop {
            let vis = match self.current().kind {
                Public => Some(Visibility::Public),
                Private => Some(Visibility::Private),
                Protected => Some(Visibility::Protected),
                Internal => Some(Visibility::Internal),
                _ => None,
            };
            if let Some(v) = vis {
                attrs.visibility = Some(v);
                self.advance();
                continue;
            }
            match self.current().kind {
                Static => attrs.is_static = true,
                Final => attrs.is_final = true,
                Override => attrs.is_override = true,
                Native => attrs.is_native = true,
                // A bare identifier before a member declaration is a
                // custom namespace qualifier (avmplus eval-parse.cpp:216
                // configname_or_namespacename; QUAL_name). Mutually
                // exclusive with an access modifier.
                Ident(_)
                    if matches!(
                        self.peek_kind(1),
                        Var | Const | Function | Static | Final | Override | Native
                    ) =>
                {
                    let (name, span) = self.expect_ident();
                    if attrs.visibility.is_some() {
                        self.error(
                            ErrorCode::UNEXPECTED_TOKEN,
                            "a namespace qualifier cannot be combined with an access modifier",
                            span,
                        );
                    }
                    attrs.namespace_ = Some(name);
                    continue;
                }
                _ => break,
            }
            self.advance();
        }
        let kind = match self.current().kind {
            Var | Const => {
                let decl = self.var_declaration();
                self.semicolon();
                MemberKind::Field(decl)
            }
            Function => {
                let keyword_span = self.advance().span;
                // `get`/`set` are contextual: accessor only when another
                // identifier follows (avmplus eval-parse.cpp:1128).
                let accessor = match &self.current().kind {
                    Ident(word)
                        if (word == "get" || word == "set")
                            && matches!(self.peek_kind(1), Ident(_)) =>
                    {
                        let is_get = word == "get";
                        self.advance();
                        Some(is_get)
                    }
                    _ => None,
                };
                let decl = Box::new(self.function_after_keyword(keyword_span, true));
                match accessor {
                    Some(true) => MemberKind::Getter(decl),
                    Some(false) => MemberKind::Setter(decl),
                    None => MemberKind::Method(decl),
                }
            }
            _ => {
                let span = self.current().span;
                self.error(
                    ErrorCode::UNEXPECTED_TOKEN,
                    format!(
                        "expected a class member, found {}",
                        self.current().kind.describe()
                    ),
                    span,
                );
                self.recover_to_statement_boundary();
                return None;
            }
        };
        let end = self.tokens[self.pos.saturating_sub(1)].span;
        Some(Member {
            attrs,
            kind,
            span: start.to(end),
        })
    }

    /// `interface I extends J, K { signatures }` (SPECS §3.5).
    fn interface_decl(&mut self, start: Span, attrs: Attributes) -> Stmt {
        self.advance(); // `interface`
        let (name, _) = self.expect_ident();
        let mut extends = Vec::new();
        if self.eat(&TokenKind::Extends) {
            loop {
                extends.push(self.type_ref());
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect(&TokenKind::LBrace);
        let mut members = Vec::new();
        while !self.at(&TokenKind::RBrace) && !self.at(&TokenKind::Eof) {
            let before = self.pos;
            if self.eat(&TokenKind::Semicolon) {
                continue;
            }
            if let Some(m) = self.interface_member() {
                members.push(m);
            }
            if self.pos == before {
                self.advance();
            }
        }
        let end = self.expect(&TokenKind::RBrace);
        let span = start.to(end);
        Stmt {
            kind: StmtKind::Interface(Box::new(InterfaceDecl {
                attrs,
                name,
                extends,
                members,
                span,
            })),
            span,
        }
    }

    fn interface_member(&mut self) -> Option<InterfaceMember> {
        let start = self.expect(&TokenKind::Function);
        let kind = match &self.current().kind {
            TokenKind::Ident(word)
                if (word == "get" || word == "set")
                    && matches!(self.peek_kind(1), TokenKind::Ident(_)) =>
            {
                let k = if word == "get" {
                    SigKind::Getter
                } else {
                    SigKind::Setter
                };
                self.advance();
                k
            }
            _ => SigKind::Method,
        };
        let (name, _) = self.expect_ident();
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
        self.semicolon();
        let end = self.tokens[self.pos.saturating_sub(1)].span;
        Some(InterfaceMember {
            kind,
            name,
            params,
            return_type,
            span: start.to(end),
        })
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
