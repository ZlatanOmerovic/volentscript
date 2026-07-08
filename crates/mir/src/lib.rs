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

mod bce;
mod lower;

pub use lower::lower;

/// Re-exported native-function identity (sema's table drives typing; the
/// backend drives emission).
pub use sema::NativeFn as SemaNativeFn;

use span::Span;

/// A backend-level type. `Any` is the boxed dynamic value (`*`);
/// `Object`/`Iface` are pointers to class instances.
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
    /// Instance pointer; payload = class index into [`Program::classes`].
    Object(u32),
    /// Instance pointer typed by interface index.
    Iface(u32),
    /// Array pointer (SPECS §3.10).
    Array,
    /// Function value: closure pointer (SPECS §3.7).
    Function,
    /// Vector pointer; payload indexes [`Program::vectors`] (reified
    /// element type, SPECS §4.2/§4.3).
    Vector(u32),
    /// RegExp pointer (ES3 §15.10; engine-backed runtime object).
    RegExp,
    /// Date pointer (ES3 §15.9; one time value).
    Date,
    /// Socket / ServerSocket pointer (SPECS §6 I/O; one runtime type,
    /// sema keeps the two nominal types apart).
    Socket,
    /// Namespace value (ES4 draft first-class namespaces; URI identity).
    Namespace,
}

/// Index of a function in [`Program::functions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FnId(pub u32);

/// Index of a local slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);

/// A lowered program. `functions[0]` is the script body (the entry point
/// the runtime's `main` shim calls).
#[derive(Debug)]
pub struct Program {
    /// All functions (script, user functions, methods, constructors —
    /// including synthesized default constructors).
    pub functions: Vec<Function>,
    /// Class layouts/vtables, index = the `Ty::Object` payload.
    pub classes: Vec<Class>,
    /// Number of interfaces (ids are dense; dispatch tables live on
    /// classes).
    pub iface_count: u32,
    /// Vector instantiations: index = `Ty::Vector` payload, value =
    /// element type.
    pub vectors: Vec<Ty>,
    /// Canonical namespace URIs; index = namespace id (declared-URI
    /// namespaces share ids; private ones get `vs:private:{id}`).
    pub namespace_uris: Vec<String>,
    /// Class indices of the prelude Error hierarchy, in runtime
    /// registration order: Error, TypeError, RangeError, ReferenceError,
    /// ArgumentError, SyntaxError. Codegen registers their descriptors at
    /// startup so the runtime can throw catchable errors.
    pub error_classes: Vec<u32>,
}

/// One lowered class: everything a backend needs to emit layout, RTTI, and
/// dispatch tables (object model per avm2overview.pdf §4.1, mapped to
/// native structs/vtables).
#[derive(Debug)]
pub struct Class {
    /// Qualified name (for RTTI / default toString).
    pub name: String,
    /// Parent class index; `None` = root (Object).
    pub parent: Option<u32>,
    /// Full instance slot types, inherited first (slot i = struct field
    /// i+1; field 0 is the descriptor pointer header).
    pub slots: Vec<Ty>,
    /// Full vtable: function per slot (overrides already substituted).
    pub vtable: Vec<FnId>,
    /// Constructor (always present — synthesized when the source had none;
    /// it chains to the parent and runs field initializers).
    pub ctor: FnId,
    /// Implemented interfaces: (interface id, method table in interface
    /// method order).
    pub ifaces: Vec<(u32, Vec<FnId>)>,
    /// `toString():String` override for RTTI display, if any.
    pub to_string: Option<FnId>,
    /// `dynamic class` (SPECS §3.2): instances carry an expando slot.
    pub is_dynamic: bool,
    /// Namespaced members declared in this class (reflection table for
    /// runtime-computed `obj.q::name` qualification).
    pub ns_members: Vec<NsMemberInfo>,
    /// Static field storage types.
    pub statics: Vec<Ty>,
}

/// Where a captured variable lives in the defining frame (closure
/// conversion).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapSrc {
    /// Cell of a local in the enclosing frame.
    ParentLocal(LocalId),
    /// Capture slot of the enclosing frame.
    ParentCapture(usize),
}

/// One lowered function.
#[derive(Debug)]
pub struct Function {
    /// Symbol-friendly name (informational; codegen derives real symbols).
    pub name: String,
    /// Instance methods/constructors receive `this` (class index) as an
    /// implicit first parameter (before `locals[0]`).
    pub this_class: Option<u32>,
    /// Return type.
    pub ret: Ty,
    /// All local slots; the first `param_count` are parameters.
    pub locals: Vec<Ty>,
    /// Number of leading locals that are parameters.
    pub param_count: usize,
    /// Locals captured by nested closures (cell-backed storage).
    pub captured: Vec<bool>,
    /// Environment slots this function receives (free variables).
    pub captures: Vec<CapSrc>,
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
#[derive(Debug, Clone)]
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
    /// `throw e` (value boxed).
    Throw(Expr),
    /// `try/catch/finally` (SPECS §3.8; setjmp scheme documented in the
    /// backend).
    Try {
        body: Vec<Stmt>,
        catches: Vec<Catch>,
        finally: Option<Vec<Stmt>>,
    },
    Empty,
}

/// One catch clause: matched by `is` against the binding's type
/// (untyped = catch-all `*`).
#[derive(Debug, Clone)]
pub struct Catch {
    /// Local receiving the exception (its type is the match target).
    pub binding: LocalId,
    /// Handler body.
    pub body: Vec<Stmt>,
}

/// One `switch` clause (bodies fall through; `break` exits).
#[derive(Debug, Clone)]
pub struct Case {
    /// `None` = `default:`.
    pub test: Option<Expr>,
    /// Clause body.
    pub body: Vec<Stmt>,
}

/// A lowered, typed expression.
#[derive(Debug, Clone)]
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
    EncodeUriComponent,
    DecodeUriComponent,
    Escape,
    Unescape,
}

/// RegExp instance operations (ES3 §15.10.6) plus the String methods that
/// take a RegExp (§15.5.4.10-12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum RegexOp {
    /// `re.test(s)` → Boolean.
    Test,
    /// `re.exec(s)` → Array | null (advances lastIndex when global).
    Exec,
    /// `re.toString()` / display form `/source/flags`.
    ToString,
    Source,
    Global,
    IgnoreCase,
    Multiline,
    LastIndex,
    /// `s.match(re)` → Array | null.
    Match,
    /// `s.search(re)` → int.
    Search,
    /// `s.replace(re, repl)` → String ($&/$1..$9 substitutions).
    Replace,
}

/// Date instance operations, funneled through the runtime's indexed
/// accessor/formatter like avmplus (core/Date.cpp getDateProperty /
/// Date::toString).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum DateFn {
    /// Indexed getter: 0 time, 1-8 local components, 9-16 UTC, 17 tz.
    Get(u32),
    /// setTime(ms) → clipped value.
    SetTime,
    /// Indexed string form: 0 toString, 1 toDateString, 2 toTimeString,
    /// 6 toUTCString (avmplus numbering).
    Format(u32),
}

/// One namespaced member for the runtime reflection table (member names
/// arrive from sema mangled `#ns{id}::raw`; lowering splits them).
#[derive(Debug)]
pub struct NsMemberInfo {
    /// Namespace id (index into [`Program::namespace_uris`]).
    pub ns: u32,
    /// Raw (unmangled) member name.
    pub name: String,
    /// Field slot index (`Class::slots`) — mutually exclusive with vslot.
    pub field_slot: Option<u32>,
    /// Field type (present with `field_slot`; drives boxing).
    pub field_ty: Option<Ty>,
    /// Vtable slot for methods.
    pub vslot: Option<u32>,
}

/// Socket instance operations (SPECS §6 I/O; runtime socket.rs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum SocketOp {
    Write,
    ReadLine,
    Read,
    Close,
    Accept,
    LocalPort,
}

/// String instance methods with runtime implementations (SPECS §6, P3 set;
/// signatures per avmplus core/String.as).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum StrMethod {
    /// `split(delim, limit)` → Array (§15.5.4.14).
    Split,
    /// `replace(search, repl)` — string pattern, first occurrence
    /// (§15.5.4.11; regex patterns are P8).
    Replace,
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

/// Array methods with runtime implementations (SPECS §6 P5 surface).
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
    /// `some(callback)` (named to avoid clashing with Option::Some in
    /// glob-importing backends).
    SomeM,
    Every,
}

/// Vector methods.
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
    /// AVM2 `coerce` to a class: null/undefined → null, matching object →
    /// pointer, else TypeError (abort until P6 exceptions).
    AnyToObject(u32),
    /// Same, interface target.
    AnyToIface(u32),
    /// Checked coercion to Array.
    AnyToArray,
    /// Checked coercion to a Vector instantiation.
    AnyToVector(u32),
    /// Checked coercion to RegExp.
    AnyToRegExp,
    /// Checked coercion to Date.
    AnyToDate,
    /// Checked coercion to Socket/ServerSocket.
    AnyToSocket,
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
#[derive(Debug, Clone)]
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
    /// Increment/decrement of a static field.
    StaticIncDec {
        class: u32,
        index: usize,
        is_inc: bool,
        is_prefix: bool,
    },
    /// Increment/decrement of a captured variable.
    CaptureIncDec {
        slot: usize,
        is_inc: bool,
        is_prefix: bool,
    },
    /// Increment/decrement of an instance field.
    FieldIncDec {
        recv: Box<Expr>,
        class: u32,
        slot: usize,
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
    /// `this` (implicit first parameter of methods).
    This,
    /// Allocate + default-init + run constructor.
    New(u32, Vec<Expr>),
    /// Field access: (receiver, class owning the slot, slot index).
    FieldGet(Box<Expr>, u32, usize),
    FieldSet(Box<Expr>, u32, usize, Box<Expr>),
    /// Vtable dispatch (receiver's static class, vtable slot).
    CallVirtual {
        recv: Box<Expr>,
        class: u32,
        vslot: usize,
        args: Vec<Expr>,
    },
    /// Interface-table dispatch. `ret` is the callee's declared return
    /// type (the expression's own `ty` differs for setter calls, whose
    /// value is the assigned operand).
    CallIface {
        recv: Box<Expr>,
        iface: u32,
        islot: usize,
        ret: Ty,
        args: Vec<Expr>,
    },
    /// Statically bound method call (`super.m`, constructor chains).
    CallDirect {
        fn_id: FnId,
        recv: Box<Expr>,
        args: Vec<Expr>,
    },
    /// Static field storage access.
    StaticGet(u32, usize),
    StaticSet(u32, usize, Box<Expr>),
    /// `[a, , b]` — holes become undefined.
    ArrayLit(Vec<Option<Expr>>),
    /// `new <T>[...]` (elements already element-coerced).
    VectorLit(u32, Vec<Expr>),
    /// Array method dispatch (args boxed per the sema signature).
    CallArr(ArrMethod, Box<Expr>, Vec<Expr>),
    /// Vector method dispatch.
    CallVec(VecMethod, Box<Expr>, Vec<Expr>),
    /// Array/Vector length read (receiver type discriminates).
    SeqLen(Box<Expr>),
    /// Array/Vector length write.
    SeqSetLen(Box<Expr>, Box<Expr>),
    /// `seq[i]` read (index already ToNumber). The `bool` is the P24
    /// bounds-check-elimination flag: when `true`, a preceding loop guard has
    /// proven the index in range, so codegen emits an unchecked load. Only
    /// ever set by the `bce` pass on unboxed numeric Vectors.
    SeqGet(Box<Expr>, Box<Expr>, bool),
    /// `seq[i] = v` (value already element-coerced). `bool` = BCE-unchecked
    /// (see [`ExprKind::SeqGet`]).
    SeqSet(Box<Expr>, Box<Expr>, Box<Expr>, bool),
    /// Captured-variable access (index into this function's `captures`).
    CaptureGet(usize),
    CaptureSet(usize, Box<Expr>),
    /// Closure over `fn_id`, environment built from the current frame.
    Closure(FnId),
    /// Plain function/static/builtin as a Function value (no environment).
    FnValue(FnId),
    BuiltinValue(Builtin),
    /// `obj.method` bound-method closure (SPECS §3.7).
    BoundMethod(Box<Expr>, u32, usize),
    /// Indirect call of a Function/`*` value (boxed ABI); `is_apply`
    /// spreads an Array argument.
    CallFnValue {
        callee: Box<Expr>,
        this_arg: Option<Box<Expr>>,
        args: Vec<Expr>,
        is_apply: bool,
    },
    /// `for..in`/`for each..in` enumeration helpers over a boxed receiver:
    /// element count, key at index, value at index.
    EnumLen(Box<Expr>),
    EnumKey(Box<Expr>, Box<Expr>),
    EnumValue(Box<Expr>, Box<Expr>),
    /// `{a: 1, ...}` — a fresh dynamic Object instance (SPECS §3.2).
    ObjectLit(Vec<(String, Expr)>),
    /// Dynamic property ops on boxed receivers.
    PropGet(Box<Expr>, String),
    PropSet(Box<Expr>, String, Box<Expr>),
    AnyIndexGet(Box<Expr>, Box<Expr>),
    AnyIndexSet(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `o.m(args)` through a boxed receiver (property → Function → call
    /// bound to the receiver).
    PropCall(Box<Expr>, String, Vec<Expr>),
    /// `key in obj` (§11.8.7).
    HasProp(Box<Expr>, Box<Expr>),
    /// `delete obj.name` (§11.4.1).
    DeleteProp(Box<Expr>, String),
    /// Native static call (Math/System/File/JSON/Date; emission strategy in
    /// the backend).
    CallNative(SemaNativeFn, Vec<Expr>),
    /// Regex literal: (pattern source, flags) — compiles a fresh RegExp
    /// per evaluation (throws SyntaxError on a bad pattern).
    RegExpLit(String, String),
    /// `new RegExp(pattern, flags)`; args are Strings.
    NewRegExp(Vec<Expr>),
    /// RegExp/String regex operation; operands per [`RegexOp`] docs.
    CallRegex(RegexOp, Vec<Expr>),
    /// `new Date(...)`: 0-7 Number components (§15.9.3).
    NewDate(Vec<Expr>),
    /// Date instance operation; receiver is operand 0.
    CallDate(DateFn, Vec<Expr>),
    /// Socket instance operation; receiver is operand 0.
    CallSocket(SocketOp, Vec<Expr>),
    /// A namespace value: index into [`Program::namespace_uris`].
    NamespaceVal(u32),
    /// `new Namespace(uri)`; the arg is a String.
    NewNamespace(Box<Expr>),
    /// Runtime-qualified read `obj.q::name` → boxed value (operands:
    /// receiver object, Namespace qualifier); the name is inline.
    NsGet(Box<Expr>, Box<Expr>, String),
    /// Runtime-qualified call `obj.q::name(args)` → boxed result
    /// (operands: receiver, qualifier, then the boxed args).
    NsCall(Box<Expr>, Box<Expr>, String, Vec<Expr>),
    /// `q.uri` / `q.toString()` — a namespace's canonical URI.
    NsUri(Box<Expr>),
}
