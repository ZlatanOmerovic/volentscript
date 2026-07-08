//! Typed-AST dump: the snapshot format for sema golden tests and the CLI
//! `check --dump` output. Shows computed types and inserted coercions.

use std::fmt::Write as _;

use crate::tast::*;

/// Renders a typed program as an indented tree.
pub fn dump(program: &TProgram) -> String {
    let mut d = Dumper::default();
    for (i, f) in program.functions.iter().enumerate() {
        d.line(format!("fn #{i} {} : {}", f.name, f.return_ty));
        d.indented(|d| {
            for (li, local) in f.locals.iter().enumerate() {
                let kind = if li < f.param_count {
                    if local.is_rest { "rest-param" } else { "param" }
                } else if local.is_const {
                    "const"
                } else {
                    "var"
                };
                d.line(format!("local %{li} {kind} {} : {}", local.name, local.ty));
                if let Some(default) = &local.default {
                    d.indented(|d| d.labeled("default", |d| d.expr(default)));
                }
            }
            for s in &f.body {
                d.stmt(s);
            }
        });
    }
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

    fn labeled(&mut self, label: &str, f: impl FnOnce(&mut Self)) {
        self.line(format!("{label}:"));
        self.indented(f);
    }

    fn stmts(&mut self, stmts: &[TStmt]) {
        for s in stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, stmt: &TStmt) {
        match &stmt.kind {
            TStmtKind::Expr(e) => {
                self.line("ExprStmt");
                self.indented(|d| d.expr(e));
            }
            TStmtKind::Assign(local, e) => {
                self.line(format!("Init %{}", local.0));
                self.indented(|d| d.expr(e));
            }
            TStmtKind::Block(b) => {
                self.line("Block");
                self.indented(|d| d.stmts(b));
            }
            TStmtKind::If {
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
            TStmtKind::While { cond, body } => {
                self.line("While");
                self.indented(|d| {
                    d.labeled("cond", |d| d.expr(cond));
                    d.labeled("body", |d| d.stmt(body));
                });
            }
            TStmtKind::DoWhile { body, cond } => {
                self.line("DoWhile");
                self.indented(|d| {
                    d.labeled("body", |d| d.stmt(body));
                    d.labeled("cond", |d| d.expr(cond));
                });
            }
            TStmtKind::For {
                init,
                cond,
                update,
                body,
            } => {
                self.line("For");
                self.indented(|d| {
                    if let Some(i) = init {
                        d.labeled("init", |d| d.stmt(i));
                    }
                    if let Some(c) = cond {
                        d.labeled("cond", |d| d.expr(c));
                    }
                    if let Some(u) = update {
                        d.labeled("update", |d| d.expr(u));
                    }
                    d.labeled("body", |d| d.stmt(body));
                });
            }
            TStmtKind::ForIn {
                is_each,
                target,
                object,
                body,
            } => {
                self.line(format!(
                    "{} target=%{}",
                    if *is_each { "ForEachIn" } else { "ForIn" },
                    target.0
                ));
                self.indented(|d| {
                    d.labeled("object", |d| d.expr(object));
                    d.labeled("body", |d| d.stmt(body));
                });
            }
            TStmtKind::Switch { scrutinee, cases } => {
                self.line("Switch");
                self.indented(|d| {
                    d.labeled("scrutinee", |d| d.expr(scrutinee));
                    for case in cases {
                        match &case.test {
                            Some(test) => {
                                d.line("Case");
                                d.indented(|d| {
                                    d.labeled("test", |d| d.expr(test));
                                    d.stmts(&case.body);
                                });
                            }
                            None => {
                                d.line("Default");
                                d.indented(|d| d.stmts(&case.body));
                            }
                        }
                    }
                });
            }
            TStmtKind::Break { label } => self.line(match label {
                Some(l) => format!("Break {l}"),
                None => "Break".into(),
            }),
            TStmtKind::Continue { label } => self.line(match label {
                Some(l) => format!("Continue {l}"),
                None => "Continue".into(),
            }),
            TStmtKind::Return { value } => {
                self.line("Return");
                if let Some(v) = value {
                    self.indented(|d| d.expr(v));
                }
            }
            TStmtKind::Throw { value } => {
                self.line("Throw");
                self.indented(|d| d.expr(value));
            }
            TStmtKind::Try {
                block,
                catches,
                finally,
            } => {
                self.line("Try");
                self.indented(|d| {
                    d.labeled("block", |d| d.stmts(block));
                    for c in catches {
                        d.labeled(&format!("catch %{}", c.binding.0), |d| d.stmts(&c.body));
                    }
                    if let Some(f) = finally {
                        d.labeled("finally", |d| d.stmts(f));
                    }
                });
            }
            TStmtKind::Labeled { label, body } => {
                self.line(format!("Labeled {label}"));
                self.indented(|d| d.stmt(body));
            }
            TStmtKind::Empty => {}
        }
    }

    fn expr(&mut self, e: &TExpr) {
        let mut header = String::new();
        let _ = write!(header, "[{}] ", e.ty);
        match &e.kind {
            TExprKind::Int(v) => self.line(format!("{header}Int {v}")),
            TExprKind::UInt(v) => self.line(format!("{header}UInt {v}")),
            TExprKind::Number(v) => self.line(format!("{header}Number {v}")),
            TExprKind::Str(v) => self.line(format!("{header}Str {v:?}")),
            TExprKind::Bool(v) => self.line(format!("{header}Bool {v}")),
            TExprKind::Null => self.line(format!("{header}Null")),
            TExprKind::Undefined => self.line(format!("{header}Undefined")),
            TExprKind::LocalGet(id) => self.line(format!("{header}LocalGet %{}", id.0)),
            TExprKind::LocalSet(id, v) => {
                self.line(format!("{header}LocalSet %{}", id.0));
                self.indented(|d| d.expr(v));
            }
            TExprKind::FnRef(id) => self.line(format!("{header}FnRef #{}", id.0)),
            TExprKind::BuiltinRef(b) => self.line(format!("{header}BuiltinRef {}", b.name())),
            TExprKind::CallFn(id, args) => {
                self.line(format!("{header}CallFn #{}", id.0));
                self.indented(|d| {
                    for a in args {
                        d.expr(a);
                    }
                });
            }
            TExprKind::CallBuiltin(b, args) => {
                self.line(format!("{header}CallBuiltin {}", b.name()));
                self.indented(|d| {
                    for a in args {
                        d.expr(a);
                    }
                });
            }
            TExprKind::CallIndirect(callee, args) => {
                self.line(format!("{header}CallIndirect"));
                self.indented(|d| {
                    d.expr(callee);
                    for a in args {
                        d.expr(a);
                    }
                });
            }
            TExprKind::CallMethod(receiver, name, args) => {
                self.line(format!("{header}CallMethod .{name}"));
                self.indented(|d| {
                    d.expr(receiver);
                    for a in args {
                        d.expr(a);
                    }
                });
            }
            TExprKind::Member(o, name) => {
                self.line(format!("{header}Member .{name}"));
                self.indented(|d| d.expr(o));
            }
            TExprKind::MemberSet(o, name, v) => {
                self.line(format!("{header}MemberSet .{name}"));
                self.indented(|d| {
                    d.expr(o);
                    d.expr(v);
                });
            }
            TExprKind::Index(o, i) => {
                self.line(format!("{header}Index"));
                self.indented(|d| {
                    d.expr(o);
                    d.expr(i);
                });
            }
            TExprKind::IndexSet(o, i, v) => {
                self.line(format!("{header}IndexSet"));
                self.indented(|d| {
                    d.expr(o);
                    d.expr(i);
                    d.expr(v);
                });
            }
            TExprKind::Unary(op, v) => {
                self.line(format!("{header}Unary {op:?}"));
                self.indented(|d| d.expr(v));
            }
            TExprKind::Postfix(op, v) => {
                self.line(format!("{header}Postfix {op:?}"));
                self.indented(|d| d.expr(v));
            }
            TExprKind::Binary(op, l, r) => {
                self.line(format!("{header}Binary {op:?}"));
                self.indented(|d| {
                    d.expr(l);
                    d.expr(r);
                });
            }
            TExprKind::Logical(op, l, r) => {
                self.line(format!("{header}Logical {op:?}"));
                self.indented(|d| {
                    d.expr(l);
                    d.expr(r);
                });
            }
            TExprKind::Conditional(c, t, f) => {
                self.line(format!("{header}Conditional"));
                self.indented(|d| {
                    d.expr(c);
                    d.expr(t);
                    d.expr(f);
                });
            }
            TExprKind::Is(v, ty) => {
                self.line(format!("{header}Is {ty}"));
                self.indented(|d| d.expr(v));
            }
            TExprKind::As(v, ty) => {
                self.line(format!("{header}As {ty}"));
                self.indented(|d| d.expr(v));
            }
            TExprKind::Coerce(c, v) => {
                self.line(format!("{header}Coerce {c:?}"));
                self.indented(|d| d.expr(v));
            }
            TExprKind::This => self.line(format!("{header}This")),
            TExprKind::New(class, args) => {
                self.line(format!("{header}New class#{}", class.0));
                self.indented(|d| {
                    for a in args {
                        d.expr(a);
                    }
                });
            }
            TExprKind::FieldGet(o, class, slot) => {
                self.line(format!("{header}FieldGet class#{} slot{slot}", class.0));
                self.indented(|d| d.expr(o));
            }
            TExprKind::FieldSet(o, class, slot, v) => {
                self.line(format!("{header}FieldSet class#{} slot{slot}", class.0));
                self.indented(|d| {
                    d.expr(o);
                    d.expr(v);
                });
            }
            TExprKind::CallVirtual {
                recv,
                class,
                vslot,
                args,
            } => {
                self.line(format!("{header}CallVirtual class#{} v{vslot}", class.0));
                self.indented(|d| {
                    d.expr(recv);
                    for a in args {
                        d.expr(a);
                    }
                });
            }
            TExprKind::CallIface {
                recv,
                iface,
                islot,
                args,
            } => {
                self.line(format!("{header}CallIface iface#{} i{islot}", iface.0));
                self.indented(|d| {
                    d.expr(recv);
                    for a in args {
                        d.expr(a);
                    }
                });
            }
            TExprKind::CallDirect { fn_id, recv, args } => {
                self.line(format!("{header}CallDirect #{}", fn_id.0));
                self.indented(|d| {
                    d.expr(recv);
                    for a in args {
                        d.expr(a);
                    }
                });
            }
            TExprKind::SuperCtor(class, args) => {
                self.line(format!("{header}SuperCtor class#{}", class.0));
                self.indented(|d| {
                    for a in args {
                        d.expr(a);
                    }
                });
            }
            TExprKind::StaticGet(class, i) => {
                self.line(format!("{header}StaticGet class#{} s{i}", class.0))
            }
            TExprKind::StaticSet(class, i, v) => {
                self.line(format!("{header}StaticSet class#{} s{i}", class.0));
                self.indented(|d| d.expr(v));
            }
            TExprKind::VectorLit(inst, elements) => {
                self.line(format!("{header}VectorLit v#{inst}"));
                self.indented(|d| {
                    for e in elements {
                        d.expr(e);
                    }
                });
            }
            TExprKind::CallArr(m, recv, args) => {
                self.line(format!("{header}CallArr {m:?}"));
                self.indented(|d| {
                    d.expr(recv);
                    for a in args {
                        d.expr(a);
                    }
                });
            }
            TExprKind::CallVec(m, recv, args) => {
                self.line(format!("{header}CallVec {m:?}"));
                self.indented(|d| {
                    d.expr(recv);
                    for a in args {
                        d.expr(a);
                    }
                });
            }
            TExprKind::SeqLen(o) => {
                self.line(format!("{header}SeqLen"));
                self.indented(|d| d.expr(o));
            }
            TExprKind::SeqSetLen(o, v) => {
                self.line(format!("{header}SeqSetLen"));
                self.indented(|d| {
                    d.expr(o);
                    d.expr(v);
                });
            }
            TExprKind::CaptureGet(i) => self.line(format!("{header}CaptureGet ^{i}")),
            TExprKind::CaptureSet(i, v) => {
                self.line(format!("{header}CaptureSet ^{i}"));
                self.indented(|d| d.expr(v));
            }
            TExprKind::Closure(f) => self.line(format!("{header}Closure #{}", f.0)),
            TExprKind::BoundMethod(o, class, vslot) => {
                self.line(format!("{header}BoundMethod class#{} v{vslot}", class.0));
                self.indented(|d| d.expr(o));
            }
            TExprKind::CallFunctionValue {
                callee,
                this_arg,
                args,
                is_apply,
            } => {
                self.line(format!(
                    "{header}{}",
                    if *is_apply { "Apply" } else { "CallValue" }
                ));
                self.indented(|d| {
                    d.expr(callee);
                    if let Some(t) = this_arg {
                        d.labeled("this", |d| d.expr(t));
                    }
                    for a in args {
                        d.expr(a);
                    }
                });
            }
            TExprKind::Array(elements) => {
                self.line(format!("{header}Array"));
                self.indented(|d| {
                    for el in elements {
                        match el {
                            Some(e) => d.expr(e),
                            None => d.line("Hole"),
                        }
                    }
                });
            }
            TExprKind::Object(props) => {
                self.line(format!("{header}Object"));
                self.indented(|d| {
                    for (k, v) in props {
                        d.labeled(&format!("prop {k}"), |d| d.expr(v));
                    }
                });
            }
            TExprKind::Comma(l, r) => {
                self.line(format!("{header}Comma"));
                self.indented(|d| {
                    d.expr(l);
                    d.expr(r);
                });
            }
            TExprKind::Error => self.line(format!("{header}Error")),
        }
    }
}
