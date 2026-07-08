//! The C ABI surface codegen emits calls to, plus the process entry shim.
//!
//! ABI conventions (mirrored by the LLVM backend — keep in sync):
//! - Strings are `*const VsString`; null pointer = AS3 `null`.
//! - Boxed `*` values cross the boundary **by pointer** (out-params for
//!   returns) — aggregate by-value ABIs differ between Rust and hand-built
//!   LLVM IR, pointers do not.
//! - Booleans are `u32` (0/1) — avoids the i1/i8 mismatch.
//!
//! Every function is `unsafe extern "C"`; safety contracts are the pointer
//! rules above. Rust-side logic lives in the safe `conv`/`string` modules.

use crate::any::{Tag, VsAny};
use crate::conv;
use crate::string::{self, VsString};

fn tag_from(raw: u32) -> Tag {
    match raw {
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

/// Reads a string argument, aborting like AS3's null-dereference TypeError
/// when null (exceptions are P6).
///
/// # Safety
/// `s` must be null or a live `VsString`.
unsafe fn require<'a>(s: *const VsString, what: &str) -> &'a VsString {
    // SAFETY: caller contract.
    match unsafe { string::deref(s) } {
        Some(r) => r,
        None => conv::type_error(&format!("null reference in {what}")),
    }
}

// --- entry ------------------------------------------------------------------
// Compiled out of `cargo test` builds: the test harness has its own `main`,
// and `vs_script` only exists in codegen-produced objects.

#[cfg(not(test))]
unsafe extern "C" {
    /// The compiled script body (function 0 of the MIR program).
    fn vs_script();
}

/// Process entry: initialize, run the script, flush, exit 0 (SPECS §7).
///
/// # Safety
/// Called by the C runtime startup exactly once.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn main(argc: i32, _argv: *const *const u8) -> i32 {
    // The deepest stack address the collector must scan to (SPECS §7 GC).
    crate::gc::record_stack_base(&argc as *const i32 as *const u8);
    // SAFETY: vs_script is emitted by codegen in every linked program.
    unsafe { vs_script() };
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    0
}

// --- strings -----------------------------------------------------------------

/// Builds a runtime string from UTF-8 bytes (used for literals).
///
/// # Safety
/// `ptr..ptr+len` must be valid UTF-8 bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_string_from_utf8(ptr: *const u8, len: u32) -> *const VsString {
    // SAFETY: caller contract.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    let s = std::str::from_utf8(bytes).unwrap_or("\u{FFFD}");
    VsString::from_rust(s)
}

/// `a + b` where either side is a String: ToString(null) = "null"
/// (ES3 §9.8, §11.6.1).
///
/// # Safety
/// Pointers null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_string_concat(
    a: *const VsString,
    b: *const VsString,
) -> *const VsString {
    // SAFETY: caller contract.
    let (a, b) = unsafe { (string::deref(a), string::deref(b)) };
    let mut units: Vec<u16> = Vec::new();
    match a {
        Some(s) => units.extend_from_slice(s.units()),
        None => units.extend("null".encode_utf16()),
    }
    match b {
        Some(s) => units.extend_from_slice(s.units()),
        None => units.extend("null".encode_utf16()),
    }
    VsString::alloc(units)
}

/// String equality for `==`/`===` (§11.9.6: code-unit sequences).
///
/// # Safety
/// Pointers null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_string_equals(a: *const VsString, b: *const VsString) -> u32 {
    // SAFETY: caller contract.
    let (a, b) = unsafe { (string::deref(a), string::deref(b)) };
    u32::from(match (a, b) {
        (Some(x), Some(y)) => x.units() == y.units(),
        (None, None) => true,
        _ => false,
    })
}

/// Relational compare (§11.8.5). `op`: 0 `<`, 1 `>`, 2 `<=`, 3 `>=`.
/// A null side is the Null value, not a string: both-strings lexicographic
/// compare applies only when both are non-null; otherwise numeric compare
/// (ToNumber(null) = 0) — matching avmplus atom semantics.
///
/// # Safety
/// Pointers null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_string_cmp(a: *const VsString, b: *const VsString, op: u32) -> u32 {
    // SAFETY: caller contract.
    let (da, db) = unsafe { (string::deref(a), string::deref(b)) };
    let ordering = match (da, db) {
        (Some(x), Some(y)) => x.units().cmp(y.units()),
        (x, y) => {
            let nx = x.map_or(0.0, |s| conv::string_to_number(&s.to_rust()));
            let ny = y.map_or(0.0, |s| conv::string_to_number(&s.to_rust()));
            match nx.partial_cmp(&ny) {
                Some(o) => o,
                None => return 0, // NaN involved: all relations false (§11.8.5)
            }
        }
    };
    u32::from(match op {
        0 => ordering.is_lt(),
        1 => ordering.is_gt(),
        2 => ordering.is_le(),
        _ => ordering.is_ge(),
    })
}

/// `String#length` (String.as:43).
///
/// # Safety
/// Pointer null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_string_length(s: *const VsString) -> i32 {
    // SAFETY: caller contract.
    unsafe { require(s, "String.length") }.len as i32
}

/// ToNumber(String) (§9.3.1); null → 0.
///
/// # Safety
/// Pointer null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_string_to_number(s: *const VsString) -> f64 {
    // SAFETY: caller contract.
    match unsafe { string::deref(s) } {
        Some(s) => conv::string_to_number(&s.to_rust()),
        None => 0.0,
    }
}

macro_rules! str_method {
    ($(#[$doc:meta])* $name:ident, |$s:ident $(, $arg:ident : $ty:ty)*| $body:expr) => {
        $(#[$doc])*
        /// # Safety
        /// String pointers null or live.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(this: *const VsString $(, $arg: $ty)*) -> *const VsString {
            // SAFETY: caller contract.
            let $s = unsafe { require(this, stringify!($name)) };
            $body
        }
    };
}

str_method!(
    /// `charAt` (§15.5.4.4).
    vs_str_char_at, |s, i: f64| {
        let i = i as i64;
        if i < 0 || i >= i64::from(s.len) {
            VsString::alloc(Vec::new())
        } else {
            VsString::alloc(vec![s.units()[i as usize]])
        }
    }
);

str_method!(
    /// `toLowerCase` (§15.5.4.16) — UTF-16-native (P26).
    vs_str_to_lower, |s| VsString::alloc(string::case_units(s.units(), false))
);

str_method!(
    /// `toUpperCase` (§15.5.4.18) — UTF-16-native (P26).
    vs_str_to_upper, |s| VsString::alloc(string::case_units(s.units(), true))
);

str_method!(
    /// `slice` (§15.5.4.13): negative indices count from the end.
    vs_str_slice, |s, start: f64, end: f64| {
        let len = i64::from(s.len);
        let norm = |v: f64| -> i64 {
            let v = v as i64;
            if v < 0 { (len + v).max(0) } else { v.min(len) }
        };
        let (a, b) = (norm(start), norm(end));
        VsString::alloc(if a < b { s.units()[a as usize..b as usize].to_vec() } else { Vec::new() })
    }
);

str_method!(
    /// `substring` (§15.5.4.15): clamps and swaps.
    vs_str_substring, |s, start: f64, end: f64| {
        let len = i64::from(s.len);
        let clamp = |v: f64| -> i64 {
            if v.is_nan() || v < 0.0 { 0 } else { (v as i64).min(len) }
        };
        let (mut a, mut b) = (clamp(start), clamp(end));
        if a > b {
            std::mem::swap(&mut a, &mut b);
        }
        VsString::alloc(s.units()[a as usize..b as usize].to_vec())
    }
);

str_method!(
    /// `substr` (§B.2.3 / String.as:176): negative start counts back.
    vs_str_substr, |s, start: f64, count: f64| {
        let len = i64::from(s.len);
        let start = {
            let v = start as i64;
            if v < 0 { (len + v).max(0) } else { v.min(len) }
        };
        let count = (count as i64).max(0).min(len - start);
        VsString::alloc(s.units()[start as usize..(start + count) as usize].to_vec())
    }
);

/// `charCodeAt` (§15.5.4.5): NaN when out of range.
///
/// # Safety
/// Pointer null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_str_char_code_at(this: *const VsString, i: f64) -> f64 {
    // SAFETY: caller contract.
    let s = unsafe { require(this, "charCodeAt") };
    let i = i as i64;
    if i < 0 || i >= i64::from(s.len) {
        f64::NAN
    } else {
        f64::from(s.units()[i as usize])
    }
}

/// `indexOf` (§15.5.4.7). Null needle matches like the string "null" would
/// not — AS3 stringifies the needle at the callsite (sema coerced it).
///
/// # Safety
/// Pointers null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_str_index_of(
    this: *const VsString,
    needle: *const VsString,
    start: f64,
) -> i32 {
    // SAFETY: caller contract.
    let s = unsafe { require(this, "indexOf") };
    let Some(needle) = (unsafe { string::deref(needle) }) else {
        return -1;
    };
    let hay = s.units();
    let nee = needle.units();
    let from = (start.max(0.0) as usize).min(hay.len());
    if nee.is_empty() {
        return from as i32;
    }
    hay[from..]
        .windows(nee.len())
        .position(|w| w == nee)
        .map_or(-1, |p| (from + p) as i32)
}

/// `lastIndexOf` (§15.5.4.8).
///
/// # Safety
/// Pointers null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_str_last_index_of(
    this: *const VsString,
    needle: *const VsString,
    start: f64,
) -> i32 {
    // SAFETY: caller contract.
    let s = unsafe { require(this, "lastIndexOf") };
    let Some(needle) = (unsafe { string::deref(needle) }) else {
        return -1;
    };
    let hay = s.units();
    let nee = needle.units();
    let limit = if start.is_nan() {
        hay.len()
    } else {
        (start.max(0.0) as usize).min(hay.len())
    };
    if nee.is_empty() {
        return limit.min(hay.len()) as i32;
    }
    let mut best = -1i64;
    for p in 0..=hay.len().saturating_sub(nee.len()) {
        if p > limit {
            break;
        }
        if &hay[p..p + nee.len()] == nee {
            best = p as i64;
        }
    }
    best as i32
}

/// `String#toString` — identity, but null receiver still faults (§15.5.4.2).
///
/// # Safety
/// Pointer null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_str_to_string(this: *const VsString) -> *const VsString {
    // SAFETY: caller contract.
    unsafe { require(this, "toString") };
    this
}

// --- numbers ------------------------------------------------------------------

/// ToInt32 (§9.5).
#[unsafe(no_mangle)]
pub extern "C" fn vs_f64_to_int32(v: f64) -> i32 {
    conv::to_int32(v)
}

/// ToUint32 (§9.6).
#[unsafe(no_mangle)]
pub extern "C" fn vs_f64_to_uint32(v: f64) -> u32 {
    conv::to_uint32(v)
}

/// ToString(Number) (§9.8.1).
#[unsafe(no_mangle)]
pub extern "C" fn vs_num_to_string(v: f64) -> *const VsString {
    VsString::from_rust(&conv::number_to_string(v))
}

/// `Number#toString(radix)` (Number.as:98).
#[unsafe(no_mangle)]
pub extern "C" fn vs_num_to_string_radix(v: f64, radix: f64) -> *const VsString {
    VsString::from_rust(&conv::number_to_string_radix(v, conv::to_int32(radix)))
}

/// `Number#toFixed` (§15.7.4.5).
#[unsafe(no_mangle)]
pub extern "C" fn vs_num_to_fixed(v: f64, digits: f64) -> *const VsString {
    VsString::from_rust(&conv::number_to_fixed(v, conv::to_int32(digits)))
}

/// ToString(Boolean) (§9.8).
#[unsafe(no_mangle)]
pub extern "C" fn vs_bool_to_string(v: u32) -> *const VsString {
    VsString::from_rust(if v != 0 { "true" } else { "false" })
}

// --- boxed values ----------------------------------------------------------------

/// ToNumber (§9.3).
///
/// # Safety
/// `v` must point to a live `VsAny`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_to_number(v: *const VsAny) -> f64 {
    // SAFETY: caller contract.
    conv::any_to_number(unsafe { *v })
}

/// ToString (§9.8): undefined → "undefined", null → "null".
///
/// # Safety
/// `v` must point to a live `VsAny`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_to_string(v: *const VsAny) -> *const VsString {
    // SAFETY: caller contract.
    VsString::from_rust(&conv::any_to_display(unsafe { *v }))
}

/// AVM2 `coerce_s`: null/undefined → null String, else ToString.
///
/// # Safety
/// `v` must point to a live `VsAny`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_coerce_string(v: *const VsAny) -> *const VsString {
    // SAFETY: caller contract.
    let v = unsafe { *v };
    match v.tag() {
        Tag::Undefined | Tag::Null => std::ptr::null(),
        Tag::String => v.as_string_ptr(),
        _ => VsString::from_rust(&conv::any_to_display(v)),
    }
}

/// ToBoolean (§9.2).
///
/// # Safety
/// `v` must point to a live `VsAny`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_truthy(v: *const VsAny) -> u32 {
    // SAFETY: caller contract.
    u32::from(conv::any_truthy(unsafe { *v }))
}

/// `typeof` (§11.4.3).
///
/// # Safety
/// `v` must point to a live `VsAny`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_typeof(v: *const VsAny) -> *const VsString {
    // SAFETY: caller contract.
    VsString::from_rust(conv::any_typeof(unsafe { *v }))
}

/// `is` against a core type (see `conv::any_is`).
///
/// # Safety
/// `v` must point to a live `VsAny`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_is(v: *const VsAny, tag: u32) -> u32 {
    // SAFETY: caller contract.
    u32::from(conv::any_is(unsafe { *v }, tag_from(tag)))
}

/// `as`: the value when `is` holds, else `null` (SPECS §3.1).
///
/// # Safety
/// `v` and `out` must point to live `VsAny`s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_as(v: *const VsAny, tag: u32, out: *mut VsAny) {
    // SAFETY: caller contract.
    unsafe {
        *out = if conv::any_is(*v, tag_from(tag)) {
            *v
        } else {
            VsAny::NULL
        };
    }
}

/// `x as String` → pointer or null.
///
/// # Safety
/// `v` must point to a live `VsAny`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_as_string(v: *const VsAny) -> *const VsString {
    // SAFETY: caller contract.
    let v = unsafe { *v };
    if v.tag() == Tag::String {
        v.as_string_ptr()
    } else {
        std::ptr::null()
    }
}

/// Strict equality (§11.9.6).
///
/// # Safety
/// `a`/`b` must point to live `VsAny`s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_strict_equals(a: *const VsAny, b: *const VsAny) -> u32 {
    // SAFETY: caller contract.
    u32::from(conv::any_strict_equals(unsafe { *a }, unsafe { *b }))
}

/// Loose equality (§11.9.3): undefined == null; numbers/strings/booleans
/// compare after conversion.
///
/// # Safety
/// `a`/`b` must point to live `VsAny`s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_equals(a: *const VsAny, b: *const VsAny) -> u32 {
    // SAFETY: caller contract.
    let (a, b) = unsafe { (*a, *b) };
    let undef_or_null = |t: Tag| matches!(t, Tag::Undefined | Tag::Null);
    let same_tag = a.tag() == b.tag();
    let result = if undef_or_null(a.tag()) || undef_or_null(b.tag()) {
        undef_or_null(a.tag()) && undef_or_null(b.tag())
    } else if a.tag() == Tag::String && b.tag() == Tag::String {
        conv::any_strict_equals(a, b)
    } else if same_tag
        && matches!(
            a.tag(),
            Tag::Object
                | Tag::Array
                | Tag::Vector
                | Tag::Function
                | Tag::RegExp
                | Tag::Date
                | Tag::Socket
                | Tag::Namespace
        )
    {
        // Reference identity (§11.9.3 step 13); namespaces are interned by
        // URI, so pointer identity IS URI identity (ES4).
        a.data == b.data
    } else {
        // Mixed/numeric: compare as numbers (§11.9.3 steps 5-21 collapse to
        // ToNumber on both sides for the primitive-only P3 universe).
        conv::any_to_number(a) == conv::any_to_number(b)
    };
    u32::from(result)
}

/// `+` on boxed values (§11.6.1): if either side is a String, concatenate
/// ToString of both; otherwise numeric addition. (No objects/valueOf in the
/// P3 universe.)
///
/// # Safety
/// `a`/`b`/`out` must point to live `VsAny`s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_add(a: *const VsAny, b: *const VsAny, out: *mut VsAny) {
    // SAFETY: caller contract.
    let (a, b) = unsafe { (*a, *b) };
    let result = if a.tag() == Tag::String || b.tag() == Tag::String {
        let s = format!("{}{}", conv::any_to_display(a), conv::any_to_display(b));
        VsAny::string(VsString::from_rust(&s))
    } else {
        VsAny::number(conv::any_to_number(a) + conv::any_to_number(b))
    };
    // SAFETY: caller contract.
    unsafe { *out = result };
}

/// Relational compare on boxed values (§11.8.5): both Strings →
/// lexicographic, else ToNumber both (NaN → false). `op` as in
/// [`vs_string_cmp`].
///
/// # Safety
/// `a`/`b` must point to live `VsAny`s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_cmp(a: *const VsAny, b: *const VsAny, op: u32) -> u32 {
    // SAFETY: caller contract.
    let (a, b) = unsafe { (*a, *b) };
    if a.tag() == Tag::String && b.tag() == Tag::String {
        return unsafe { vs_string_cmp(a.as_string_ptr(), b.as_string_ptr(), op) };
    }
    let (x, y) = (conv::any_to_number(a), conv::any_to_number(b));
    let Some(ordering) = x.partial_cmp(&y) else {
        return 0; // NaN: all relations false
    };
    u32::from(match op {
        0 => ordering.is_lt(),
        1 => ordering.is_gt(),
        2 => ordering.is_le(),
        _ => ordering.is_ge(),
    })
}

// --- objects --------------------------------------------------------------------

use crate::object::{self, VsClassDesc};

/// Allocates a zeroed instance and stores its descriptor header.
/// (Zero bytes = int/uint 0, false, null refs, `*` undefined — exactly the
/// AS3 defaults except Number, which codegen sets to NaN after this call;
/// SPECS §3.11.)
///
/// # Safety
/// `desc` must be a static class descriptor; `size` its instance size.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_alloc_object(desc: *const VsClassDesc, size: u64) -> *mut u8 {
    // GC block, conservatively scanned (slots hold refs, numbers, anys).
    let p = crate::gc::alloc(size as usize, crate::gc::Kind::Raw);
    // SAFETY: word 0 is the header (layout contract).
    unsafe { *(p as *mut *const VsClassDesc) = desc };
    p
}

/// `obj is Class` (SPECS §3.1: real runtime test against class identity).
///
/// # Safety
/// `obj` null or live; `desc` static.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_obj_is_class(obj: *const u8, desc: *const VsClassDesc) -> u32 {
    u32::from(object::is_class(obj, desc))
}

/// `obj is Interface`.
///
/// # Safety
/// `obj` null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_obj_is_iface(obj: *const u8, iface_id: u32) -> u32 {
    u32::from(object::is_iface(obj, iface_id))
}

/// Interface dispatch: the method table for `iface_id`. Aborts on null
/// receivers (TypeError semantics until P6) — a missing table cannot
/// happen on sema-checked programs.
///
/// # Safety
/// `obj` null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_iface_table(obj: *const u8, iface_id: u32) -> *const *const u8 {
    if obj.is_null() {
        conv::type_error("null reference in interface method call");
    }
    object::iface_table(obj, iface_id)
        .unwrap_or_else(|| conv::type_error("object does not implement the interface"))
}

/// Null-receiver guard for virtual calls/field access.
///
/// # Safety
/// Always safe; diverges on null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_null_check(obj: *const u8) {
    if obj.is_null() {
        conv::type_error("null reference (property access on null object)");
    }
}

/// ToString for a class instance (used by string concatenation).
///
/// # Safety
/// `obj` null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_object_to_string(obj: *const u8) -> *const VsString {
    VsString::from_rust(&object::object_to_display(obj))
}

/// Boxed `is Class`.
///
/// # Safety
/// `v` live; `desc` static.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_is_class(v: *const VsAny, desc: *const VsClassDesc) -> u32 {
    // SAFETY: caller contract.
    u32::from(object::any_is_class(unsafe { *v }, desc))
}

/// Boxed `is Interface`.
///
/// # Safety
/// `v` live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_is_iface(v: *const VsAny, iface_id: u32) -> u32 {
    // SAFETY: caller contract.
    let v = unsafe { *v };
    u32::from(v.tag() == Tag::Object && object::is_iface(v.as_object_ptr(), iface_id))
}

/// Boxed `as Class` → pointer or null.
///
/// # Safety
/// `v` live; `desc` static.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_as_class(v: *const VsAny, desc: *const VsClassDesc) -> *const u8 {
    // SAFETY: caller contract.
    let v = unsafe { *v };
    if object::any_is_class(v, desc) {
        v.as_object_ptr()
    } else {
        std::ptr::null()
    }
}

/// Boxed `as Interface` → pointer or null.
///
/// # Safety
/// `v` live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_as_iface(v: *const VsAny, iface_id: u32) -> *const u8 {
    // SAFETY: caller contract.
    let v = unsafe { *v };
    if v.tag() == Tag::Object && object::is_iface(v.as_object_ptr(), iface_id) {
        v.as_object_ptr()
    } else {
        std::ptr::null()
    }
}

/// AVM2 `coerce` to class (checked; aborts on mismatch until P6).
///
/// # Safety
/// `v` live; `desc` static.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_coerce_class(
    v: *const VsAny,
    desc: *const VsClassDesc,
) -> *const u8 {
    // SAFETY: caller contract.
    object::any_coerce_class(unsafe { *v }, desc)
}

/// AVM2 `coerce` to interface.
///
/// # Safety
/// `v` live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_coerce_iface(v: *const VsAny, iface_id: u32) -> *const u8 {
    // SAFETY: caller contract.
    object::any_coerce_iface(unsafe { *v }, iface_id)
}

// --- builtins -----------------------------------------------------------------

/// `trace(...args)`: ToString each argument, join with spaces, newline to
/// stdout (SPECS §6; avmplus shell semantics).
///
/// # Safety
/// `args..args+argc` must be live `VsAny`s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_trace(argc: u32, args: *const VsAny) {
    // SAFETY: caller contract.
    let args = unsafe { std::slice::from_raw_parts(args, argc as usize) };
    let line = args
        .iter()
        .map(|a| conv::any_to_display(*a))
        .collect::<Vec<_>>()
        .join(" ");
    println!("{line}");
}

/// parseInt (§15.1.2.2); null string → NaN (ToString(null)="null" parses to
/// NaN anyway).
///
/// # Safety
/// Pointer null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_parse_int(s: *const VsString, radix: i32) -> f64 {
    // SAFETY: caller contract.
    match unsafe { string::deref(s) } {
        Some(s) => conv::parse_int(&s.to_rust(), radix),
        None => f64::NAN,
    }
}

/// parseFloat (§15.1.2.3).
///
/// # Safety
/// Pointer null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_parse_float(s: *const VsString) -> f64 {
    // SAFETY: caller contract.
    match unsafe { string::deref(s) } {
        Some(s) => conv::parse_float(&s.to_rust()),
        None => f64::NAN,
    }
}

// --- arrays & vectors --------------------------------------------------------

use crate::seq::{self, VsArray, VsVector};

/// Builds an Array from staged boxed elements.
///
/// # Safety
/// `args..args+argc` live `VsAny`s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_array_new(argc: u32, args: *const VsAny) -> *const VsArray {
    // SAFETY: caller contract.
    let items = unsafe { std::slice::from_raw_parts(args, argc as usize) };
    seq::new_array(items.to_vec())
}

/// Builds a Vector from staged boxed elements (already element-coerced).
///
/// # Safety
/// `args..args+argc` live `VsAny`s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_vector_new(
    inst: u32,
    kind: u32,
    argc: u32,
    args: *const VsAny,
) -> *const VsVector {
    // SAFETY: caller contract.
    let items = unsafe { std::slice::from_raw_parts(args, argc as usize) };
    seq::new_vector(inst, kind, items.to_vec())
}

/// # Safety
/// `arr` live.
unsafe fn arr<'x>(a: *const VsArray, what: &str) -> &'x VsArray {
    if a.is_null() {
        conv::type_error(&format!("null reference in {what}"));
    }
    // SAFETY: non-null arrays are live.
    unsafe { &*a }
}

/// # Safety
/// `v` live.
unsafe fn vec_ref<'x>(v: *const VsVector, what: &str) -> &'x VsVector {
    if v.is_null() {
        conv::type_error(&format!("null reference in {what}"));
    }
    // SAFETY: non-null vectors are live.
    unsafe { &*v }
}

/// # Safety
/// `v` live; the caller holds no other reference to it (single-threaded).
unsafe fn vec_mut<'x>(v: *const VsVector, what: &str) -> &'x mut VsVector {
    if v.is_null() {
        conv::type_error(&format!("null reference in {what}"));
    }
    // SAFETY: non-null vectors are live; VS is single-threaded so the
    // exclusive borrow is sound for the duration of one runtime call.
    unsafe { &mut *(v as *mut VsVector) }
}

/// `Array#length` / element count.
///
/// # Safety
/// Pointer null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_arr_len(a: *const VsArray) -> u32 {
    // SAFETY: caller contract.
    unsafe { arr(a, "Array.length") }.data.borrow().len() as u32
}

/// `Array#length = n` (§15.4.5.2: truncate or extend with holes).
///
/// # Safety
/// Pointer null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_arr_set_len(a: *const VsArray, n: u32) {
    // SAFETY: caller contract.
    unsafe { arr(a, "Array.length") }
        .data
        .borrow_mut()
        .resize(n as usize, VsAny::UNDEFINED);
}

/// `arr[i]` read: out-of-range → undefined (§15.4).
///
/// # Safety
/// Pointers live; `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_arr_get(a: *const VsArray, index: f64, out: *mut VsAny) {
    // SAFETY: caller contract.
    let a = unsafe { arr(a, "Array index") };
    let data = a.data.borrow();
    let v = if index >= 0.0 && (index as usize) < data.len() {
        data[index as usize]
    } else {
        VsAny::UNDEFINED
    };
    // SAFETY: caller contract.
    unsafe { *out = v };
}

/// `arr[i] = v`: extends with holes when i >= length.
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_arr_set(a: *const VsArray, index: f64, v: *const VsAny) {
    // SAFETY: caller contract.
    let a = unsafe { arr(a, "Array index") };
    if index < 0.0 {
        return; // negative indices are expando keys — dynamic props are P7
    }
    let i = index as usize;
    let mut data = a.data.borrow_mut();
    if i >= data.len() {
        data.resize(i + 1, VsAny::UNDEFINED);
    }
    // SAFETY: caller contract.
    data[i] = unsafe { *v };
}

/// push (§15.4.4.7) — returns new length.
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_arr_push(a: *const VsArray, argc: u32, args: *const VsAny) -> u32 {
    // SAFETY: caller contract.
    let a = unsafe { arr(a, "push") };
    let items = unsafe { std::slice::from_raw_parts(args, argc as usize) };
    let mut data = a.data.borrow_mut();
    data.extend_from_slice(items);
    data.len() as u32
}

/// pop (§15.4.4.6).
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_arr_pop(a: *const VsArray, out: *mut VsAny) {
    // SAFETY: caller contract.
    let v = unsafe { arr(a, "pop") }
        .data
        .borrow_mut()
        .pop()
        .unwrap_or(VsAny::UNDEFINED);
    unsafe { *out = v };
}

/// shift (§15.4.4.9).
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_arr_shift(a: *const VsArray, out: *mut VsAny) {
    // SAFETY: caller contract.
    let a = unsafe { arr(a, "shift") };
    let mut data = a.data.borrow_mut();
    let v = if data.is_empty() {
        VsAny::UNDEFINED
    } else {
        data.remove(0)
    };
    unsafe { *out = v };
}

/// unshift (§15.4.4.13) — returns new length.
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_arr_unshift(a: *const VsArray, argc: u32, args: *const VsAny) -> u32 {
    // SAFETY: caller contract.
    let a = unsafe { arr(a, "unshift") };
    let items = unsafe { std::slice::from_raw_parts(args, argc as usize) };
    let mut data = a.data.borrow_mut();
    for (i, it) in items.iter().enumerate() {
        data.insert(i, *it);
    }
    data.len() as u32
}

/// slice (§15.4.4.10).
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_arr_slice(a: *const VsArray, start: f64, end: f64) -> *const VsArray {
    // SAFETY: caller contract.
    let a = unsafe { arr(a, "slice") };
    let data = a.data.borrow();
    let (s, e) = (
        seq::norm_index(start, data.len()),
        seq::norm_index(end, data.len()),
    );
    seq::new_array(if s < e {
        data[s..e].to_vec()
    } else {
        Vec::new()
    })
}

/// splice (§15.4.4.12) — returns removed elements; inserts `args`.
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_arr_splice(
    a: *const VsArray,
    start: f64,
    delete_count: f64,
    argc: u32,
    args: *const VsAny,
) -> *const VsArray {
    // SAFETY: caller contract.
    let a = unsafe { arr(a, "splice") };
    let items = unsafe { std::slice::from_raw_parts(args, argc as usize) };
    let mut data = a.data.borrow_mut();
    let s = seq::norm_index(start, data.len());
    let del = delete_count.max(0.0) as usize;
    let e = (s + del).min(data.len());
    let removed: Vec<VsAny> = data.splice(s..e, items.iter().copied()).collect();
    seq::new_array(removed)
}

/// indexOf (AS3 Array): strict equality search.
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_arr_index_of(
    a: *const VsArray,
    needle: *const VsAny,
    from: f64,
) -> i32 {
    // SAFETY: caller contract.
    let a = unsafe { arr(a, "indexOf") };
    let needle = unsafe { *needle };
    let data = a.data.borrow();
    let from = seq::norm_index(from, data.len());
    data[from..]
        .iter()
        .position(|v| conv::any_strict_equals(*v, needle))
        .map_or(-1, |p| (from + p) as i32)
}

/// concat (§15.4.4.4): array arguments flatten one level.
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_arr_concat(
    a: *const VsArray,
    argc: u32,
    args: *const VsAny,
) -> *const VsArray {
    // SAFETY: caller contract.
    let a = unsafe { arr(a, "concat") };
    let items = unsafe { std::slice::from_raw_parts(args, argc as usize) };
    let mut out = a.data.borrow().clone();
    for it in items {
        if it.tag() == Tag::Array {
            // SAFETY: Array-tagged payloads hold live VsArrays.
            out.extend_from_slice(&unsafe { &*it.as_array_ptr() }.data.borrow());
        } else {
            out.push(*it);
        }
    }
    seq::new_array(out)
}

/// join (§15.4.4.3).
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_arr_join(a: *const VsArray, sep: *const VsString) -> *const VsString {
    // SAFETY: caller contract.
    let a = unsafe { arr(a, "join") };
    // SAFETY: caller contract.
    let sep: &[u16] = unsafe { string::deref(sep) }.map_or(&[b',' as u16], VsString::units);
    // P26: join UTF-16 units directly (string elements are copied, not
    // transcoded).
    VsString::alloc(seq::join_units(&a.data.borrow(), sep))
}

/// reverse (§15.4.4.8) — in place, returns the array.
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_arr_reverse(a: *const VsArray) -> *const VsArray {
    // SAFETY: caller contract.
    unsafe { arr(a, "reverse") }.data.borrow_mut().reverse();
    a
}

/// sort (§15.4.4.11, default comparator: ToString comparison).
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_arr_sort(a: *const VsArray) -> *const VsArray {
    // SAFETY: caller contract.
    unsafe { arr(a, "sort") }
        .data
        .borrow_mut()
        .sort_by_key(|v| conv::any_to_display(*v));
    a
}

/// Vector length.
///
/// # Safety
/// Pointer null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_vec_len(v: *const VsVector) -> u32 {
    // SAFETY: caller contract.
    unsafe { vec_ref(v, "Vector.length") }.len
}

/// Vector length set (extends with the element-kind zero value).
///
/// # Safety
/// Pointer null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_vec_set_len(v: *const VsVector, n: u32) {
    // SAFETY: caller contract.
    unsafe { vec_mut(v, "Vector.length") }.resize(n);
}

/// `vec[i]` read: out of range → RangeError. This is the slow path; for
/// numeric kinds compiled code inlines the in-bounds load (P23) and only
/// calls here to raise on an out-of-range index.
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_vec_get(v: *const VsVector, index: f64, out: *mut VsAny) {
    // SAFETY: caller contract.
    let v = unsafe { vec_ref(v, "Vector index") };
    if index < 0.0 || index as usize >= v.len as usize {
        exc::throw_error(
            exc::ErrorKind::Range,
            &format!("vector index {index} out of range"),
        );
    }
    // SAFETY: caller contract; index proven in range.
    unsafe { *out = v.get_any(index as usize) };
}

/// `vec[i] = x`: i == length appends, beyond → RangeError (AS3 Vector).
/// Slow path; compiled code inlines the in-bounds numeric store (P23).
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_vec_set(v: *const VsVector, index: f64, value: *const VsAny) {
    // SAFETY: caller contract.
    let v = unsafe { vec_mut(v, "Vector index") };
    let i = index as usize;
    if index < 0.0 || i > v.len as usize {
        exc::throw_error(
            exc::ErrorKind::Range,
            &format!("vector index {index} out of range"),
        );
    }
    // SAFETY: caller contract.
    let value = unsafe { *value };
    if i == v.len as usize {
        v.push_any(value);
    } else {
        v.set_any(i, value);
    }
}

/// Vector push — returns new length.
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_vec_push(v: *const VsVector, argc: u32, args: *const VsAny) -> u32 {
    // SAFETY: caller contract.
    let v = unsafe { vec_mut(v, "push") };
    let items = unsafe { std::slice::from_raw_parts(args, argc as usize) };
    for &it in items {
        v.push_any(it);
    }
    v.len
}

/// Vector pop.
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_vec_pop(v: *const VsVector, out: *mut VsAny) {
    // SAFETY: caller contract.
    let value = unsafe { vec_mut(v, "pop") }
        .pop_any()
        .unwrap_or(VsAny::UNDEFINED);
    unsafe { *out = value };
}

/// Vector shift.
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_vec_shift(v: *const VsVector, out: *mut VsAny) {
    // SAFETY: caller contract.
    let v = unsafe { vec_mut(v, "shift") };
    let value = if v.len == 0 {
        VsAny::UNDEFINED
    } else {
        let mut items = v.to_boxed();
        let first = items.remove(0);
        v.refill(&items);
        first
    };
    unsafe { *out = value };
}

/// Vector unshift — returns new length.
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_vec_unshift(v: *const VsVector, argc: u32, args: *const VsAny) -> u32 {
    // SAFETY: caller contract.
    let v = unsafe { vec_mut(v, "unshift") };
    let items = unsafe { std::slice::from_raw_parts(args, argc as usize) };
    let mut merged = items.to_vec();
    merged.extend(v.to_boxed());
    v.refill(&merged);
    v.len
}

/// Vector slice — same instantiation.
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_vec_slice(v: *const VsVector, start: f64, end: f64) -> *const VsVector {
    // SAFETY: caller contract.
    let v = unsafe { vec_ref(v, "slice") };
    let len = v.len as usize;
    let (s, e) = (seq::norm_index(start, len), seq::norm_index(end, len));
    let elems: Vec<VsAny> = if s < e {
        (s..e).map(|i| v.get_any(i)).collect()
    } else {
        Vec::new()
    };
    seq::new_vector(v.inst, v.kind, elems)
}

/// Vector indexOf (strict equality).
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_vec_index_of(
    v: *const VsVector,
    needle: *const VsAny,
    from: f64,
) -> i32 {
    // SAFETY: caller contract.
    let v = unsafe { vec_ref(v, "indexOf") };
    let needle = unsafe { *needle };
    let from = seq::norm_index(from, v.len as usize);
    (from..v.len as usize)
        .find(|&i| conv::any_strict_equals(v.get_any(i), needle))
        .map_or(-1, |p| p as i32)
}

/// Vector join.
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_vec_join(v: *const VsVector, sep: *const VsString) -> *const VsString {
    // SAFETY: caller contract.
    let v = unsafe { vec_ref(v, "join") };
    // SAFETY: caller contract.
    let sep: &[u16] = unsafe { string::deref(sep) }.map_or(&[b',' as u16], VsString::units);
    VsString::alloc(seq::join_units(&v.to_boxed(), sep))
}

/// Vector reverse — in place, returns the vector.
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_vec_reverse(v: *const VsVector) -> *const VsVector {
    // SAFETY: caller contract.
    let vr = unsafe { vec_mut(v, "reverse") };
    let mut items = vr.to_boxed();
    items.reverse();
    vr.refill(&items);
    v
}

/// ToString for a bare Array value (concat contexts).
///
/// # Safety
/// Pointer null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_arr_to_string(a: *const VsArray) -> *const VsString {
    if a.is_null() {
        return std::ptr::null();
    }
    // SAFETY: caller contract.
    VsString::alloc(seq::join_units(&unsafe { &*a }.data.borrow(), &[b',' as u16]))
}

/// ToString for a bare Vector value.
///
/// # Safety
/// Pointer null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_vec_to_string(v: *const VsVector) -> *const VsString {
    if v.is_null() {
        return std::ptr::null();
    }
    // SAFETY: caller contract.
    VsString::alloc(seq::join_units(&unsafe { &*v }.to_boxed(), &[b',' as u16]))
}

/// Boxed `is Array`.
///
/// # Safety
/// `v` live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_is_array(v: *const VsAny) -> u32 {
    // SAFETY: caller contract.
    u32::from(unsafe { *v }.tag() == Tag::Array)
}

/// Boxed `is Vector.<inst>` — reified per instantiation (SPECS §4.2).
///
/// # Safety
/// `v` live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_is_vector(v: *const VsAny, inst: u32) -> u32 {
    // SAFETY: caller contract.
    let v = unsafe { *v };
    u32::from(v.tag() == Tag::Vector && unsafe { &*v.as_vector_ptr() }.inst == inst)
}

/// Boxed `as Array` → ptr or null.
///
/// # Safety
/// `v` live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_as_array(v: *const VsAny) -> *const VsArray {
    // SAFETY: caller contract.
    let v = unsafe { *v };
    if v.tag() == Tag::Array {
        v.as_array_ptr()
    } else {
        std::ptr::null()
    }
}

/// Boxed `as Vector.<inst>` → ptr or null.
///
/// # Safety
/// `v` live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_as_vector(v: *const VsAny, inst: u32) -> *const VsVector {
    // SAFETY: caller contract.
    let v = unsafe { *v };
    if v.tag() == Tag::Vector && unsafe { &*v.as_vector_ptr() }.inst == inst {
        v.as_vector_ptr()
    } else {
        std::ptr::null()
    }
}

/// AVM2 coerce to Array (checked).
///
/// # Safety
/// `v` live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_coerce_array(v: *const VsAny) -> *const VsArray {
    // SAFETY: caller contract.
    let v = unsafe { *v };
    match v.tag() {
        Tag::Null | Tag::Undefined => std::ptr::null(),
        Tag::Array => v.as_array_ptr(),
        _ => conv::type_error(&format!(
            "cannot convert {} to Array",
            conv::any_to_display(v)
        )),
    }
}

/// AVM2 coerce to Vector.<inst> (checked).
///
/// # Safety
/// `v` live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_coerce_vector(v: *const VsAny, inst: u32) -> *const VsVector {
    // SAFETY: caller contract.
    let v = unsafe { *v };
    match v.tag() {
        Tag::Null | Tag::Undefined => std::ptr::null(),
        Tag::Vector if unsafe { &*v.as_vector_ptr() }.inst == inst => v.as_vector_ptr(),
        _ => conv::type_error(&format!(
            "cannot convert {} to Vector",
            conv::any_to_display(v)
        )),
    }
}

/// `String#split` (§15.5.4.14, string separators; regex separators are P7).
///
/// # Safety
/// Pointers null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_str_split(
    this: *const VsString,
    delim: *const VsString,
    limit: f64,
) -> *const VsArray {
    // SAFETY: caller contract.
    let s = unsafe { require(this, "split") };
    let hay = s.units();
    let limit = if limit.is_nan() || limit < 0.0 {
        u32::MAX as usize
    } else {
        limit as usize
    };
    // P26: split on UTF-16 code units, no UTF-8 round-trip.
    // SAFETY: caller contract.
    let parts: Vec<VsAny> = match unsafe { string::deref(delim) } {
        None => vec![VsAny::string(VsString::alloc(hay.to_vec()))],
        // Empty separator splits into individual code units (§15.5.4.14).
        Some(d) if d.len == 0 => hay
            .iter()
            .take(limit)
            .map(|&u| VsAny::string(VsString::alloc(vec![u])))
            .collect(),
        Some(d) => {
            let dn = d.units();
            let mut parts = Vec::new();
            let mut cursor = 0usize;
            while parts.len() < limit {
                match string::find_units(hay, dn, cursor) {
                    Some(pos) => {
                        parts.push(VsAny::string(VsString::alloc(hay[cursor..pos].to_vec())));
                        cursor = pos + dn.len();
                    }
                    None => {
                        parts.push(VsAny::string(VsString::alloc(hay[cursor..].to_vec())));
                        break;
                    }
                }
            }
            parts
        }
    };
    seq::new_array(parts)
}

// --- closures, cells, exceptions, enumeration --------------------------------

use crate::closure::{self, VsClosure};
use crate::exc;
use crate::object::VsClassDesc as VsClassDescExc;

/// Heap cell for a captured variable (closure conversion, SPECS §3.7).
///
/// # Safety
/// `init` live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_cell_new(init: *const VsAny) -> *mut VsAny {
    let p = crate::gc::alloc(std::mem::size_of::<VsAny>(), crate::gc::Kind::Raw) as *mut VsAny;
    // SAFETY: caller contract; fresh block of exactly VsAny size.
    unsafe { p.write(*init) };
    p
}

/// Builds a Function value.
///
/// # Safety
/// `env..env+envc` live cell pointers; `wrapper` a codegen wrapper.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_closure_new(
    wrapper: *const u8,
    envc: u32,
    env: *const *mut VsAny,
    this: *const u8,
) -> *const VsClosure {
    // SAFETY: caller contract.
    let env = if envc == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(env, envc as usize) }.to_vec()
    };
    closure::new_closure(wrapper, env, this)
}

/// Invokes a Function value with boxed args.
///
/// # Safety
/// Pointers live; `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_closure_call(
    c: *const VsClosure,
    this_arg: *const u8,
    argc: u32,
    args: *const VsAny,
    out: *mut VsAny,
) {
    // SAFETY: caller contract.
    unsafe { closure::invoke(c, this_arg, argc, args, out) }
}

/// `apply`: spreads an Array argument.
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_closure_apply(
    c: *const VsClosure,
    this_arg: *const u8,
    args_array: *const VsArray,
    out: *mut VsAny,
) {
    let items: Vec<VsAny> = if args_array.is_null() {
        Vec::new()
    } else {
        // SAFETY: caller contract.
        unsafe { &*args_array }.data.borrow().clone()
    };
    // `items` lives on the Rust heap across the callback: keep GC off.
    let _gc = crate::gc::defer();
    // SAFETY: caller contract.
    unsafe { closure::invoke(c, this_arg, items.len() as u32, items.as_ptr(), out) }
}

/// Handler registration for compiled `try` (buffer from `_setjmp`).
///
/// # Safety
/// `buf` points at a live jmp_buf in the registering frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_push_handler(buf: *mut u8) {
    exc::push_handler(buf);
}

/// Pops the innermost handler (normal try exit).
///
/// # Safety
/// Balanced with `vs_push_handler`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_pop_handler() {
    exc::pop_handler();
}

/// Reads the in-flight exception into `out` (start of catch dispatch).
///
/// # Safety
/// `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_take_exception(out: *mut VsAny) {
    // SAFETY: caller contract.
    unsafe { *out = exc::take_current() };
}

/// `throw` (never returns).
///
/// # Safety
/// `v` live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_throw(v: *const VsAny) -> ! {
    // SAFETY: caller contract.
    exc::throw(unsafe { *v })
}

/// Startup registration of the prelude Error descriptors (order: Error,
/// TypeError, RangeError, ReferenceError, ArgumentError, SyntaxError).
///
/// # Safety
/// Arrays of `count` static descriptor pointers / sizes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_register_errors(
    count: u32,
    descs: *const *const VsClassDescExc,
    sizes: *const u64,
) {
    // SAFETY: caller contract.
    unsafe {
        exc::register_errors(
            std::slice::from_raw_parts(descs, count as usize),
            std::slice::from_raw_parts(sizes, count as usize),
        )
    }
}

/// Enumeration length for `for..in` over a boxed receiver: Arrays and
/// Vectors iterate; sealed objects/others yield nothing (SPECS §3.2).
///
/// # Safety
/// `v` live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_enum_len(v: *const VsAny) -> i32 {
    // SAFETY: caller contract.
    let v = unsafe { *v };
    match v.tag() {
        Tag::Array => unsafe { &*v.as_array_ptr() }.data.borrow().len() as i32,
        Tag::Vector => unsafe { &*v.as_vector_ptr() }.len as i32,
        _ => 0,
    }
}

/// `for..in` key at index (Array/Vector indices are int keys).
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_enum_key(_v: *const VsAny, index: i32, out: *mut VsAny) {
    // SAFETY: caller contract.
    unsafe { *out = VsAny::int(index) };
}

/// `for each..in` value at index.
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_enum_value(v: *const VsAny, index: i32, out: *mut VsAny) {
    // SAFETY: caller contract.
    let v = unsafe { *v };
    let value = match v.tag() {
        Tag::Array => unsafe { &*v.as_array_ptr() }
            .data
            .borrow()
            .get(index as usize)
            .copied()
            .unwrap_or(VsAny::UNDEFINED),
        Tag::Vector => {
            let vec = unsafe { &*v.as_vector_ptr() };
            if index >= 0 && (index as usize) < vec.len as usize {
                vec.get_any(index as usize)
            } else {
                VsAny::UNDEFINED
            }
        }
        _ => VsAny::UNDEFINED,
    };
    // SAFETY: caller contract.
    unsafe { *out = value };
}

/// Array sort with a comparator Function (§15.4.4.11).
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_arr_sort_with(
    a: *const VsArray,
    cmp: *const VsClosure,
) -> *const VsArray {
    // SAFETY: caller contract.
    let arr = unsafe { arr(a, "sort") };
    let mut data = arr.data.borrow_mut();
    // sort_by stages elements in scratch buffers the GC can't see: defer.
    let _gc = crate::gc::defer();
    data.sort_by(|x, y| {
        let args = [*x, *y];
        let mut out = VsAny::UNDEFINED;
        // SAFETY: comparator is a live Function value.
        unsafe { closure::invoke(cmp, std::ptr::null(), 2, args.as_ptr(), &mut out) };
        let n = conv::any_to_number(out);
        if n < 0.0 {
            std::cmp::Ordering::Less
        } else if n > 0.0 {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    });
    a
}

/// AVM2 coerce to Function (checked).
///
/// # Safety
/// `v` live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_coerce_function(v: *const VsAny) -> *const VsClosure {
    // SAFETY: caller contract.
    let v = unsafe { *v };
    match v.tag() {
        Tag::Null | Tag::Undefined => std::ptr::null(),
        Tag::Function => v.as_closure_ptr(),
        _ => conv::type_error(&format!(
            "cannot convert {} to Function",
            conv::any_to_display(v)
        )),
    }
}

// --- P7 natives: props, JSON, System, File, misc -------------------------------

use crate::natives;

fn str_arg16(s: *const VsString) -> Vec<u16> {
    // SAFETY: strings live or null.
    unsafe { string::deref(s) }
        .map(|s| s.units().to_vec())
        .unwrap_or_default()
}

/// Dynamic property read on a boxed receiver: expandos on dynamic
/// objects; Array/Vector `length`; sealed objects → ReferenceError.
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_get_prop(v: *const VsAny, name: *const VsString, out: *mut VsAny) {
    // SAFETY: caller contract.
    let v = unsafe { *v };
    let name16 = str_arg16(name);
    let result = match v.tag() {
        Tag::Object if object::is_dynamic(v.as_object_ptr()) => {
            object::get_prop(v.as_object_ptr(), &name16)
        }
        Tag::Array if name16 == "length".encode_utf16().collect::<Vec<u16>>() => {
            // SAFETY: array live.
            VsAny::uint(unsafe { &*v.as_array_ptr() }.data.borrow().len() as u32)
        }
        Tag::Vector if name16 == "length".encode_utf16().collect::<Vec<u16>>() => {
            // SAFETY: vector live.
            VsAny::uint(unsafe { &*v.as_vector_ptr() }.len)
        }
        Tag::Null | Tag::Undefined => {
            conv::type_error("property access on null");
        }
        _ => exc::throw_error(
            exc::ErrorKind::Reference,
            &format!(
                "property `{}` not found on sealed value (reflective access is Phase 8)",
                String::from_utf16_lossy(&name16)
            ),
        ),
    };
    // SAFETY: caller contract.
    unsafe { *out = result };
}

/// Dynamic property write.
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_set_prop(
    v: *const VsAny,
    name: *const VsString,
    value: *const VsAny,
) {
    // SAFETY: caller contract.
    let v = unsafe { *v };
    let value = unsafe { *value };
    let name16 = str_arg16(name);
    match v.tag() {
        Tag::Object if object::set_prop(v.as_object_ptr(), &name16, value) => {}
        Tag::Null | Tag::Undefined => conv::type_error("property write on null"),
        _ => exc::throw_error(
            exc::ErrorKind::Reference,
            &format!(
                "cannot create property `{}` on a sealed value (SPECS §3.2)",
                String::from_utf16_lossy(&name16)
            ),
        ),
    }
}

/// `key in obj` (§11.8.7 over the dynamic universe).
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_has_prop(key: *const VsAny, obj: *const VsAny) -> u32 {
    // SAFETY: caller contract.
    let (key, obj) = unsafe { (*key, *obj) };
    let name: Vec<u16> = conv::any_to_display(key).encode_utf16().collect();
    let result = match obj.tag() {
        Tag::Object => object::has_prop(obj.as_object_ptr(), &name),
        Tag::Array => {
            let n = conv::any_to_number(key);
            // SAFETY: array live.
            n >= 0.0 && (n as usize) < unsafe { &*obj.as_array_ptr() }.data.borrow().len()
        }
        Tag::Vector => {
            let n = conv::any_to_number(key);
            // SAFETY: vector live.
            n >= 0.0 && (n as usize) < unsafe { &*obj.as_vector_ptr() }.len as usize
        }
        _ => false,
    };
    u32::from(result)
}

/// `delete obj.key` (§11.4.1).
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_delete_prop(obj: *const VsAny, name: *const VsString) -> u32 {
    // SAFETY: caller contract.
    let obj = unsafe { *obj };
    let name16 = str_arg16(name);
    u32::from(match obj.tag() {
        Tag::Object => object::delete_prop(obj.as_object_ptr(), &name16),
        _ => false,
    })
}

/// Index read on a boxed receiver (`o[k]`): Arrays/Vectors by number,
/// dynamic objects by ToString(key).
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_index_get(v: *const VsAny, key: *const VsAny, out: *mut VsAny) {
    // SAFETY: caller contract.
    let (v, key) = unsafe { (*v, *key) };
    let result = match v.tag() {
        Tag::Array => {
            let i = conv::any_to_number(key);
            // SAFETY: array live.
            let data = unsafe { &*v.as_array_ptr() }.data.borrow();
            if i >= 0.0 && (i as usize) < data.len() {
                data[i as usize]
            } else {
                VsAny::UNDEFINED
            }
        }
        Tag::Vector => {
            let i = conv::any_to_number(key);
            // SAFETY: vector live.
            let vec = unsafe { &*v.as_vector_ptr() };
            if i >= 0.0 && (i as usize) < vec.len as usize {
                vec.get_any(i as usize)
            } else {
                exc::throw_error(exc::ErrorKind::Range, "vector index out of range");
            }
        }
        Tag::Object if object::is_dynamic(v.as_object_ptr()) => {
            let name: Vec<u16> = conv::any_to_display(key).encode_utf16().collect();
            object::get_prop(v.as_object_ptr(), &name)
        }
        Tag::Null | Tag::Undefined => conv::type_error("index access on null"),
        _ => VsAny::UNDEFINED,
    };
    // SAFETY: caller contract.
    unsafe { *out = result };
}

/// Index write on a boxed receiver.
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_index_set(v: *const VsAny, key: *const VsAny, value: *const VsAny) {
    // SAFETY: caller contract.
    let (v, key, value) = unsafe { (*v, *key, *value) };
    match v.tag() {
        Tag::Array => {
            let arr = v.as_array_ptr();
            // SAFETY: array live.
            unsafe { vs_arr_set(arr, conv::any_to_number(key), &value) };
        }
        Tag::Vector => {
            let vec = v.as_vector_ptr();
            // SAFETY: vector live.
            unsafe { vs_vec_set(vec, conv::any_to_number(key), &value) };
        }
        Tag::Object => {
            let name: Vec<u16> = conv::any_to_display(key).encode_utf16().collect();
            if !object::set_prop(v.as_object_ptr(), &name, value) {
                exc::throw_error(
                    exc::ErrorKind::Reference,
                    "cannot create property on a sealed value (SPECS §3.2)",
                );
            }
        }
        _ => conv::type_error("index write on this value"),
    }
}

/// Method call on a boxed receiver: reads the property, invokes it as a
/// Function bound to the receiver.
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_call_prop(
    v: *const VsAny,
    name: *const VsString,
    argc: u32,
    args: *const VsAny,
    out: *mut VsAny,
) {
    let mut f = VsAny::UNDEFINED;
    // SAFETY: caller contract.
    unsafe { vs_any_get_prop(v, name, &mut f) };
    if f.tag() != Tag::Function {
        conv::type_error(&format!(
            "value of property `{}` is not a function",
            // SAFETY: strings live or null.
            unsafe { string::deref(name) }
                .map(|s| s.to_rust())
                .unwrap_or_default()
        ));
    }
    // SAFETY: caller contract; receiver object passed as `this` when it is
    // an object.
    let recv = unsafe { *v };
    let this = if recv.tag() == Tag::Object {
        recv.as_object_ptr()
    } else {
        std::ptr::null()
    };
    // SAFETY: function payload live.
    unsafe { closure::invoke(f.as_closure_ptr(), this, argc, args, out) };
}

/// Enumeration over dynamic objects joins the Array/Vector cases.
///
/// # Safety
/// `v` live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_enum_len2(v: *const VsAny) -> i32 {
    // SAFETY: caller contract.
    let v = unsafe { *v };
    match v.tag() {
        Tag::Object => object::prop_count(v.as_object_ptr()) as i32,
        // SAFETY: payloads live.
        _ => unsafe { vs_enum_len(&v) },
    }
}

/// Key at index (objects → property name string).
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_enum_key2(v: *const VsAny, index: i32, out: *mut VsAny) {
    // SAFETY: caller contract.
    let val = unsafe { *v };
    let result = match val.tag() {
        Tag::Object => match object::prop_key_at(val.as_object_ptr(), index as usize) {
            Some(k) => VsAny::string(VsString::alloc(k)),
            None => VsAny::UNDEFINED,
        },
        _ => VsAny::int(index),
    };
    // SAFETY: caller contract.
    unsafe { *out = result };
}

/// Value at index.
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_enum_value2(v: *const VsAny, index: i32, out: *mut VsAny) {
    // SAFETY: caller contract.
    let val = unsafe { *v };
    match val.tag() {
        Tag::Object => {
            let r = object::prop_value_at(val.as_object_ptr(), index as usize);
            // SAFETY: caller contract.
            unsafe { *out = r };
        }
        // SAFETY: caller contract.
        _ => unsafe { vs_enum_value(v, index, out) },
    }
}

/// Math.min/max pair steps (§15.8.2.11/12: NaN propagates).
#[unsafe(no_mangle)]
pub extern "C" fn vs_math_min2(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else {
        a.min(b)
    }
}

/// Pairwise max.
#[unsafe(no_mangle)]
pub extern "C" fn vs_math_max2(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else {
        a.max(b)
    }
}

/// Math.random (§15.8.2.14).
#[unsafe(no_mangle)]
pub extern "C" fn vs_math_random() -> f64 {
    natives::random()
}

/// System.args(): program arguments (excluding the binary name).
#[unsafe(no_mangle)]
pub extern "C" fn vs_system_args() -> *const VsArray {
    let items: Vec<VsAny> = std::env::args()
        .skip(1)
        .map(|a| VsAny::string(VsString::from_rust(&a)))
        .collect();
    seq::new_array(items)
}

/// System.exit(code).
#[unsafe(no_mangle)]
pub extern "C" fn vs_system_exit(code: i32) -> ! {
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    std::process::exit(code)
}

/// System.getenv(name) → value or null.
///
/// # Safety
/// Pointer live or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_system_getenv(name: *const VsString) -> *const VsString {
    // SAFETY: caller contract.
    let name = unsafe { require(name, "getenv") }.to_rust();
    match std::env::var(&name) {
        Ok(v) => VsString::from_rust(&v),
        Err(_) => std::ptr::null(),
    }
}

/// System.time() / Date.now(): milliseconds since the epoch.
#[unsafe(no_mangle)]
pub extern "C" fn vs_system_time() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(f64::NAN)
}

/// File.read(path) → contents or null on any error.
///
/// # Safety
/// Pointer live or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_file_read(path: *const VsString) -> *const VsString {
    // SAFETY: caller contract.
    let path = unsafe { require(path, "File.read") }.to_rust();
    match std::fs::read_to_string(&path) {
        Ok(text) => VsString::from_rust(&text),
        Err(_) => std::ptr::null(),
    }
}

/// File.write(path, text) → success.
///
/// # Safety
/// Pointers live or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_file_write(path: *const VsString, text: *const VsString) -> u32 {
    // SAFETY: caller contract.
    let path = unsafe { require(path, "File.write") }.to_rust();
    let text = unsafe { string::deref(text) }
        .map(|s| s.to_rust())
        .unwrap_or_default();
    u32::from(std::fs::write(&path, text).is_ok())
}

/// File.exists(path).
///
/// # Safety
/// Pointer live or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_file_exists(path: *const VsString) -> u32 {
    // SAFETY: caller contract.
    let path = unsafe { require(path, "File.exists") }.to_rust();
    u32::from(std::path::Path::new(&path).exists())
}

/// JSON.stringify.
///
/// # Safety
/// `v` live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_json_stringify(v: *const VsAny) -> *const VsString {
    // SAFETY: caller contract.
    // Top-level undefined/Function stringifies to "null" (deviation from
    // ES5's undefined result — keeps the return type non-nullable).
    match natives::stringify(unsafe { *v }, 0) {
        Some(s) => VsString::from_rust(&s),
        None => VsString::from_rust("null"),
    }
}

/// JSON.parse; `object_desc`/`object_size` describe the plain Object
/// class so parsed objects are real dynamic instances.
///
/// # Safety
/// Pointers live; descriptor static.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_json_parse(
    text: *const VsString,
    object_desc: *const VsClassDesc,
    object_size: u64,
    out: *mut VsAny,
) {
    // SAFETY: caller contract.
    let text = unsafe { require(text, "JSON.parse") }.to_rust();
    let make = || {
        // SAFETY: descriptor/size from codegen.
        unsafe { vs_alloc_object(object_desc, object_size) as *const u8 }
    };
    let mut parser = natives::JsonParser::new(&text);
    let v = parser.parse(make);
    // SAFETY: caller contract.
    unsafe { *out = v };
}

/// URI/escape functions (§15.1.3, §B.2).
///
/// # Safety
/// Pointer live or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_encode_uri_component(s: *const VsString) -> *const VsString {
    // SAFETY: caller contract.
    VsString::from_rust(&natives::encode_uri_component(
        &unsafe { require(s, "encodeURIComponent") }.to_rust(),
    ))
}

/// decodeURIComponent.
///
/// # Safety
/// Pointer live or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_decode_uri_component(s: *const VsString) -> *const VsString {
    // SAFETY: caller contract.
    VsString::from_rust(&natives::decode_uri_component(
        &unsafe { require(s, "decodeURIComponent") }.to_rust(),
    ))
}

/// escape.
///
/// # Safety
/// Pointer live or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_escape(s: *const VsString) -> *const VsString {
    // SAFETY: caller contract.
    VsString::from_rust(&natives::escape(&unsafe { require(s, "escape") }.to_rust()))
}

/// unescape.
///
/// # Safety
/// Pointer live or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_unescape(s: *const VsString) -> *const VsString {
    // SAFETY: caller contract.
    VsString::from_rust(&natives::unescape(
        &unsafe { require(s, "unescape") }.to_rust(),
    ))
}

/// String#replace with a string pattern (§15.5.4.11: first occurrence).
///
/// # Safety
/// Pointers live or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_str_replace(
    this: *const VsString,
    search: *const VsString,
    repl: *const VsString,
) -> *const VsString {
    // SAFETY: caller contract.
    let s = unsafe { require(this, "replace") };
    let hay = s.units();
    // SAFETY: caller contract.
    let search: &[u16] = unsafe { string::deref(search) }.map_or(&[], VsString::units);
    let repl: &[u16] = unsafe { string::deref(repl) }.map_or(&[], VsString::units);
    // P26: first-match replace on UTF-16 units (§15.5.4.11, string search).
    // An empty search inserts the replacement at the front, matching Rust's
    // `replacen` and ES semantics.
    let out = if search.is_empty() {
        let mut v = Vec::with_capacity(repl.len() + hay.len());
        v.extend_from_slice(repl);
        v.extend_from_slice(hay);
        v
    } else if let Some(pos) = string::find_units(hay, search, 0) {
        let mut v = Vec::with_capacity(hay.len() - search.len() + repl.len());
        v.extend_from_slice(&hay[..pos]);
        v.extend_from_slice(repl);
        v.extend_from_slice(&hay[pos + search.len()..]);
        v
    } else {
        hay.to_vec()
    };
    VsString::alloc(out)
}

/// Array iteration with callbacks: mode 0 forEach, 1 map, 2 filter,
/// 3 some, 4 every (§15.4.4.16-20 shapes; callback(item, index, array)).
///
/// # Safety
/// Pointers live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_arr_iterate(
    a: *const VsArray,
    cb: *const VsClosure,
    mode: u32,
    out: *mut VsAny,
) {
    // SAFETY: caller contract.
    let array = unsafe { arr(a, "iterate") };
    let items = array.data.borrow().clone();
    let self_any = VsAny::array(a);
    // `items`/`mapped`/`filtered` live on the Rust heap across the
    // callbacks: keep GC off.
    let _gc = crate::gc::defer();
    let mut mapped = Vec::new();
    let mut filtered = Vec::new();
    let mut result = VsAny::boolean(mode == 4); // some=false / every=true
    for (i, item) in items.iter().enumerate() {
        let args = [*item, VsAny::number(i as f64), self_any];
        let mut r = VsAny::UNDEFINED;
        // SAFETY: callback live.
        unsafe { closure::invoke(cb, std::ptr::null(), 3, args.as_ptr(), &mut r) };
        match (mode, conv::any_truthy(r)) {
            (1, _) => mapped.push(r),
            (2, true) => filtered.push(*item),
            (3, true) => {
                result = VsAny::boolean(true);
                break;
            }
            (4, false) => {
                result = VsAny::boolean(false);
                break;
            }
            _ => {}
        }
    }
    let final_v = match mode {
        1 => VsAny::array(seq::new_array(mapped)),
        2 => VsAny::array(seq::new_array(filtered)),
        3 | 4 => result,
        _ => VsAny::UNDEFINED,
    };
    // SAFETY: caller contract.
    unsafe { *out = final_v };
}

// --- garbage collection (SPECS §7) -------------------------------------------

/// Safepoint: emitted by the backend at function entries and loop headers.
/// Collects when the allocation debt is due (see the gc module docs).
#[unsafe(no_mangle)]
pub extern "C" fn vs_gc_safepoint() {
    crate::gc::safepoint();
}

/// Registers a static root range (compiled prologue: ref/any statics).
///
/// # Safety
/// `addr..addr + words*8` must be a live static global.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_gc_add_root(addr: *const u8, words: u32) {
    crate::gc::add_root(addr, words as usize);
}

/// System.gc(): forces a full collection.
#[unsafe(no_mangle)]
pub extern "C" fn vs_gc_collect() {
    crate::gc::collect();
}

/// System.gcLiveBytes(): live GC payload bytes (tests/tuning).
#[unsafe(no_mangle)]
pub extern "C" fn vs_gc_live_bytes() -> f64 {
    crate::gc::live_bytes() as f64
}

// --- RegExp (ES3 §15.10; SPECS §6) --------------------------------------------

use crate::regexp::{self, VsRegExp};

/// Live regexp or TypeError (null receiver).
///
/// # Safety
/// `re` null or a live VsRegExp.
unsafe fn re_ref<'x>(re: *const VsRegExp) -> &'x VsRegExp {
    if re.is_null() {
        exc::throw_error(exc::ErrorKind::Type, "null RegExp");
    }
    // SAFETY: caller contract.
    unsafe { &*re }
}

/// Regex literal / constructor from codegen UTF-8 globals.
///
/// # Safety
/// `pat`/`flags` point to `pat_len`/`flags_len` valid UTF-8 bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_regexp_lit(
    pat: *const u8,
    pat_len: u32,
    flags: *const u8,
    flags_len: u32,
) -> *const VsRegExp {
    // SAFETY: caller contract (codegen emits the literal bytes).
    let (pat, flags) = unsafe {
        (
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(pat, pat_len as usize)),
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(flags, flags_len as usize)),
        )
    };
    regexp::new(pat, flags)
}

/// `new RegExp(pattern, flags)` with runtime String operands.
///
/// # Safety
/// Pointers null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_regexp_new(
    pattern: *const VsString,
    flags: *const VsString,
) -> *const VsRegExp {
    // SAFETY: caller contract. Null pattern/flags read as "" (§15.10.4.1
    // undefined-pattern rule).
    let pat = unsafe { string::deref(pattern) }
        .map(|s| s.to_rust())
        .unwrap_or_default();
    let fl = unsafe { string::deref(flags) }
        .map(|s| s.to_rust())
        .unwrap_or_default();
    regexp::new(&pat, &fl)
}

/// `re.test(s)`.
///
/// # Safety
/// Pointers null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_regexp_test(re: *const VsRegExp, s: *const VsString) -> u32 {
    // SAFETY: caller contract.
    let re = unsafe { re_ref(re) };
    match unsafe { string::deref(s) } {
        Some(s) => u32::from(regexp::test(re, s)),
        None => {
            let s = VsString::from_rust("null");
            // SAFETY: freshly allocated live string.
            u32::from(regexp::test(re, unsafe { &*s }))
        }
    }
}

/// `re.exec(s)` → match Array or null.
///
/// # Safety
/// Pointers null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_regexp_exec(
    re: *const VsRegExp,
    s: *const VsString,
) -> *const seq::VsArray {
    // SAFETY: caller contract.
    let re = unsafe { re_ref(re) };
    match unsafe { string::deref(s) } {
        Some(s) => regexp::exec(re, s),
        None => {
            let s = VsString::from_rust("null");
            // SAFETY: freshly allocated live string.
            regexp::exec(re, unsafe { &*s })
        }
    }
}

/// `re.source`.
///
/// # Safety
/// `re` null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_regexp_source(re: *const VsRegExp) -> *const VsString {
    // SAFETY: caller contract.
    unsafe { re_ref(re) }.source
}

/// Flag accessor: returns non-zero when `mask` bits are set.
///
/// # Safety
/// `re` null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_regexp_flag(re: *const VsRegExp, mask: u32) -> u32 {
    // SAFETY: caller contract.
    u32::from(unsafe { re_ref(re) }.flags & mask as u8 != 0)
}

/// `re.lastIndex` (UTF-16 units).
///
/// # Safety
/// `re` null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_regexp_last_index(re: *const VsRegExp) -> i32 {
    // SAFETY: caller contract.
    unsafe { re_ref(re) }.last_index.get() as i32
}

/// `re.toString()` → "/source/flags".
///
/// # Safety
/// `re` null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_regexp_display(re: *const VsRegExp) -> *const VsString {
    if re.is_null() {
        return std::ptr::null();
    }
    // SAFETY: caller contract.
    VsString::from_rust(&regexp::to_display(unsafe { &*re }))
}

/// `s.match(re)` → Array or null.
///
/// # Safety
/// Pointers null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_str_match_re(
    s: *const VsString,
    re: *const VsRegExp,
) -> *const seq::VsArray {
    // SAFETY: caller contract.
    let re = unsafe { re_ref(re) };
    match unsafe { string::deref(s) } {
        Some(s) => regexp::string_match(s, re),
        None => exc::throw_error(exc::ErrorKind::Type, "match() on null String"),
    }
}

/// `s.search(re)` → first match index or -1.
///
/// # Safety
/// Pointers null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_str_search_re(s: *const VsString, re: *const VsRegExp) -> i32 {
    // SAFETY: caller contract.
    let re = unsafe { re_ref(re) };
    match unsafe { string::deref(s) } {
        Some(s) => regexp::string_search(s, re),
        None => exc::throw_error(exc::ErrorKind::Type, "search() on null String"),
    }
}

/// `s.replace(re, repl)`.
///
/// # Safety
/// Pointers null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_str_replace_re(
    s: *const VsString,
    re: *const VsRegExp,
    repl: *const VsString,
) -> *const VsString {
    // SAFETY: caller contract.
    let re = unsafe { re_ref(re) };
    let (Some(s), Some(repl)) = (unsafe { string::deref(s) }, unsafe { string::deref(repl) })
    else {
        exc::throw_error(exc::ErrorKind::Type, "replace() on null String");
    };
    regexp::string_replace(s, re, repl)
}

/// AVM2-style coerce of a boxed value to RegExp (null/undefined pass as
/// null, RegExp passes, anything else TypeError).
///
/// # Safety
/// `v` live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_to_regexp(v: *const VsAny) -> *const VsRegExp {
    // SAFETY: caller contract.
    let v = unsafe { *v };
    match v.tag() {
        Tag::Null | Tag::Undefined => std::ptr::null(),
        Tag::RegExp => v.data as *const VsRegExp,
        _ => exc::throw_error(
            exc::ErrorKind::Type,
            &format!("cannot convert {} to RegExp", conv::any_to_display(v)),
        ),
    }
}

/// `v as RegExp` (null on mismatch, never throws — §AS3 `as`).
///
/// # Safety
/// `v` live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_as_regexp(v: *const VsAny) -> *const VsRegExp {
    // SAFETY: caller contract.
    let v = unsafe { *v };
    if v.tag() == Tag::RegExp {
        v.data as *const VsRegExp
    } else {
        std::ptr::null()
    }
}

// --- Date (ES3 §15.9; SPECS §6) -----------------------------------------------

use crate::date::{self, VsDate};

/// Live date or TypeError.
///
/// # Safety
/// `d` null or a live VsDate.
unsafe fn date_ref<'x>(d: *const VsDate) -> &'x VsDate {
    if d.is_null() {
        exc::throw_error(exc::ErrorKind::Type, "null Date");
    }
    // SAFETY: caller contract.
    unsafe { &*d }
}

/// `new Date(...)`: `argc` numeric components at `parts` (0 = now,
/// 1 = epoch millis, 2..=7 = local civil components; §15.9.3).
///
/// # Safety
/// `parts` points to `argc` f64s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_date_new(argc: u32, parts: *const f64) -> *const VsDate {
    // SAFETY: caller contract.
    let parts = unsafe { std::slice::from_raw_parts(parts, argc as usize) };
    date::alloc(match parts.len() {
        0 => date::now_ms(),
        1 => parts[0],
        _ => date::from_components(parts),
    })
}

/// `Date.UTC(...)` (§15.9.4.3): components in UTC → epoch millis.
///
/// # Safety
/// `parts` points to `argc` f64s (argc >= 2 checked by sema).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_date_utc(argc: u32, parts: *const f64) -> f64 {
    // SAFETY: caller contract.
    let parts = unsafe { std::slice::from_raw_parts(parts, argc as usize) };
    date::utc_from_parts(parts)
}

/// Indexed getter (date.rs `get` docs for the index map).
///
/// # Safety
/// `d` null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_date_get(d: *const VsDate, index: u32) -> f64 {
    // SAFETY: caller contract.
    date::get(unsafe { date_ref(d) }, index)
}

/// `setTime(ms)` → the clipped stored value.
///
/// # Safety
/// `d` null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_date_set_time(d: *const VsDate, value: f64) -> f64 {
    // SAFETY: caller contract.
    date::set_time(unsafe { date_ref(d) }, value)
}

/// Indexed string form (avmplus numbering: 0 toString, 1 toDateString,
/// 2 toTimeString, 6 toUTCString).
///
/// # Safety
/// `d` null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_date_to_string(d: *const VsDate, index: u32) -> *const VsString {
    if d.is_null() {
        return std::ptr::null();
    }
    // SAFETY: caller contract.
    date::to_string(unsafe { &*d }, index)
}

/// `v as Date` (null on mismatch).
///
/// # Safety
/// `v` live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_as_date(v: *const VsAny) -> *const VsDate {
    // SAFETY: caller contract.
    let v = unsafe { *v };
    if v.tag() == Tag::Date {
        v.data as *const VsDate
    } else {
        std::ptr::null()
    }
}

/// Checked coerce of a boxed value to Date (TypeError on mismatch).
///
/// # Safety
/// `v` live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_to_date(v: *const VsAny) -> *const VsDate {
    // SAFETY: caller contract.
    let v = unsafe { *v };
    match v.tag() {
        Tag::Null | Tag::Undefined => std::ptr::null(),
        Tag::Date => v.data as *const VsDate,
        _ => exc::throw_error(
            exc::ErrorKind::Type,
            &format!("cannot convert {} to Date", conv::any_to_display(v)),
        ),
    }
}

// --- sockets (SPECS §6 I/O) ---------------------------------------------------

use crate::socket::{self, VsSocket};

/// Live socket or TypeError.
///
/// # Safety
/// `s` null or a live VsSocket.
unsafe fn sock_ref<'x>(s: *const VsSocket) -> &'x VsSocket {
    if s.is_null() {
        exc::throw_error(exc::ErrorKind::Type, "null Socket");
    }
    // SAFETY: caller contract.
    unsafe { &*s }
}

/// `Socket.connect(host, port)`.
///
/// # Safety
/// `host` null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_socket_connect(host: *const VsString, port: i32) -> *const VsSocket {
    // SAFETY: caller contract.
    let host = match unsafe { string::deref(host) } {
        Some(h) => h.to_rust(),
        None => exc::throw_error(exc::ErrorKind::Type, "Socket.connect: null host"),
    };
    socket::connect(&host, port.clamp(0, 65535) as u16)
}

/// `ServerSocket.bind(port)`.
#[unsafe(no_mangle)]
pub extern "C" fn vs_socket_bind(port: i32) -> *const VsSocket {
    socket::bind(port.clamp(0, 65535) as u16)
}

/// `server.accept()`.
///
/// # Safety
/// `s` null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_socket_accept(s: *const VsSocket) -> *const VsSocket {
    // SAFETY: caller contract.
    socket::accept(unsafe { sock_ref(s) })
}

/// `socket.write(data)`.
///
/// # Safety
/// Pointers null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_socket_write(s: *const VsSocket, data: *const VsString) {
    // SAFETY: caller contract.
    let sock = unsafe { sock_ref(s) };
    match unsafe { string::deref(data) } {
        Some(d) => socket::write(sock, d),
        None => exc::throw_error(exc::ErrorKind::Type, "write: null data"),
    }
}

/// `socket.readLine()` → line or null (EOF).
///
/// # Safety
/// `s` null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_socket_read_line(s: *const VsSocket) -> *const VsString {
    // SAFETY: caller contract.
    socket::read_line(unsafe { sock_ref(s) })
}

/// `socket.read(max)` → chunk or null (EOF).
///
/// # Safety
/// `s` null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_socket_read(s: *const VsSocket, max: i32) -> *const VsString {
    // SAFETY: caller contract.
    socket::read(unsafe { sock_ref(s) }, max.max(1) as usize)
}

/// `close()`.
///
/// # Safety
/// `s` null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_socket_close(s: *const VsSocket) {
    // SAFETY: caller contract.
    socket::close(unsafe { sock_ref(s) });
}

/// `localPort` (listener: bound port; useful with bind(0)).
///
/// # Safety
/// `s` null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_socket_local_port(s: *const VsSocket) -> i32 {
    // SAFETY: caller contract.
    socket::local_port(unsafe { sock_ref(s) })
}

/// Checked coerce of a boxed value to Socket (TypeError on mismatch).
///
/// # Safety
/// `v` live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_to_socket(v: *const VsAny) -> *const VsSocket {
    // SAFETY: caller contract.
    let v = unsafe { *v };
    match v.tag() {
        Tag::Null | Tag::Undefined => std::ptr::null(),
        Tag::Socket => v.data as *const VsSocket,
        _ => exc::throw_error(
            exc::ErrorKind::Type,
            &format!("cannot convert {} to Socket", conv::any_to_display(v)),
        ),
    }
}

/// `v as Socket`-style pointer extraction for any pointer tag: the
/// payload when tags match, else null (never throws).
///
/// # Safety
/// `v` live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_any_as_ptr(v: *const VsAny, tag: u32) -> *const u8 {
    // SAFETY: caller contract.
    let v = unsafe { *v };
    if v.tag() == tag_from(tag) && tag_from(tag) != Tag::Undefined {
        v.data as *const u8
    } else {
        std::ptr::null()
    }
}

// --- namespaces (ES4 first-class, SPECS §5 P16) -------------------------------

use crate::namespace::{self, VsNamespace};

/// Interns a namespace by URI (codegen literal bytes).
///
/// # Safety
/// `uri` points to `len` valid UTF-8 bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_namespace_intern(uri: *const u8, len: u32) -> *const VsNamespace {
    // SAFETY: caller contract.
    let uri =
        unsafe { std::str::from_utf8_unchecked(std::slice::from_raw_parts(uri, len as usize)) };
    namespace::intern(uri)
}

/// `new Namespace(uri)` with a runtime String operand.
///
/// # Safety
/// `uri` null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_namespace_new(uri: *const VsString) -> *const VsNamespace {
    // SAFETY: caller contract.
    let uri = unsafe { string::deref(uri) }
        .map(|s| s.to_rust())
        .unwrap_or_default();
    namespace::intern(&uri)
}

/// `q.uri`.
///
/// # Safety
/// `ns` null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_namespace_uri(ns: *const VsNamespace) -> *const VsString {
    if ns.is_null() {
        exc::throw_error(exc::ErrorKind::Type, "null Namespace");
    }
    // SAFETY: caller contract.
    unsafe { (*ns).uri }
}

/// Runtime-qualified read `obj.q::name` (receiver boxed; ReferenceError
/// when absent).
///
/// # Safety
/// Pointers live; `name` points to `name_len` UTF-8 bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_ns_get(
    recv: *const VsAny,
    ns: *const VsNamespace,
    name: *const u8,
    name_len: u32,
    out: *mut VsAny,
) {
    // SAFETY: caller contract.
    let (recv, name) = unsafe {
        (
            *recv,
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(name, name_len as usize)),
        )
    };
    if ns.is_null() {
        exc::throw_error(exc::ErrorKind::Type, "null Namespace qualifier");
    }
    if recv.tag() != Tag::Object {
        exc::throw_error(
            exc::ErrorKind::Type,
            &format!("`::{name}` needs a class instance receiver"),
        );
    }
    // SAFETY: caller contract.
    unsafe { namespace::get(recv.as_object_ptr(), &*ns, name, out) }
}

/// Runtime-qualified call `obj.q::name(args)`.
///
/// # Safety
/// Pointers live; `args` points to `argc` boxed values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_ns_call(
    recv: *const VsAny,
    ns: *const VsNamespace,
    name: *const u8,
    name_len: u32,
    argc: u32,
    args: *const VsAny,
    out: *mut VsAny,
) {
    // SAFETY: caller contract.
    let (recv, name) = unsafe {
        (
            *recv,
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(name, name_len as usize)),
        )
    };
    if ns.is_null() {
        exc::throw_error(exc::ErrorKind::Type, "null Namespace qualifier");
    }
    if recv.tag() != Tag::Object {
        exc::throw_error(
            exc::ErrorKind::Type,
            &format!("`::{name}()` needs a class instance receiver"),
        );
    }
    // SAFETY: caller contract.
    unsafe { namespace::call(recv.as_object_ptr(), &*ns, name, argc, args, out) }
}

// --- File IO expansion (SPECS §6, P18) ----------------------------------------

/// File.append(path, text) → success (creates the file if absent).
///
/// # Safety
/// Pointers live or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_file_append(path: *const VsString, text: *const VsString) -> u32 {
    use std::io::Write as _;
    // SAFETY: caller contract.
    let path = unsafe { require(path, "File.append") }.to_rust();
    let text = unsafe { string::deref(text) }
        .map(|s| s.to_rust())
        .unwrap_or_default();
    let ok = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .is_ok();
    u32::from(ok)
}

/// File.remove(path) → success (files only; directories use rmdir).
///
/// # Safety
/// Pointer live or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_file_remove(path: *const VsString) -> u32 {
    // SAFETY: caller contract.
    let path = unsafe { require(path, "File.remove") }.to_rust();
    u32::from(std::fs::remove_file(&path).is_ok())
}

/// File.copy(from, to) → success.
///
/// # Safety
/// Pointers live or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_file_copy(from: *const VsString, to: *const VsString) -> u32 {
    // SAFETY: caller contract.
    let from = unsafe { require(from, "File.copy") }.to_rust();
    let to = unsafe { require(to, "File.copy") }.to_rust();
    u32::from(std::fs::copy(&from, &to).is_ok())
}

/// File.rename(from, to) → success (also moves).
///
/// # Safety
/// Pointers live or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_file_rename(from: *const VsString, to: *const VsString) -> u32 {
    // SAFETY: caller contract.
    let from = unsafe { require(from, "File.rename") }.to_rust();
    let to = unsafe { require(to, "File.rename") }.to_rust();
    u32::from(std::fs::rename(&from, &to).is_ok())
}

/// File.mkdir(path) → success (recursive, like `mkdir -p`).
///
/// # Safety
/// Pointer live or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_file_mkdir(path: *const VsString) -> u32 {
    // SAFETY: caller contract.
    let path = unsafe { require(path, "File.mkdir") }.to_rust();
    u32::from(std::fs::create_dir_all(&path).is_ok())
}

/// File.rmdir(path) → success (empty directories only — no recursive
/// delete footgun; remove files first).
///
/// # Safety
/// Pointer live or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_file_rmdir(path: *const VsString) -> u32 {
    // SAFETY: caller contract.
    let path = unsafe { require(path, "File.rmdir") }.to_rust();
    u32::from(std::fs::remove_dir(&path).is_ok())
}

/// File.list(path) → sorted Array of entry names, or null on error.
///
/// # Safety
/// Pointer live or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_file_list(path: *const VsString) -> *const seq::VsArray {
    // SAFETY: caller contract.
    let path = unsafe { require(path, "File.list") }.to_rust();
    let Ok(entries) = std::fs::read_dir(&path) else {
        return std::ptr::null();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect();
    names.sort();
    let items: Vec<VsAny> = names
        .iter()
        .map(|n| VsAny::string(VsString::from_rust(n)))
        .collect();
    seq::new_array(items)
}

/// File.isDirectory(path).
///
/// # Safety
/// Pointer live or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_file_is_directory(path: *const VsString) -> u32 {
    // SAFETY: caller contract.
    let path = unsafe { require(path, "File.isDirectory") }.to_rust();
    u32::from(std::path::Path::new(&path).is_dir())
}

/// File.size(path) → bytes, or -1 on error.
///
/// # Safety
/// Pointer live or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_file_size(path: *const VsString) -> f64 {
    // SAFETY: caller contract.
    let path = unsafe { require(path, "File.size") }.to_rust();
    std::fs::metadata(&path)
        .map(|m| m.len() as f64)
        .unwrap_or(-1.0)
}

/// File.mtime(path) → epoch milliseconds, or -1 on error.
///
/// # Safety
/// Pointer live or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn vs_file_mtime(path: *const VsString) -> f64 {
    // SAFETY: caller contract.
    let path = unsafe { require(path, "File.mtime") }.to_rust();
    std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as f64)
        .unwrap_or(-1.0)
}

/// System.readLine() → next stdin line without its terminator, or null
/// at EOF.
#[unsafe(no_mangle)]
pub extern "C" fn vs_system_read_line() -> *const VsString {
    use std::io::BufRead as _;
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => std::ptr::null(),
        Ok(_) => {
            while line.ends_with('\n') || line.ends_with('\r') {
                line.pop();
            }
            VsString::from_rust(&line)
        }
    }
}
