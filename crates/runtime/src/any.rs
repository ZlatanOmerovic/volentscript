//! The boxed dynamic value: what a `*`-typed slot holds.

use crate::string::VsString;

/// Runtime type tag of a boxed value. Layout is ABI: codegen emits these
/// numbers — append only.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum Tag {
    Undefined = 0,
    Null = 1,
    Boolean = 2,
    Int = 3,
    UInt = 4,
    Number = 5,
    String = 6,
    /// Class instance; payload = object pointer (header = descriptor ptr).
    Object = 7,
    /// Array; payload = VsArray pointer.
    Array = 8,
    /// Vector.<T>; payload = VsVector pointer.
    Vector = 9,
    /// Function value; payload = VsClosure pointer.
    Function = 10,
    /// RegExp; payload = VsRegExp pointer.
    RegExp = 11,
    /// Date; payload = VsDate pointer.
    Date = 12,
    /// Socket / ServerSocket; payload = VsSocket pointer.
    Socket = 13,
    /// Namespace value; payload = VsNamespace pointer (interned).
    Namespace = 14,
}

/// A boxed dynamic value (`*`). 16 bytes, passed/returned by value.
/// `data` holds: Boolean 0/1, Int sext, UInt zext, Number f64 bits,
/// String pointer bits; unused otherwise.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VsAny {
    /// Type tag.
    pub tag: u32,
    /// Payload bits.
    pub data: u64,
}

impl VsAny {
    /// The `undefined` value.
    pub const UNDEFINED: VsAny = VsAny {
        tag: Tag::Undefined as u32,
        data: 0,
    };
    /// The `null` value.
    pub const NULL: VsAny = VsAny {
        tag: Tag::Null as u32,
        data: 0,
    };

    /// Boxes a Number.
    pub fn number(v: f64) -> VsAny {
        VsAny {
            tag: Tag::Number as u32,
            data: v.to_bits(),
        }
    }

    /// Boxes an int.
    pub fn int(v: i32) -> VsAny {
        VsAny {
            tag: Tag::Int as u32,
            data: v as i64 as u64,
        }
    }

    /// Boxes a uint.
    pub fn uint(v: u32) -> VsAny {
        VsAny {
            tag: Tag::UInt as u32,
            data: u64::from(v),
        }
    }

    /// Boxes a Boolean.
    pub fn boolean(v: bool) -> VsAny {
        VsAny {
            tag: Tag::Boolean as u32,
            data: u64::from(v),
        }
    }

    /// Boxes a String pointer (null pointer boxes as `null`).
    pub fn string(ptr: *const VsString) -> VsAny {
        if ptr.is_null() {
            VsAny::NULL
        } else {
            VsAny {
                tag: Tag::String as u32,
                data: ptr as usize as u64,
            }
        }
    }

    /// Boxes an object pointer (null boxes as `null`).
    pub fn object(ptr: *const u8) -> VsAny {
        if ptr.is_null() {
            VsAny::NULL
        } else {
            VsAny {
                tag: Tag::Object as u32,
                data: ptr as usize as u64,
            }
        }
    }

    /// Object payload (only when `tag == Object`).
    pub fn as_object_ptr(&self) -> *const u8 {
        self.data as usize as *const u8
    }

    /// Boxes an Array pointer (null → `null`).
    pub fn array(ptr: *const crate::seq::VsArray) -> VsAny {
        if ptr.is_null() {
            VsAny::NULL
        } else {
            VsAny {
                tag: Tag::Array as u32,
                data: ptr as usize as u64,
            }
        }
    }

    /// Boxes a Vector pointer (null → `null`).
    pub fn vector(ptr: *const crate::seq::VsVector) -> VsAny {
        if ptr.is_null() {
            VsAny::NULL
        } else {
            VsAny {
                tag: Tag::Vector as u32,
                data: ptr as usize as u64,
            }
        }
    }

    /// Array payload.
    pub fn as_array_ptr(&self) -> *const crate::seq::VsArray {
        self.data as usize as *const crate::seq::VsArray
    }

    /// Vector payload.
    pub fn as_vector_ptr(&self) -> *const crate::seq::VsVector {
        self.data as usize as *const crate::seq::VsVector
    }

    /// Boxes a Function value (null → `null`).
    pub fn function(ptr: *const crate::closure::VsClosure) -> VsAny {
        if ptr.is_null() {
            VsAny::NULL
        } else {
            VsAny {
                tag: Tag::Function as u32,
                data: ptr as usize as u64,
            }
        }
    }

    /// Function payload.
    pub fn as_closure_ptr(&self) -> *const crate::closure::VsClosure {
        self.data as usize as *const crate::closure::VsClosure
    }

    /// The tag, decoded.
    pub fn tag(&self) -> Tag {
        match self.tag {
            1 => Tag::Null,
            2 => Tag::Boolean,
            3 => Tag::Int,
            4 => Tag::UInt,
            5 => Tag::Number,
            6 => Tag::String,
            7 => Tag::Object,
            8 => Tag::Array,
            9 => Tag::Vector,
            10 => Tag::Function,
            11 => Tag::RegExp,
            12 => Tag::Date,
            13 => Tag::Socket,
            14 => Tag::Namespace,
            _ => Tag::Undefined,
        }
    }

    /// String payload (only when `tag == String`).
    pub fn as_string_ptr(&self) -> *const VsString {
        self.data as usize as *const VsString
    }

    /// Numeric payload interpreted per tag; `None` for non-numeric tags.
    pub fn numeric(&self) -> Option<f64> {
        match self.tag() {
            Tag::Int => Some(self.data as i64 as i32 as f64),
            Tag::UInt => Some((self.data as u32) as f64),
            Tag::Number => Some(f64::from_bits(self.data)),
            _ => None,
        }
    }
}
