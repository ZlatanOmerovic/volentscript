//! AST node types for the P1 core subset.
//!
//! Shape follows the grammar sketched in SPECS §9; the concrete productions
//! are checked against avmplus `eval-parse-*.cpp` (see parser crate). P1
//! covers functions, primitives, expressions, and statements — classes,
//! interfaces, and packages arrive in later phases and extend these types.

#![forbid(unsafe_code)]

mod dump;

pub use dump::dump;

use span::Span;

/// A parsed compilation unit (one `.as` file): its top-level directives.
#[derive(Debug)]
pub struct Program {
    /// Top-level statements and declarations, in source order.
    pub directives: Vec<Stmt>,
    /// Span covering the whole file.
    pub span: Span,
}

// --- statements -----------------------------------------------------------

/// A statement or declaration.
#[derive(Debug)]
pub struct Stmt {
    /// What kind of statement.
    pub kind: StmtKind,
    /// Source range.
    pub span: Span,
}

/// Statement kinds (SPECS §3.8).
#[derive(Debug)]
#[allow(missing_docs)] // field names + the grammar are the documentation
pub enum StmtKind {
    Expr(Expr),
    VarDecl(VarDecl),
    Function(Box<FunctionDecl>),
    Block(Block),
    If {
        cond: Expr,
        then_branch: Box<Stmt>,
        else_branch: Option<Box<Stmt>>,
    },
    While {
        cond: Expr,
        body: Box<Stmt>,
    },
    DoWhile {
        body: Box<Stmt>,
        cond: Expr,
    },
    For {
        init: Option<Box<ForInit>>,
        cond: Option<Expr>,
        update: Option<Expr>,
        body: Box<Stmt>,
    },
    /// `for..in` (keys) and `for each..in` (values, `is_each`).
    ForIn {
        is_each: bool,
        target: ForInTarget,
        object: Expr,
        body: Box<Stmt>,
    },
    Switch {
        scrutinee: Expr,
        cases: Vec<SwitchCase>,
    },
    Break {
        label: Option<String>,
    },
    Continue {
        label: Option<String>,
    },
    Return {
        value: Option<Expr>,
    },
    Throw {
        value: Expr,
    },
    Try {
        block: Block,
        /// AS3 allows multiple `catch` clauses discriminated by type.
        catches: Vec<CatchClause>,
        finally: Option<Block>,
    },
    Labeled {
        label: String,
        body: Box<Stmt>,
    },
    Empty,
}

/// `{ ... }`.
#[derive(Debug)]
pub struct Block {
    /// Statements in source order.
    pub stmts: Vec<Stmt>,
    /// Source range including the braces.
    pub span: Span,
}

/// The `init` part of a C-style `for`.
#[derive(Debug)]
#[allow(missing_docs)]
pub enum ForInit {
    VarDecl(VarDecl),
    Expr(Expr),
}

/// The loop variable of a `for..in` / `for each..in`.
#[derive(Debug)]
#[allow(missing_docs)]
pub enum ForInTarget {
    /// `for (var x:T in obj)`
    VarDecl(VarDecl),
    /// `for (x in obj)` — any assignable expression.
    Expr(Expr),
}

/// One `case`/`default` clause of a `switch`.
#[derive(Debug)]
pub struct SwitchCase {
    /// `None` for `default:`.
    pub test: Option<Expr>,
    /// Clause body (fall-through is semantic, not syntactic).
    pub body: Vec<Stmt>,
    /// Source range.
    pub span: Span,
}

/// One `catch (name:Type) { ... }` clause.
#[derive(Debug)]
pub struct CatchClause {
    /// Bound exception variable.
    pub name: String,
    /// Declared catch type; `None` catches everything (`*`).
    pub ty: Option<TypeRef>,
    /// Handler body.
    pub body: Block,
    /// Source range.
    pub span: Span,
}

/// `var`/`const` declaration with one or more bindings.
#[derive(Debug)]
pub struct VarDecl {
    /// `const` vs `var`.
    pub is_const: bool,
    /// The declared bindings.
    pub bindings: Vec<Binding>,
    /// Source range.
    pub span: Span,
}

/// One `name:Type = init` binding.
#[derive(Debug)]
pub struct Binding {
    /// Declared name.
    pub name: String,
    /// Declared type, if annotated.
    pub ty: Option<TypeRef>,
    /// Initializer, if present.
    pub init: Option<Expr>,
    /// Source range.
    pub span: Span,
}

// --- functions ---------------------------------------------------------------

/// `function name(params):T { ... }` — declaration or expression form.
#[derive(Debug)]
pub struct FunctionDecl {
    /// Function name; `None` only for anonymous function expressions.
    pub name: Option<String>,
    /// Parameter list.
    pub params: Vec<Param>,
    /// Declared return type, if annotated.
    pub return_type: Option<TypeRef>,
    /// Body block.
    pub body: Block,
    /// Source range.
    pub span: Span,
}

/// One function parameter.
#[derive(Debug)]
pub struct Param {
    /// Parameter name.
    pub name: String,
    /// Declared type, if annotated.
    pub ty: Option<TypeRef>,
    /// Default value, if any.
    pub default: Option<Expr>,
    /// `...rest` parameter.
    pub rest: bool,
    /// Source range.
    pub span: Span,
}

// --- types ---------------------------------------------------------------------

/// A syntactic type reference (SPECS §9 `typeRef`).
#[derive(Debug)]
pub struct TypeRef {
    /// The named or wildcard type.
    pub kind: TypeRefKind,
    /// Trailing `?` — nullable (SPECS §4.1).
    pub nullable: bool,
    /// Source range.
    pub span: Span,
}

/// Type reference kinds.
#[derive(Debug)]
#[allow(missing_docs)]
pub enum TypeRefKind {
    /// `*` — the any type (SPECS §3.11).
    Any,
    /// `void`.
    Void,
    /// Possibly-qualified name with optional `.<T,...>` application.
    Name {
        /// Dotted path, e.g. `["flash", "utils", "Dictionary"]` — one
        /// element for plain names.
        path: Vec<String>,
        /// `.<...>` type arguments (empty when absent).
        type_args: Vec<TypeRef>,
    },
}

// --- expressions ------------------------------------------------------------------

/// An expression.
#[derive(Debug)]
pub struct Expr {
    /// What kind of expression.
    pub kind: ExprKind,
    /// Source range.
    pub span: Span,
}

/// Expression kinds. Operator inventory per SPECS §3.9.
#[derive(Debug)]
#[allow(missing_docs)]
pub enum ExprKind {
    Int(i32),
    UInt(u32),
    Number(f64),
    Str(String),
    Bool(bool),
    Null,
    This,
    Ident(String),
    /// `[a, , b]` — `None` elements are elisions (sparse arrays, SPECS §3.10).
    Array(Vec<Option<Expr>>),
    /// `{a: 1, "b": 2, 3: x}` object initializer.
    Object(Vec<(PropName, Expr)>),
    Function(Box<FunctionDecl>),
    Unary(UnaryOp, Box<Expr>),
    /// Postfix `++`/`--`.
    Postfix(PostfixOp, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    Assign(AssignOp, Box<Expr>, Box<Expr>),
    /// `cond ? a : b`.
    Conditional(Box<Expr>, Box<Expr>, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
    New(Box<Expr>, Vec<Expr>),
    /// `a.b`.
    Member(Box<Expr>, String),
    /// `a[b]`.
    Index(Box<Expr>, Box<Expr>),
    /// `a, b`.
    Comma(Box<Expr>, Box<Expr>),
}

/// An object-initializer property name.
#[derive(Debug)]
#[allow(missing_docs)]
pub enum PropName {
    Ident(String),
    Str(String),
    Number(f64),
}

/// Prefix unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum UnaryOp {
    Delete,
    Void,
    Typeof,
    PreInc,
    PreDec,
    Plus,
    Minus,
    BitNot,
    Not,
}

/// Postfix unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum PostfixOp {
    Inc,
    Dec,
}

/// Binary operators, tightest tier first (ECMA-262 3rd ed. §11; `is`, `as`,
/// `in`, `instanceof` sit at the relational tier per avmplus
/// eval-parse-expr.cpp:638).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum BinaryOp {
    Mul,
    Div,
    Rem,
    Add,
    Sub,
    Shl,
    Shr,
    Ushr,
    Lt,
    Gt,
    Le,
    Ge,
    In,
    Instanceof,
    Is,
    As,
    Eq,
    Ne,
    StrictEq,
    StrictNe,
    BitAnd,
    BitXor,
    BitOr,
    LogAnd,
    LogOr,
}

/// Assignment operators (`=` and compounds; `&&=`/`||=` are real AS3
/// operators per avmplus eval-lex.h:44,46).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum AssignOp {
    Assign,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Shl,
    Shr,
    Ushr,
    BitAnd,
    BitOr,
    BitXor,
    LogAnd,
    LogOr,
}
