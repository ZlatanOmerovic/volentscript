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
        _ => return None,
    })
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
