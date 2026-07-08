//! The typed AST: what `check` produces and P3 lowers to MIR.
//!
//! Every expression carries its computed type; every implicit conversion the
//! source relied on is an explicit [`TExprKind::Coerce`] node, so later
//! stages never re-derive coercion rules (SPECS §8 stage 4, "coercion
//! insertion").

use span::Span;

use crate::classes::{ClassId, IfaceId, Registry};
use crate::ty::Ty;

/// Index of a function in [`TProgram::functions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FnId(pub u32);

/// Index of a local slot in the enclosing function's [`TFunction::locals`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);

/// A fully type-checked program.
#[derive(Debug)]
pub struct TProgram {
    /// All functions, including nested ones, function expressions, methods,
    /// constructors, and accessors. `functions[0]` is the synthesized script
    /// body (top-level statements, preceded by static initializers).
    pub functions: Vec<TFunction>,
    /// The class/interface registry (layouts, vtables).
    pub registry: Registry,
    /// `Vector.<T>` instantiation table: index = `Ty::Vector` payload,
    /// value = element type.
    pub vectors: Vec<crate::ty::Ty>,
    /// The top-level `main` function, when declared (SPECS §7: invoked
    /// after top-level statements; an int return becomes the exit code).
    pub entry_main: Option<FnId>,
}

/// The id of the synthesized top-level script function.
pub const SCRIPT_FN: FnId = FnId(0);

/// One checked function.
#[derive(Debug)]
pub struct TFunction {
    /// Source name; script body is `<script>`, anonymous exprs `<anonymous>`.
    pub name: String,
    /// When this is an instance method/accessor/constructor: the class whose
    /// `this` it receives (backends add the implicit receiver parameter).
    pub method_of: Option<ClassId>,
    /// Declared return type (`Void` when unannotated per AS3, but the
    /// checker records what was declared).
    pub return_ty: Ty,
    /// All local slots: parameters first (in order), then hoisted vars.
    pub locals: Vec<Local>,
    /// Number of leading `locals` that are parameters.
    pub param_count: usize,
    /// Free variables captured from enclosing frames, in environment order.
    pub captures: Vec<CapSrc>,
    /// Body statements.
    pub body: Vec<TStmt>,
    /// Source range.
    pub span: Span,
}

/// Where a captured variable lives in the *defining* function's frame
/// (closure conversion, SPECS §3.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapSrc {
    /// A local (cell) of the immediately enclosing function.
    ParentLocal(LocalId),
    /// A capture slot of the immediately enclosing function (multi-level).
    ParentCapture(usize),
}

/// One local slot (parameter or hoisted `var`).
#[derive(Debug)]
pub struct Local {
    /// Declared name.
    pub name: String,
    /// Declared (or defaulted) type.
    pub ty: Ty,
    /// Declared `T?` (SPECS §4.1). Non-reference types ignore this.
    pub nullable: bool,
    /// `const` — assignment outside initialization is E0304.
    pub is_const: bool,
    /// Parameter default value, if this local is a defaulted parameter.
    pub default: Option<TExpr>,
    /// Rest parameter (`...args`).
    pub is_rest: bool,
    /// Captured by a nested function — storage becomes a heap cell.
    pub captured: bool,
}

/// A checked statement.
#[derive(Debug)]
pub struct TStmt {
    /// What kind of statement.
    pub kind: TStmtKind,
    /// Source range.
    pub span: Span,
}

/// Checked statement kinds. Mirrors `ast::StmtKind` minus declarations
/// (hoisted into [`TFunction::locals`] / [`TProgram::functions`]).
#[derive(Debug)]
#[allow(missing_docs)]
pub enum TStmtKind {
    Expr(TExpr),
    /// Initialization of a hoisted local (`var x = e` after hoisting).
    Assign(LocalId, TExpr),
    Block(Vec<TStmt>),
    If {
        cond: TExpr,
        then_branch: Box<TStmt>,
        else_branch: Option<Box<TStmt>>,
    },
    While {
        cond: TExpr,
        body: Box<TStmt>,
    },
    DoWhile {
        body: Box<TStmt>,
        cond: TExpr,
    },
    For {
        init: Option<Box<TStmt>>,
        cond: Option<TExpr>,
        update: Option<TExpr>,
        body: Box<TStmt>,
    },
    ForIn {
        is_each: bool,
        target: LocalId,
        object: TExpr,
        body: Box<TStmt>,
    },
    Switch {
        scrutinee: TExpr,
        cases: Vec<TCase>,
    },
    Break {
        label: Option<String>,
    },
    Continue {
        label: Option<String>,
    },
    Return {
        value: Option<TExpr>,
    },
    Throw {
        value: TExpr,
    },
    Try {
        block: Vec<TStmt>,
        catches: Vec<TCatch>,
        finally: Option<Vec<TStmt>>,
    },
    Labeled {
        label: String,
        body: Box<TStmt>,
    },
    Empty,
}

/// One checked `switch` clause.
#[derive(Debug)]
pub struct TCase {
    /// `None` = `default:`.
    pub test: Option<TExpr>,
    /// Clause body.
    pub body: Vec<TStmt>,
}

/// One checked `catch` clause.
#[derive(Debug)]
pub struct TCatch {
    /// Local slot the exception binds to.
    pub binding: LocalId,
    /// Handler body.
    pub body: Vec<TStmt>,
}

/// A checked expression: kind + computed type.
#[derive(Debug)]
pub struct TExpr {
    /// Computed (post-coercion) type.
    pub ty: Ty,
    /// Source range.
    pub span: Span,
    /// What kind of expression.
    pub kind: TExprKind,
}

/// Array instance methods (SPECS §6, P5 set).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum ArrMethod {
    Push,
    Pop,
    Shift,
    Unshift,
    Slice,
    Splice,
    IndexOf,
    Concat,
    Join,
    Reverse,
    Sort,
    ForEach,
    Map,
    Filter,
    Some,
    Every,
}

/// Vector instance methods (same surface where meaningful, SPECS §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum VecMethod {
    Push,
    Pop,
    Shift,
    Unshift,
    Slice,
    IndexOf,
    Join,
    Reverse,
}

/// Checked expression kinds.
#[derive(Debug)]
#[allow(missing_docs)]
pub enum TExprKind {
    Int(i32),
    UInt(u32),
    Number(f64),
    Str(String),
    Bool(bool),
    Null,
    /// `undefined` (the `*` default).
    Undefined,
    LocalGet(LocalId),
    LocalSet(LocalId, Box<TExpr>),
    /// Reference to a declared function (a `Function` value).
    FnRef(FnId),
    /// A builtin global function used as a value.
    BuiltinRef(crate::builtins::BuiltinFn),
    /// Direct call of a declared function.
    CallFn(FnId, Vec<TExpr>),
    /// Call of a builtin (e.g. `trace`).
    CallBuiltin(crate::builtins::BuiltinFn, Vec<TExpr>),
    /// Indirect call through a `Function`/`*` value — unchecked, returns `*`.
    CallIndirect(Box<TExpr>, Vec<TExpr>),
    /// Property read (P2: builtin members like `String#length` or dynamic
    /// access on `*`).
    Member(Box<TExpr>, String),
    MemberSet(Box<TExpr>, String, Box<TExpr>),
    Index(Box<TExpr>, Box<TExpr>),
    IndexSet(Box<TExpr>, Box<TExpr>, Box<TExpr>),
    /// Builtin method call (receiver, method, args) e.g. `s.charAt(0)`.
    CallMethod(Box<TExpr>, String, Vec<TExpr>),
    Unary(ast::UnaryOp, Box<TExpr>),
    /// Post-increment/-decrement of a local (other targets desugared later).
    Postfix(ast::PostfixOp, Box<TExpr>),
    Binary(ast::BinaryOp, Box<TExpr>, Box<TExpr>),
    /// Logical `&&`/`||` keep operand values; separate from strict-value
    /// binaries for lowering.
    Logical(ast::BinaryOp, Box<TExpr>, Box<TExpr>),
    Conditional(Box<TExpr>, Box<TExpr>, Box<TExpr>),
    /// Runtime type test/cast against a core type, class, or interface.
    Is(Box<TExpr>, Ty),
    As(Box<TExpr>, Ty),
    /// `this` inside an instance member.
    This,
    /// `new C(args)` — allocate, init fields, run constructor.
    New(ClassId, Vec<TExpr>),
    /// Instance field read: (receiver, defining class, slot).
    FieldGet(Box<TExpr>, ClassId, usize),
    /// Instance field write.
    FieldSet(Box<TExpr>, ClassId, usize, Box<TExpr>),
    /// Virtual dispatch through the receiver's vtable slot.
    CallVirtual {
        recv: Box<TExpr>,
        class: ClassId,
        vslot: usize,
        args: Vec<TExpr>,
    },
    /// Dispatch through an interface method table.
    CallIface {
        recv: Box<TExpr>,
        iface: IfaceId,
        islot: usize,
        args: Vec<TExpr>,
    },
    /// Statically bound call with an explicit receiver: `super.m(...)`
    /// and constructor-chained calls.
    CallDirect {
        fn_id: FnId,
        recv: Box<TExpr>,
        args: Vec<TExpr>,
    },
    /// `super(args)` constructor chain (only in constructors).
    SuperCtor(ClassId, Vec<TExpr>),
    /// Static field read/write: (class, static index).
    StaticGet(ClassId, usize),
    StaticSet(ClassId, usize, Box<TExpr>),
    /// `new <T>[...]` / `new Vector.<T>(...)` (payload: instantiation id).
    VectorLit(u32, Vec<TExpr>),
    /// Array method call.
    CallArr(ArrMethod, Box<TExpr>, Vec<TExpr>),
    /// Vector method call (element coercions already inserted).
    CallVec(VecMethod, Box<TExpr>, Vec<TExpr>),
    /// `length` read on Array/Vector.
    SeqLen(Box<TExpr>),
    /// `length = n` on Array/Vector (truncates/extends).
    SeqSetLen(Box<TExpr>, Box<TExpr>),
    /// Read/write of a variable captured from an enclosing frame
    /// (index into the current function's `captures`).
    CaptureGet(usize),
    CaptureSet(usize, Box<TExpr>),
    /// Closure creation over a checked function (env from current frame).
    Closure(FnId),
    /// `obj.method` extraction — permanently bound method closure
    /// (SPECS §3.7).
    BoundMethod(Box<TExpr>, ClassId, usize),
    /// Native static call (Math/System/File/JSON/Date surfaces).
    CallNative(crate::builtins::NativeFn, Vec<TExpr>),
    /// Dynamic property ops on `*`/dynamic objects (SPECS §3.2).
    HasProp(Box<TExpr>, Box<TExpr>),
    DeleteProp(Box<TExpr>, Box<TExpr>),
    /// `f.call(thisArg, ...)` / `f.apply(thisArg, argsArray)`.
    CallFunctionValue {
        callee: Box<TExpr>,
        this_arg: Option<Box<TExpr>>,
        args: Vec<TExpr>,
        is_apply: bool,
    },
    /// Implicit conversion made explicit; `ty` is the target.
    Coerce(Coercion, Box<TExpr>),
    Array(Vec<Option<TExpr>>),
    Object(Vec<(String, TExpr)>),
    Comma(Box<TExpr>, Box<TExpr>),
    /// Placeholder produced after a reported error.
    Error,
}

/// Which runtime conversion a [`TExprKind::Coerce`] performs
/// (AS3 conversion semantics, SPECS §3.3; ECMA-262 3rd ed. §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum Coercion {
    /// ToInt32 (ECMA-262 §9.5).
    ToInt,
    /// ToUint32 (ECMA-262 §9.6).
    ToUInt,
    /// ToNumber (ECMA-262 §9.3).
    ToNumber,
    /// ToBoolean (ECMA-262 §9.2).
    ToBoolean,
    /// ToString (ECMA-262 §9.8) — inserted only where AS3 does (e.g. `+`).
    ToString,
    /// Box/erase into `*`.
    ToAny,
    /// Runtime-checked conversion from `*` to a typed slot
    /// (AVM2 `coerce` semantics: throws TypeError on mismatch).
    FromAny,
}
