//! The P2 core type universe.
//!
//! Classes, interfaces, and generics widen this in P4/P5; the enum becomes an
//! interned `TypeId` table then. Keeping it a plain enum now avoids premature
//! machinery while the set is closed.

use std::fmt;

use crate::classes::{ClassId, IfaceId};

/// A semantic type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ty {
    /// Instance of a registered class (nominal identity, SPECS §4.5).
    Class(ClassId),
    /// Value typed by a registered interface.
    Iface(IfaceId),
    /// The Array class (SPECS §3.10): dynamic length, `*` elements.
    Array,
    /// `Vector.<T>` (SPECS §4.3): typed dense vector; payload indexes the
    /// checker's instantiation table (reified — each element type is a
    /// distinct runtime type).
    Vector(u32),
    /// 32-bit signed integer (SPECS §3.3).
    Int,
    /// 32-bit unsigned integer.
    UInt,
    /// IEEE-754 double.
    Number,
    /// `true`/`false`.
    Boolean,
    /// Immutable UTF-16 string; a reference type (nullable in AS3).
    String,
    /// No value (function returns only).
    Void,
    /// `*` — the any type: dynamic escape hatch (SPECS §3.11).
    Any,
    /// The type of the `null` literal (bottom of reference types).
    Null,
    /// ES3 §15.10 RegExp: reference type, engine-backed (SPECS §6).
    RegExp,
    /// ES3 §15.9 Date: reference type wrapping one time value (SPECS §6).
    Date,
    /// TCP stream socket (SPECS §6 I/O — Redtamarin-shaped, not flash.net).
    Socket,
    /// TCP listener (`ServerSocket.bind` / `accept`).
    ServerSocket,
    /// First-class namespace value (ES4 draft; URI identity, SPECS §5).
    Namespace,
    /// The AS3 `Function` type: callable, signature unchecked
    /// (AS3's `Function` class carries no signature).
    Function,
    /// Produced after an error; compatible with everything to suppress
    /// cascading diagnostics.
    Error,
}

impl Ty {
    /// int/uint/Number.
    pub fn is_numeric(self) -> bool {
        matches!(self, Ty::Int | Ty::UInt | Ty::Number)
    }

    /// Types whose storage is a reference and can hold `null` in AS3
    /// (P5 null-safety narrows this; see SPECS §4.1).
    pub fn is_reference(self) -> bool {
        matches!(
            self,
            Ty::String
                | Ty::Function
                | Ty::Any
                | Ty::Null
                | Ty::Class(_)
                | Ty::Iface(_)
                | Ty::Array
                | Ty::Vector(_)
                | Ty::RegExp
                | Ty::Date
                | Ty::Socket
                | Ty::ServerSocket
                | Ty::Namespace
        )
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            // Bare ids; diagnostics use `Checker::ty_name` for real names.
            Ty::Class(id) => return write!(f, "class#{}", id.0),
            Ty::Iface(id) => return write!(f, "interface#{}", id.0),
            Ty::Array => "Array",
            Ty::Vector(id) => return write!(f, "Vector#{id}"),
            Ty::Int => "int",
            Ty::UInt => "uint",
            Ty::Number => "Number",
            Ty::Boolean => "Boolean",
            Ty::String => "String",
            Ty::Void => "void",
            Ty::Any => "*",
            Ty::Null => "null",
            Ty::Function => "Function",
            Ty::RegExp => "RegExp",
            Ty::Date => "Date",
            Ty::Socket => "Socket",
            Ty::ServerSocket => "ServerSocket",
            Ty::Namespace => "Namespace",
            Ty::Error => "<error>",
        })
    }
}
