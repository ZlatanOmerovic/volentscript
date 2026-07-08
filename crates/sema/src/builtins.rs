//! The P2 builtin surface: global functions/constants and members of the
//! core primitive types.
//!
//! Signatures are taken from the reference implementation's own `.as`
//! declarations — `docs/avmplus/core/actionscript.lang.as`,
//! `docs/avmplus/core/String.as`, `docs/avmplus/core/Number.as`,
//! `docs/avmplus/shell/shell_toplevel.as` — cited per item. The stdlib
//! phase tags are SPECS §6 (P3 items only, here).

use crate::ty::Ty;

/// A builtin global function known to the checker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum BuiltinFn {
    /// `trace(...s)` — avmplus shell_toplevel.as:283 declares it untyped;
    /// SPECS §6 fixes our surface to "→ stdout + newline", return void
    /// (SPECS takes precedence, SPECS §2).
    Trace,
    /// `parseInt(s:String = "NaN", radix:int = 0):Number` —
    /// actionscript.lang.as:49.
    ParseInt,
    /// `parseFloat(str:String = "NaN"):Number` — actionscript.lang.as:53.
    ParseFloat,
    /// `isNaN(n:Number):Boolean` — actionscript.lang.as:41.
    IsNaN,
    /// `isFinite(n:Number):Boolean` — actionscript.lang.as:45.
    IsFinite,
    /// `encodeURIComponent(uri:String):String` — actionscript.lang.as:37.
    EncodeUriComponent,
    /// `decodeURIComponent(uri:String):String`.
    DecodeUriComponent,
    /// `escape(s:String):String` — actionscript.lang.as:63.
    Escape,
    /// `unescape(s:String):String` — actionscript.lang.as:67.
    Unescape,
}

/// A builtin function signature: parameter types, minimum required argument
/// count, whether it accepts unlimited trailing args, and the return type.
#[derive(Debug)]
pub struct Signature {
    /// Fixed parameter types, in order.
    pub params: &'static [Ty],
    /// Arguments before the first defaulted parameter.
    pub required: usize,
    /// Accepts `...rest` beyond `params`.
    pub variadic: bool,
    /// Result type.
    pub ret: Ty,
}

impl BuiltinFn {
    /// The checker-visible signature.
    pub fn signature(self) -> Signature {
        match self {
            BuiltinFn::Trace => Signature {
                params: &[],
                required: 0,
                variadic: true,
                ret: Ty::Void,
            },
            BuiltinFn::ParseInt => Signature {
                params: &[Ty::String, Ty::Int],
                required: 0,
                variadic: false,
                ret: Ty::Number,
            },
            BuiltinFn::ParseFloat => Signature {
                params: &[Ty::String],
                required: 0,
                variadic: false,
                ret: Ty::Number,
            },
            BuiltinFn::IsNaN | BuiltinFn::IsFinite => Signature {
                params: &[Ty::Number],
                required: 0,
                variadic: false,
                ret: Ty::Boolean,
            },
            BuiltinFn::EncodeUriComponent
            | BuiltinFn::DecodeUriComponent
            | BuiltinFn::Escape
            | BuiltinFn::Unescape => Signature {
                params: &[Ty::String],
                required: 1,
                variadic: false,
                ret: Ty::String,
            },
        }
    }

    /// Source-level name.
    pub fn name(self) -> &'static str {
        match self {
            BuiltinFn::Trace => "trace",
            BuiltinFn::ParseInt => "parseInt",
            BuiltinFn::ParseFloat => "parseFloat",
            BuiltinFn::IsNaN => "isNaN",
            BuiltinFn::IsFinite => "isFinite",
            BuiltinFn::EncodeUriComponent => "encodeURIComponent",
            BuiltinFn::DecodeUriComponent => "decodeURIComponent",
            BuiltinFn::Escape => "escape",
            BuiltinFn::Unescape => "unescape",
        }
    }

    /// Looks a builtin up by source name.
    pub fn lookup(name: &str) -> Option<BuiltinFn> {
        Some(match name {
            "trace" => BuiltinFn::Trace,
            "parseInt" => BuiltinFn::ParseInt,
            "parseFloat" => BuiltinFn::ParseFloat,
            "isNaN" => BuiltinFn::IsNaN,
            "isFinite" => BuiltinFn::IsFinite,
            "encodeURIComponent" => BuiltinFn::EncodeUriComponent,
            "decodeURIComponent" => BuiltinFn::DecodeUriComponent,
            "escape" => BuiltinFn::Escape,
            "unescape" => BuiltinFn::Unescape,
            _ => return None,
        })
    }
}

/// Global constants (ECMA-262 3rd ed. §15.1.1: NaN, Infinity, undefined).
pub fn global_const(name: &str) -> Option<Ty> {
    match name {
        "NaN" | "Infinity" => Some(Ty::Number),
        "undefined" => Some(Ty::Any),
        _ => None,
    }
}

/// Core type names usable in annotations and as `is`/`as` operands.
pub fn type_name(name: &str) -> Option<Ty> {
    Some(match name {
        "int" => Ty::Int,
        "uint" => Ty::UInt,
        "Number" => Ty::Number,
        "Boolean" => Ty::Boolean,
        "String" => Ty::String,
        "Function" => Ty::Function,
        "RegExp" => Ty::RegExp,
        "Date" => Ty::Date,
        "Socket" => Ty::Socket,
        "ServerSocket" => Ty::ServerSocket,
        "Namespace" => Ty::Namespace,
        _ => return None,
    })
}

/// Native static functions backing the P7 stdlib classes (Math, System,
/// File, JSON, Date.now). Emission strategy lives in codegen; signatures
/// here. Math per ECMA-262 3rd ed. §15.8; System/File are the
/// Redtamarin-shaped CLI surface SPECS §6 calls for (not `flash.*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum NativeFn {
    MathAbs,
    MathCeil,
    MathFloor,
    MathRound,
    MathSqrt,
    MathPow,
    MathExp,
    MathLog,
    MathSin,
    MathCos,
    MathTan,
    MathAsin,
    MathAcos,
    MathAtan,
    MathAtan2,
    MathMin,
    MathMax,
    MathRandom,
    SystemArgs,
    SystemExit,
    SystemGc,
    SystemGcLiveBytes,
    SystemGetenv,
    SystemTime,
    FileRead,
    FileWrite,
    FileExists,
    JsonStringify,
    JsonParse,
    DateNow,
    DateUTC,
    SocketConnect,
    ServerSocketBind,
}

/// One native static method.
pub struct NativeMethod {
    /// Source-level name.
    pub name: &'static str,
    /// Which native.
    pub func: NativeFn,
    /// Signature.
    pub sig: Signature,
}

/// A native static constant.
pub struct NativeConst {
    /// Source-level name.
    pub name: &'static str,
    /// Value (all are Numbers).
    pub value: f64,
}

const fn nsig(params: &'static [Ty], required: usize, variadic: bool, ret: Ty) -> Signature {
    Signature {
        params,
        required,
        variadic,
        ret,
    }
}

/// Methods of a native static class, by class name.
pub fn native_methods(class: &str) -> Option<&'static [NativeMethod]> {
    use NativeFn::*;
    const N: Ty = Ty::Number;
    static MATH: &[NativeMethod] = &[
        NativeMethod {
            name: "abs",
            func: MathAbs,
            sig: nsig(&[N], 1, false, N),
        },
        NativeMethod {
            name: "ceil",
            func: MathCeil,
            sig: nsig(&[N], 1, false, N),
        },
        NativeMethod {
            name: "floor",
            func: MathFloor,
            sig: nsig(&[N], 1, false, N),
        },
        NativeMethod {
            name: "round",
            func: MathRound,
            sig: nsig(&[N], 1, false, N),
        },
        NativeMethod {
            name: "sqrt",
            func: MathSqrt,
            sig: nsig(&[N], 1, false, N),
        },
        NativeMethod {
            name: "pow",
            func: MathPow,
            sig: nsig(&[N, N], 2, false, N),
        },
        NativeMethod {
            name: "exp",
            func: MathExp,
            sig: nsig(&[N], 1, false, N),
        },
        NativeMethod {
            name: "log",
            func: MathLog,
            sig: nsig(&[N], 1, false, N),
        },
        NativeMethod {
            name: "sin",
            func: MathSin,
            sig: nsig(&[N], 1, false, N),
        },
        NativeMethod {
            name: "cos",
            func: MathCos,
            sig: nsig(&[N], 1, false, N),
        },
        NativeMethod {
            name: "tan",
            func: MathTan,
            sig: nsig(&[N], 1, false, N),
        },
        NativeMethod {
            name: "asin",
            func: MathAsin,
            sig: nsig(&[N], 1, false, N),
        },
        NativeMethod {
            name: "acos",
            func: MathAcos,
            sig: nsig(&[N], 1, false, N),
        },
        NativeMethod {
            name: "atan",
            func: MathAtan,
            sig: nsig(&[N], 1, false, N),
        },
        NativeMethod {
            name: "atan2",
            func: MathAtan2,
            sig: nsig(&[N, N], 2, false, N),
        },
        // min/max: 2+ args, folded pairwise in codegen (§15.8.2.11/12).
        NativeMethod {
            name: "min",
            func: MathMin,
            sig: nsig(&[N, N], 2, true, N),
        },
        NativeMethod {
            name: "max",
            func: MathMax,
            sig: nsig(&[N, N], 2, true, N),
        },
        NativeMethod {
            name: "random",
            func: MathRandom,
            sig: nsig(&[], 0, false, N),
        },
    ];
    static SYSTEM: &[NativeMethod] = &[
        NativeMethod {
            name: "args",
            func: SystemArgs,
            sig: nsig(&[], 0, false, Ty::Array),
        },
        NativeMethod {
            name: "exit",
            func: SystemExit,
            sig: nsig(&[Ty::Int], 1, false, Ty::Void),
        },
        NativeMethod {
            name: "getenv",
            func: SystemGetenv,
            sig: nsig(&[Ty::String], 1, false, Ty::String),
        },
        NativeMethod {
            name: "time",
            func: SystemTime,
            sig: nsig(&[], 0, false, N),
        },
        NativeMethod {
            name: "gc",
            func: SystemGc,
            sig: nsig(&[], 0, false, Ty::Void),
        },
        NativeMethod {
            name: "gcLiveBytes",
            func: SystemGcLiveBytes,
            sig: nsig(&[], 0, false, N),
        },
    ];
    static FILE: &[NativeMethod] = &[
        NativeMethod {
            name: "read",
            func: FileRead,
            sig: nsig(&[Ty::String], 1, false, Ty::String),
        },
        NativeMethod {
            name: "write",
            func: FileWrite,
            sig: nsig(&[Ty::String, Ty::String], 2, false, Ty::Boolean),
        },
        NativeMethod {
            name: "exists",
            func: FileExists,
            sig: nsig(&[Ty::String], 1, false, Ty::Boolean),
        },
    ];
    static JSON_M: &[NativeMethod] = &[
        NativeMethod {
            name: "stringify",
            func: JsonStringify,
            sig: nsig(&[Ty::Any], 1, false, Ty::String),
        },
        NativeMethod {
            name: "parse",
            func: JsonParse,
            sig: nsig(&[Ty::String], 1, false, Ty::Any),
        },
    ];
    static DATE: &[NativeMethod] = &[
        NativeMethod {
            name: "now",
            func: DateNow,
            sig: nsig(&[], 0, false, N),
        },
        // §15.9.4.3: year+month required, day/h/min/s/ms optional.
        NativeMethod {
            name: "UTC",
            func: DateUTC,
            sig: nsig(&[N, N, N, N, N, N, N], 2, false, N),
        },
    ];
    static SOCKET: &[NativeMethod] = &[NativeMethod {
        name: "connect",
        func: SocketConnect,
        sig: nsig(&[Ty::String, Ty::Int], 2, false, Ty::Socket),
    }];
    static SERVER_SOCKET: &[NativeMethod] = &[NativeMethod {
        name: "bind",
        func: ServerSocketBind,
        sig: nsig(&[Ty::Int], 1, false, Ty::ServerSocket),
    }];
    Some(match class {
        "Math" => MATH,
        "System" => SYSTEM,
        "File" => FILE,
        "JSON" => JSON_M,
        "Date" => DATE,
        "Socket" => SOCKET,
        "ServerSocket" => SERVER_SOCKET,
        _ => return None,
    })
}

/// Constants of a native static class (§15.8.1).
pub fn native_consts(class: &str) -> Option<&'static [NativeConst]> {
    static MATH_C: &[NativeConst] = &[
        NativeConst {
            name: "PI",
            value: std::f64::consts::PI,
        },
        NativeConst {
            name: "E",
            value: std::f64::consts::E,
        },
        NativeConst {
            name: "LN10",
            value: std::f64::consts::LN_10,
        },
        NativeConst {
            name: "LN2",
            value: std::f64::consts::LN_2,
        },
        NativeConst {
            name: "LOG10E",
            value: std::f64::consts::LOG10_E,
        },
        NativeConst {
            name: "LOG2E",
            value: std::f64::consts::LOG2_E,
        },
        NativeConst {
            name: "SQRT1_2",
            value: std::f64::consts::FRAC_1_SQRT_2,
        },
        NativeConst {
            name: "SQRT2",
            value: std::f64::consts::SQRT_2,
        },
    ];
    match class {
        "Math" => Some(MATH_C),
        _ => None,
    }
}

/// Whether a name is a native static class (unshadowed).
pub fn is_native_class(name: &str) -> bool {
    matches!(
        name,
        "Math" | "System" | "File" | "JSON" | "Date" | "Socket" | "ServerSocket"
    )
}

/// A member (property or method) of a builtin type.
#[derive(Debug)]
pub enum Member {
    /// Read-only property.
    Property(Ty),
    /// Method: signature.
    Method(Signature),
}

/// Looks up a member on a core-type receiver. `None` = no such member —
/// sealed-class semantics make that a compile error (SPECS §3.2).
///
/// Signatures per avmplus core/String.as and core/Number.as (lines cited on
/// each entry). `split` returns `Array`, which lands in P5 — typed `*` until
/// then.
pub fn member(receiver: Ty, name: &str) -> Option<Member> {
    use Member::*;
    let m = match (receiver, name) {
        // String.as:43
        (Ty::String, "length") => Property(Ty::Int),
        // String.as:63
        (Ty::String, "charAt") => Method(sig(&[Ty::Number], 0, Ty::String)),
        // String.as:71
        (Ty::String, "charCodeAt") => Method(sig(&[Ty::Number], 0, Ty::Number)),
        // String.as:47
        (Ty::String, "indexOf") => Method(sig(&[Ty::String, Ty::Number], 0, Ty::Int)),
        // String.as:55
        (Ty::String, "lastIndexOf") => Method(sig(&[Ty::String, Ty::Number], 0, Ty::Int)),
        // String.as:79 — variadic
        (Ty::String, "concat") => Method(Signature {
            params: &[],
            required: 0,
            variadic: true,
            ret: Ty::String,
        }),
        // String.as:141
        (Ty::String, "slice") => Method(sig(&[Ty::Number, Ty::Number], 0, Ty::String)),
        // String.as:168
        (Ty::String, "substring") => Method(sig(&[Ty::Number, Ty::Number], 0, Ty::String)),
        // String.as:176
        (Ty::String, "substr") => Method(sig(&[Ty::Number, Ty::Number], 0, Ty::String)),
        // String.as:151
        (Ty::String, "split") => Method(sig(&[Ty::Any, Ty::Any], 0, Ty::Array)),
        // §15.5.4.11 — pattern is a String or a RegExp (checked at the
        // call site; the parameter is `*` to admit both).
        (Ty::String, "replace") => Method(sig(&[Ty::Any, Ty::String], 2, Ty::String)),
        // AS3 String.match/search take a RegExp (String.as:87,133).
        (Ty::String, "match") => Method(sig(&[Ty::RegExp], 1, Ty::Array)),
        (Ty::String, "search") => Method(sig(&[Ty::RegExp], 1, Ty::Int)),

        // RegExp (ES3 §15.10.6, AS3 RegExp class). `exec` returns the
        // match Array or null.
        (Ty::RegExp, "test") => Method(sig(&[Ty::String], 1, Ty::Boolean)),
        (Ty::RegExp, "exec") => Method(sig(&[Ty::String], 1, Ty::Array)),
        (Ty::RegExp, "toString") => Method(sig(&[], 0, Ty::String)),
        (Ty::RegExp, "source") => Property(Ty::String),
        (Ty::RegExp, "global") => Property(Ty::Boolean),
        (Ty::RegExp, "ignoreCase") => Property(Ty::Boolean),
        (Ty::RegExp, "multiline") => Property(Ty::Boolean),
        // Read-only in v1 (the runtime advances it on global exec).
        (Ty::RegExp, "lastIndex") => Property(Ty::Int),

        // Date (ES3 §15.9.5, avmplus core/Date.as AS3 surface). Component
        // setters and locale forms are backlog; setTime is the mutator.
        (
            Ty::Date,
            "getTime" | "valueOf" | "getFullYear" | "getMonth" | "getDate" | "getDay" | "getHours"
            | "getMinutes" | "getSeconds" | "getMilliseconds" | "getUTCFullYear" | "getUTCMonth"
            | "getUTCDate" | "getUTCDay" | "getUTCHours" | "getUTCMinutes" | "getUTCSeconds"
            | "getUTCMilliseconds" | "getTimezoneOffset",
        ) => Method(sig(&[], 0, Ty::Number)),
        (Ty::Date, "setTime") => Method(sig(&[Ty::Number], 1, Ty::Number)),
        (Ty::Date, "toString" | "toDateString" | "toTimeString" | "toUTCString") => {
            Method(sig(&[], 0, Ty::String))
        }

        // Sockets (SPECS §6 I/O). readLine/read return null at EOF.
        (Ty::Socket, "write") => Method(sig(&[Ty::String], 1, Ty::Void)),
        (Ty::Socket, "readLine") => Method(sig(&[], 0, Ty::String)),
        (Ty::Socket, "read") => Method(sig(&[Ty::Int], 0, Ty::String)),
        (Ty::Socket | Ty::ServerSocket, "close") => Method(sig(&[], 0, Ty::Void)),
        (Ty::Socket | Ty::ServerSocket, "localPort") => Property(Ty::Int),
        (Ty::ServerSocket, "accept") => Method(sig(&[], 0, Ty::Socket)),

        // Namespace values (ES4 draft; SPECS §5 P16).
        (Ty::Namespace, "uri") => Property(Ty::String),
        (Ty::Namespace, "toString") => Method(sig(&[], 0, Ty::String)),
        // String.as:182,194
        (Ty::String, "toLowerCase") => Method(sig(&[], 0, Ty::String)),
        (Ty::String, "toUpperCase") => Method(sig(&[], 0, Ty::String)),
        // String.as:206
        (Ty::String, "toString") => Method(sig(&[], 0, Ty::String)),

        // Number.as:98/176/246 — toString(radix = 10), radix untyped
        (Ty::Number | Ty::Int | Ty::UInt, "toString") => Method(sig(&[Ty::Any], 0, Ty::String)),
        // Number.as:146/216/286 — toFixed(p = 0)
        (Ty::Number | Ty::Int | Ty::UInt, "toFixed") => Method(sig(&[Ty::Any], 0, Ty::String)),
        // Number.as:122/198/268
        (Ty::Number | Ty::Int | Ty::UInt, "toExponential") => {
            Method(sig(&[Ty::Any], 0, Ty::String))
        }
        // Number.as:131/207/277
        (Ty::Number | Ty::Int | Ty::UInt, "toPrecision") => Method(sig(&[Ty::Any], 0, Ty::String)),
        // Number.as:101/179/249 — valueOf():receiver type
        (Ty::Number | Ty::Int | Ty::UInt, "valueOf") => Method(sig(&[], 0, receiver)),

        (Ty::Boolean, "toString") => Method(sig(&[], 0, Ty::String)),
        (Ty::Boolean, "valueOf") => Method(sig(&[], 0, Ty::Boolean)),
        _ => return None,
    };
    Some(m)
}

fn sig(params: &'static [Ty], required: usize, ret: Ty) -> Signature {
    Signature {
        params,
        required,
        variadic: false,
        ret,
    }
}
