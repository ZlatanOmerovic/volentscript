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
pub unsafe extern "C" fn main(_argc: i32, _argv: *const *const u8) -> i32 {
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
    /// `toLowerCase` (§15.5.4.16).
    vs_str_to_lower, |s| VsString::from_rust(&s.to_rust().to_lowercase())
);

str_method!(
    /// `toUpperCase` (§15.5.4.18).
    vs_str_to_upper, |s| VsString::from_rust(&s.to_rust().to_uppercase())
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
    let result = if undef_or_null(a.tag()) || undef_or_null(b.tag()) {
        undef_or_null(a.tag()) && undef_or_null(b.tag())
    } else if a.tag() == Tag::String && b.tag() == Tag::String {
        conv::any_strict_equals(a, b)
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
    let layout = std::alloc::Layout::from_size_align(size as usize, 8).expect("layout");
    // SAFETY: layout is non-zero (header word always present); allocation
    // is intentionally leaked (P3+ memory model, see crate docs).
    let p = unsafe { std::alloc::alloc_zeroed(layout) };
    assert!(!p.is_null(), "out of memory");
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
