//! MIR: the typed, desugared, backend-agnostic mid-level IR.
//!
//! The frontend lowers the typed AST into this IR; backends consume it
//! (SPECS §8 stage 5). Nothing here references any backend, and no backend
//! type may appear above `codegen` (CLAUDE.md prime directive 3).
//!
//! P3 lowers the core subset. Typed-AST constructs belonging to later
//! phases (dynamic objects, closures, exceptions, `for..in`) are rejected
//! during lowering with phase-gated diagnostics — codegen never sees them.

#![forbid(unsafe_code)]

mod lower;

pub use lower::lower;

use span::Span;

/// A backend-level type. `Any` is the boxed dynamic value (`*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum Ty {
    Int,
    UInt,
    Number,
    Boolean,
    String,
    Any,
    Void,
}

/// Index of a function in [`Program::functions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FnId(pub u32);

/// Index of a local slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalId(pub u32);

/// A lowered program. `functions[0]` is the script body (the entry point
/// the runtime's `main` shim calls).
#[derive(Debug)]
pub struct Program {
    /// All functions.
    pub functions: Vec<Function>,
}

/// One lowered function.
#[derive(Debug)]
pub struct Function {
    /// Symbol-friendly name (informational; codegen derives real symbols).
    pub name: String,
    /// Return type.
    pub ret: Ty,
    /// All local slots; the first `param_count` are parameters.
    pub locals: Vec<Ty>,
    /// Number of leading locals that are parameters.
    pub param_count: usize,
    /// Callsite-fillable defaults for trailing parameters (index i is the
    /// default for parameter i; `None` = required).
    pub param_defaults: Vec<Option<Expr>>,
    /// Body statements.
    pub body: Vec<Stmt>,
    /// Source range.
    pub span: Span,
}

/// A lowered statement. Control flow stays structured — the backend builds
/// basic blocks (labels drive labeled break/continue).
#[derive(Debug)]
#[allow(missing_docs)]
pub enum Stmt {
    Expr(Expr),
    Assign(LocalId, Expr),
    Block(Vec<Stmt>),
    If {
        cond: Expr,
        then_branch: Box<Stmt>,
        else_branch: Option<Box<Stmt>>,
    },
    While {
        label: Option<String>,
        cond: Expr,
        body: Box<Stmt>,
    },
    DoWhile {
        label: Option<String>,
        body: Box<Stmt>,
        cond: Expr,
    },
    For {
        label: Option<String>,
        init: Option<Box<Stmt>>,
        cond: Option<Expr>,
        update: Option<Expr>,
        body: Box<Stmt>,
    },
    Switch {
        scrutinee: Expr,
        cases: Vec<Case>,
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
    Empty,
}

/// One `switch` clause (bodies fall through; `break` exits).
#[derive(Debug)]
pub struct Case {
    /// `None` = `default:`.
    pub test: Option<Expr>,
    /// Clause body.
    pub body: Vec<Stmt>,
}

/// A lowered, typed expression.
#[derive(Debug)]
pub struct Expr {
    /// Result type.
    pub ty: Ty,
    /// Source range (for backend-internal errors only).
    pub span: Span,
    /// Operation.
    pub kind: ExprKind,
}

/// Builtin global functions the runtime provides (SPECS §6, P3 set).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum Builtin {
    Trace,
    ParseInt,
    ParseFloat,
    IsNaN,
    IsFinite,
}

/// String instance methods with runtime implementations (SPECS §6, P3 set;
/// signatures per avmplus core/String.as).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum StrMethod {
    CharAt,
    CharCodeAt,
    IndexOf,
    LastIndexOf,
    Slice,
    Substring,
    Substr,
    ToLowerCase,
    ToUpperCase,
    ToString,
}

/// Numeric instance methods (receiver coerced to Number).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum NumMethod {
    ToString,
    ToFixed,
}

/// Value conversions (AS3 semantics, SPECS §3.3; ECMA-262 3rd ed. §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum Conv {
    /// ToInt32 (§9.5).
    ToInt,
    /// ToUint32 (§9.6).
    ToUInt,
    /// ToNumber (§9.3).
    ToNumber,
    /// ToBoolean (§9.2).
    ToBoolean,
    /// ToString (§9.8).
    ToString,
    /// Box into `*`.
    ToAny,
    /// AVM2 `coerce_s`: null/undefined → null, else ToString.
    AnyToString,
}

/// Binary operators (operand types already made uniform by sema).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum BinOp {
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
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
    StrictEq,
    StrictNe,
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
    TypeOf,
}

/// Lowered expression kinds.
#[derive(Debug)]
#[allow(missing_docs)]
pub enum ExprKind {
    Int(i32),
    UInt(u32),
    Number(f64),
    Str(String),
    Bool(bool),
    Null,
    Undefined,
    LocalGet(LocalId),
    LocalSet(LocalId, Box<Expr>),
    CallFn(FnId, Vec<Expr>),
    CallBuiltin(Builtin, Vec<Expr>),
    /// String method on a non-null String receiver.
    CallStrMethod(StrMethod, Box<Expr>, Vec<Expr>),
    /// Numeric method; receiver already coerced to Number.
    CallNumMethod(NumMethod, Box<Expr>, Vec<Expr>),
    /// `String#length`.
    StrLen(Box<Expr>),
    Unary(UnOp, Box<Expr>),
    /// Pre/post increment/decrement of a local; `is_inc`, `is_prefix`.
    IncDec {
        target: LocalId,
        is_inc: bool,
        is_prefix: bool,
    },
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// Short-circuit `&&`/`||` (value-preserving); `is_and`.
    Logical {
        is_and: bool,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Conditional(Box<Expr>, Box<Expr>, Box<Expr>),
    /// Runtime type test of a boxed value against a core type.
    Is(Box<Expr>, Ty),
    /// Checked cast: value or null (target recorded; result type is `ty`).
    As(Box<Expr>, Ty),
    Conv(Conv, Box<Expr>),
    Comma(Box<Expr>, Box<Expr>),
}
