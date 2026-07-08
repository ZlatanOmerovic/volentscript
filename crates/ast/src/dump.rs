//! Compact, stable tree dump of the AST — the snapshot format for golden
//! tests (SPECS §10) and the output of `asr parse`.

use std::fmt::Write as _;

use crate::*;

/// Renders a program as an indented tree.
pub fn dump(program: &Program) -> String {
    let mut d = Dumper::default();
    d.line("Program");
    d.indented(|d| {
        for stmt in &program.directives {
            d.stmt(stmt);
        }
    });
    d.out
}

#[derive(Default)]
struct Dumper {
    out: String,
    depth: usize,
}

impl Dumper {
    fn line(&mut self, text: impl AsRef<str>) {
        for _ in 0..self.depth {
            self.out.push_str("  ");
        }
        self.out.push_str(text.as_ref());
        self.out.push('\n');
    }

    fn indented(&mut self, f: impl FnOnce(&mut Self)) {
        self.depth += 1;
        f(self);
        self.depth -= 1;
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Expr(e) => {
                self.line("ExprStmt");
                self.indented(|d| d.expr(e));
            }
            StmtKind::VarDecl(v) => self.var_decl(v),
            StmtKind::Function(f) => self.function(f),
            StmtKind::Block(b) => self.block(b),
            StmtKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.line("If");
                self.indented(|d| {
                    d.labeled("cond", |d| d.expr(cond));
                    d.labeled("then", |d| d.stmt(then_branch));
                    if let Some(e) = else_branch {
                        d.labeled("else", |d| d.stmt(e));
                    }
                });
            }
            StmtKind::While { cond, body } => {
                self.line("While");
                self.indented(|d| {
                    d.labeled("cond", |d| d.expr(cond));
                    d.labeled("body", |d| d.stmt(body));
                });
            }
            StmtKind::DoWhile { body, cond } => {
                self.line("DoWhile");
                self.indented(|d| {
                    d.labeled("body", |d| d.stmt(body));
                    d.labeled("cond", |d| d.expr(cond));
                });
            }
            StmtKind::For {
                init,
                cond,
                update,
                body,
            } => {
                self.line("For");
                self.indented(|d| {
                    if let Some(init) = init {
                        d.labeled("init", |d| match init.as_ref() {
                            ForInit::VarDecl(v) => d.var_decl(v),
                            ForInit::Expr(e) => d.expr(e),
                        });
                    }
                    if let Some(cond) = cond {
                        d.labeled("cond", |d| d.expr(cond));
                    }
                    if let Some(update) = update {
                        d.labeled("update", |d| d.expr(update));
                    }
                    d.labeled("body", |d| d.stmt(body));
                });
            }
            StmtKind::ForIn {
                is_each,
                target,
                object,
                body,
            } => {
                self.line(if *is_each { "ForEachIn" } else { "ForIn" });
                self.indented(|d| {
                    d.labeled("target", |d| match target {
                        ForInTarget::VarDecl(v) => d.var_decl(v),
                        ForInTarget::Expr(e) => d.expr(e),
                    });
                    d.labeled("object", |d| d.expr(object));
                    d.labeled("body", |d| d.stmt(body));
                });
            }
            StmtKind::Switch { scrutinee, cases } => {
                self.line("Switch");
                self.indented(|d| {
                    d.labeled("scrutinee", |d| d.expr(scrutinee));
                    for case in cases {
                        match &case.test {
                            Some(test) => {
                                d.line("Case");
                                d.indented(|d| {
                                    d.labeled("test", |d| d.expr(test));
                                    for s in &case.body {
                                        d.stmt(s);
                                    }
                                });
                            }
                            None => {
                                d.line("Default");
                                d.indented(|d| {
                                    for s in &case.body {
                                        d.stmt(s);
                                    }
                                });
                            }
                        }
                    }
                });
            }
            StmtKind::Break { label } => match label {
                Some(l) => self.line(format!("Break label={l}")),
                None => self.line("Break"),
            },
            StmtKind::Continue { label } => match label {
                Some(l) => self.line(format!("Continue label={l}")),
                None => self.line("Continue"),
            },
            StmtKind::Return { value } => {
                self.line("Return");
                if let Some(v) = value {
                    self.indented(|d| d.expr(v));
                }
            }
            StmtKind::Throw { value } => {
                self.line("Throw");
                self.indented(|d| d.expr(value));
            }
            StmtKind::Try {
                block,
                catches,
                finally,
            } => {
                self.line("Try");
                self.indented(|d| {
                    d.block(block);
                    for c in catches {
                        d.line(format!(
                            "Catch name={}{}",
                            c.name,
                            c.ty.as_ref()
                                .map(|t| format!(" type={}", type_text(t)))
                                .unwrap_or_default()
                        ));
                        d.indented(|d| d.block(&c.body));
                    }
                    if let Some(f) = finally {
                        d.line("Finally");
                        d.indented(|d| d.block(f));
                    }
                });
            }
            StmtKind::Labeled { label, body } => {
                self.line(format!("Labeled label={label}"));
                self.indented(|d| d.stmt(body));
            }
            StmtKind::Empty => self.line("Empty"),
        }
    }

    fn labeled(&mut self, label: &str, f: impl FnOnce(&mut Self)) {
        self.line(format!("{label}:"));
        self.indented(f);
    }

    fn block(&mut self, block: &Block) {
        self.line("Block");
        self.indented(|d| {
            for stmt in &block.stmts {
                d.stmt(stmt);
            }
        });
    }

    fn var_decl(&mut self, decl: &VarDecl) {
        self.line(if decl.is_const {
            "ConstDecl"
        } else {
            "VarDecl"
        });
        self.indented(|d| {
            for b in &decl.bindings {
                let mut text = format!("Binding name={}", b.name);
                if let Some(ty) = &b.ty {
                    let _ = write!(text, " type={}", type_text(ty));
                }
                d.line(text);
                if let Some(init) = &b.init {
                    d.indented(|d| d.labeled("init", |d| d.expr(init)));
                }
            }
        });
    }

    fn function(&mut self, f: &FunctionDecl) {
        let mut header = String::from("Function");
        if let Some(name) = &f.name {
            let _ = write!(header, " name={name}");
        }
        if let Some(ret) = &f.return_type {
            let _ = write!(header, " returns={}", type_text(ret));
        }
        self.line(header);
        self.indented(|d| {
            for p in &f.params {
                let mut text = String::from("Param ");
                if p.rest {
                    text.push_str("...");
                }
                text.push_str(&p.name);
                if let Some(ty) = &p.ty {
                    let _ = write!(text, " type={}", type_text(ty));
                }
                d.line(text);
                if let Some(default) = &p.default {
                    d.indented(|d| d.labeled("default", |d| d.expr(default)));
                }
            }
            d.block(&f.body);
        });
    }

    fn expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Int(v) => self.line(format!("Int {v}")),
            ExprKind::UInt(v) => self.line(format!("UInt {v}")),
            ExprKind::Number(v) => self.line(format!("Number {v}")),
            ExprKind::Str(v) => self.line(format!("Str {v:?}")),
            ExprKind::Bool(v) => self.line(format!("Bool {v}")),
            ExprKind::Null => self.line("Null"),
            ExprKind::This => self.line("This"),
            ExprKind::Ident(name) => self.line(format!("Ident {name}")),
            ExprKind::Array(elements) => {
                self.line("Array");
                self.indented(|d| {
                    for e in elements {
                        match e {
                            Some(e) => d.expr(e),
                            None => d.line("Hole"),
                        }
                    }
                });
            }
            ExprKind::Object(props) => {
                self.line("Object");
                self.indented(|d| {
                    for (name, value) in props {
                        let key = match name {
                            PropName::Ident(s) => s.clone(),
                            PropName::Str(s) => format!("{s:?}"),
                            PropName::Number(n) => n.to_string(),
                        };
                        d.labeled(&format!("prop {key}"), |d| d.expr(value));
                    }
                });
            }
            ExprKind::Function(f) => self.function(f),
            ExprKind::Unary(op, e) => {
                self.line(format!("Unary {op:?}"));
                self.indented(|d| d.expr(e));
            }
            ExprKind::Postfix(op, e) => {
                self.line(format!("Postfix {op:?}"));
                self.indented(|d| d.expr(e));
            }
            ExprKind::Binary(op, l, r) => {
                self.line(format!("Binary {op:?}"));
                self.indented(|d| {
                    d.expr(l);
                    d.expr(r);
                });
            }
            ExprKind::Assign(op, l, r) => {
                self.line(format!("Assign {op:?}"));
                self.indented(|d| {
                    d.expr(l);
                    d.expr(r);
                });
            }
            ExprKind::Conditional(c, t, e) => {
                self.line("Conditional");
                self.indented(|d| {
                    d.expr(c);
                    d.expr(t);
                    d.expr(e);
                });
            }
            ExprKind::Call(callee, args) => {
                self.line("Call");
                self.indented(|d| {
                    d.expr(callee);
                    for a in args {
                        d.labeled("arg", |d| d.expr(a));
                    }
                });
            }
            ExprKind::New(callee, args) => {
                self.line("New");
                self.indented(|d| {
                    d.expr(callee);
                    for a in args {
                        d.labeled("arg", |d| d.expr(a));
                    }
                });
            }
            ExprKind::Member(object, name) => {
                self.line(format!("Member .{name}"));
                self.indented(|d| d.expr(object));
            }
            ExprKind::Index(object, index) => {
                self.line("Index");
                self.indented(|d| {
                    d.expr(object);
                    d.expr(index);
                });
            }
            ExprKind::Comma(l, r) => {
                self.line("Comma");
                self.indented(|d| {
                    d.expr(l);
                    d.expr(r);
                });
            }
        }
    }
}

/// One-line rendering of a type reference, e.g. `Vector.<int>?`.
pub fn type_text(ty: &TypeRef) -> String {
    let mut out = match &ty.kind {
        TypeRefKind::Any => "*".to_string(),
        TypeRefKind::Void => "void".to_string(),
        TypeRefKind::Name { path, type_args } => {
            let mut s = path.join(".");
            if !type_args.is_empty() {
                s.push_str(".<");
                s.push_str(
                    &type_args
                        .iter()
                        .map(type_text)
                        .collect::<Vec<_>>()
                        .join(","),
                );
                s.push('>');
            }
            s
        }
    };
    if ty.nullable {
        out.push('?');
    }
    out
}
