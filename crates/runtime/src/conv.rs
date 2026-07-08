//! Conversions per ECMA-262 3rd ed. §9 (the AS3 rules, SPECS §3.3).
//! Section numbers cited per function.

use crate::any::{Tag, VsAny};

/// ToInt32 (§9.5): modulo-2^32 with sign.
pub fn to_int32(v: f64) -> i32 {
    if !v.is_finite() || v == 0.0 {
        return 0;
    }
    let m = v.trunc() % 4_294_967_296.0; // 2^32
    let m = if m < 0.0 { m + 4_294_967_296.0 } else { m };
    if m >= 2_147_483_648.0 {
        (m - 4_294_967_296.0) as i32
    } else {
        m as i32
    }
}

/// ToUint32 (§9.6).
pub fn to_uint32(v: f64) -> u32 {
    to_int32(v) as u32
}

/// ToString(Number) (§9.8.1). Deviation, revisit P7: no exponential
/// notation for |x| ≥ 1e21 (prints expanded digits instead).
pub fn number_to_string(v: f64) -> String {
    if v.is_nan() {
        return "NaN".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if v == 0.0 {
        return "0".to_string();
    }
    // Rust's Display prints the shortest round-trip form and omits ".0",
    // matching §9.8.1 for the common range.
    format!("{v}")
}

/// ToNumber(String) (§9.3.1): whitespace-trimmed; "" → 0; hex literals;
/// otherwise StrDecimalLiteral or NaN.
pub fn string_to_number(s: &str) -> f64 {
    let t = s.trim_matches(|c: char| c.is_whitespace());
    if t.is_empty() {
        return 0.0;
    }
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return match u64::from_str_radix(hex, 16) {
            Ok(v) => v as f64,
            Err(_) => f64::NAN,
        };
    }
    if t == "Infinity" || t == "+Infinity" {
        return f64::INFINITY;
    }
    if t == "-Infinity" {
        return f64::NEG_INFINITY;
    }
    t.parse::<f64>().unwrap_or(f64::NAN)
}

/// ToBoolean (§9.2): undefined/null → false; ±0/NaN → false; "" → false.
pub fn any_truthy(v: VsAny) -> bool {
    match v.tag() {
        Tag::Undefined | Tag::Null => false,
        Tag::Boolean => v.data != 0,
        Tag::Int => v.data as i64 as i32 != 0,
        Tag::UInt => v.data as u32 != 0,
        Tag::Number => {
            let f = f64::from_bits(v.data);
            f != 0.0 && !f.is_nan()
        }
        Tag::RegExp | Tag::Date | Tag::Socket => v.data != 0,
        Tag::String => {
            // SAFETY: String-tagged payloads always hold a live VsString.
            unsafe { crate::string::deref(v.as_string_ptr()) }.is_some_and(|s| s.len > 0)
        }
        // Every object is truthy (§9.2).
        Tag::Object | Tag::Array | Tag::Vector | Tag::Function => true,
    }
}

/// ToNumber(Any) (§9.3): undefined → NaN, null → 0, boolean → 0/1.
pub fn any_to_number(v: VsAny) -> f64 {
    match v.tag() {
        Tag::Undefined => f64::NAN,
        Tag::Null => 0.0,
        Tag::Boolean => {
            if v.data != 0 {
                1.0
            } else {
                0.0
            }
        }
        Tag::String => {
            // SAFETY: String-tagged payloads always hold a live VsString.
            match unsafe { crate::string::deref(v.as_string_ptr()) } {
                Some(s) => string_to_number(&s.to_rust()),
                None => 0.0,
            }
        }
        // Objects: ToPrimitive would call valueOf; the P4 object model has
        // none, so NaN (documented deviation until valueOf lands).
        Tag::Object | Tag::Array | Tag::Vector | Tag::Function => f64::NAN,
        _ => v.numeric().unwrap_or(f64::NAN),
    }
}

/// ToString(Any) (§9.8): undefined → "undefined", null → "null".
pub fn any_to_display(v: VsAny) -> String {
    match v.tag() {
        Tag::Undefined => "undefined".to_string(),
        Tag::Null => "null".to_string(),
        Tag::Boolean => if v.data != 0 { "true" } else { "false" }.to_string(),
        Tag::Int => (v.data as i64 as i32).to_string(),
        Tag::UInt => (v.data as u32).to_string(),
        Tag::Number => number_to_string(f64::from_bits(v.data)),
        Tag::String => {
            // SAFETY: String-tagged payloads always hold a live VsString.
            match unsafe { crate::string::deref(v.as_string_ptr()) } {
                Some(s) => s.to_rust(),
                None => "null".to_string(),
            }
        }
        Tag::Object => crate::object::object_to_display(v.as_object_ptr()),
        // §15.4.4.2: elements joined with ",".
        Tag::Array => {
            // SAFETY: Array-tagged payloads hold live VsArrays.
            let arr = unsafe { &*v.as_array_ptr() };
            crate::seq::join(&arr.data.borrow(), ",")
        }
        Tag::Vector => {
            // SAFETY: Vector-tagged payloads hold live VsVectors.
            let vec = unsafe { &*v.as_vector_ptr() };
            crate::seq::join(&vec.data.borrow(), ",")
        }
        Tag::Function => "function Function() {}".to_string(),
        Tag::RegExp => {
            // SAFETY: RegExp-tagged payloads hold live VsRegExps.
            crate::regexp::to_display(unsafe { &*(v.data as *const crate::regexp::VsRegExp) })
        }
        Tag::Date => {
            // SAFETY: Date-tagged payloads hold live VsDates; to_string
            // returns a live runtime string.
            unsafe {
                let s = crate::date::to_string(&*(v.data as *const crate::date::VsDate), 0);
                crate::string::deref(s)
                    .map(|s| s.to_rust())
                    .unwrap_or_default()
            }
        }
        Tag::Socket => "[object Socket]".to_string(),
    }
}

/// `typeof` result per ES3 §11.4.3 (AS3 additionally reports "number" for
/// int/uint — they are Number at the language level).
pub fn any_typeof(v: VsAny) -> &'static str {
    match v.tag() {
        Tag::Undefined => "undefined",
        Tag::Null => "object",
        Tag::Boolean => "boolean",
        Tag::Int | Tag::UInt | Tag::Number => "number",
        Tag::String => "string",
        Tag::Object | Tag::Array | Tag::Vector | Tag::RegExp | Tag::Date | Tag::Socket => "object",
        Tag::Function => "function",
    }
}

/// `is` for core types (AS3 semantics: `5.0 is int` is true when the value
/// is an integral Number in int range — numeric `is` tests the value, not
/// the storage tag; per avmplus AvmCore::istype for builtin numerics).
pub fn any_is(v: VsAny, target: Tag) -> bool {
    match target {
        Tag::Int => v
            .numeric()
            .is_some_and(|f| f.trunc() == f && f >= i32::MIN as f64 && f <= i32::MAX as f64),
        Tag::UInt => v
            .numeric()
            .is_some_and(|f| f.trunc() == f && f >= 0.0 && f <= u32::MAX as f64),
        Tag::Number => v.numeric().is_some(),
        Tag::Boolean => v.tag() == Tag::Boolean,
        Tag::String => v.tag() == Tag::String,
        Tag::Function => v.tag() == Tag::Function,
        Tag::RegExp => v.tag() == Tag::RegExp,
        Tag::Date => v.tag() == Tag::Date,
        Tag::Socket => v.tag() == Tag::Socket,
        Tag::Null | Tag::Undefined | Tag::Object | Tag::Array | Tag::Vector => false,
    }
}

/// Strict equality (§11.9.6) over boxed values; int/uint/Number compare
/// numerically (same language-level type).
pub fn any_strict_equals(a: VsAny, b: VsAny) -> bool {
    match (a.numeric(), b.numeric()) {
        (Some(x), Some(y)) => return x == y,
        (None, None) => {}
        _ => return false,
    }
    match (a.tag(), b.tag()) {
        (Tag::Undefined, Tag::Undefined) | (Tag::Null, Tag::Null) => true,
        (Tag::Boolean, Tag::Boolean) => a.data == b.data,
        // Object identity (§11.9.6 step 13).
        (Tag::Object, Tag::Object) | (Tag::Array, Tag::Array) | (Tag::Vector, Tag::Vector) => {
            a.data == b.data
        }
        (Tag::String, Tag::String) => {
            // SAFETY: String-tagged payloads always hold live VsStrings.
            let (sa, sb) = unsafe {
                (
                    crate::string::deref(a.as_string_ptr()),
                    crate::string::deref(b.as_string_ptr()),
                )
            };
            match (sa, sb) {
                (Some(x), Some(y)) => x.units() == y.units(),
                (None, None) => true,
                _ => false,
            }
        }
        _ => false,
    }
}

/// Throws a catchable TypeError (constructed from the program's Error
/// descriptors when registered; aborts otherwise).
pub fn type_error(msg: &str) -> ! {
    crate::exc::throw_error(crate::exc::ErrorKind::Type, msg)
}

/// parseInt (§15.1.2.2).
pub fn parse_int(s: &str, radix: i32) -> f64 {
    let mut t = s.trim_matches(|c: char| c.is_whitespace());
    let mut sign = 1.0;
    if let Some(rest) = t.strip_prefix('-') {
        sign = -1.0;
        t = rest;
    } else if let Some(rest) = t.strip_prefix('+') {
        t = rest;
    }
    let mut radix = radix;
    if radix == 16 || radix == 0 {
        if let Some(rest) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
            t = rest;
            radix = 16;
        } else if radix == 0 {
            radix = 10;
        }
    }
    if !(2..=36).contains(&radix) {
        return f64::NAN;
    }
    let digits: Vec<u32> = t.chars().map_while(|c| c.to_digit(radix as u32)).collect();
    if digits.is_empty() {
        return f64::NAN;
    }
    let mut acc = 0.0f64;
    for d in digits {
        acc = acc * f64::from(radix) + f64::from(d);
    }
    sign * acc
}

/// parseFloat (§15.1.2.3): longest valid StrDecimalLiteral prefix.
pub fn parse_float(s: &str) -> f64 {
    let t = s.trim_start_matches(|c: char| c.is_whitespace());
    if t.starts_with("Infinity") || t.starts_with("+Infinity") {
        return f64::INFINITY;
    }
    if t.starts_with("-Infinity") {
        return f64::NEG_INFINITY;
    }
    // Longest parsable prefix.
    let bytes = t.as_bytes();
    let mut end = 0;
    let mut seen_dot = false;
    let mut seen_e = false;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'0'..=b'9' => end = i + 1,
            b'+' | b'-' if i == 0 => {}
            b'+' | b'-' if i > 0 && (bytes[i - 1] == b'e' || bytes[i - 1] == b'E') => {}
            b'.' if !seen_dot && !seen_e => seen_dot = true,
            b'e' | b'E' if !seen_e && end > 0 => seen_e = true,
            _ => break,
        }
    }
    if end == 0 {
        return f64::NAN;
    }
    t[..end].parse::<f64>().unwrap_or(f64::NAN)
}

/// Number#toString(radix) (§15.7.4.2; radix 10 → §9.8.1).
pub fn number_to_string_radix(v: f64, radix: i32) -> String {
    if radix == 10 {
        return number_to_string(v);
    }
    if !(2..=36).contains(&radix) {
        // ASC range-checks at runtime; exceptions are P6.
        type_error("toString radix must be between 2 and 36");
    }
    if v.is_nan() {
        return "NaN".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    // Integral part only for non-base-10 (fractional digits in arbitrary
    // bases follow avmplus MathUtils::convertDoubleToString — P7 fidelity).
    let neg = v < 0.0;
    let mut i = v.abs().trunc();
    let digits = "0123456789abcdefghijklmnopqrstuvwxyz".as_bytes();
    let mut out = Vec::new();
    if i == 0.0 {
        out.push(b'0');
    }
    let base = f64::from(radix);
    while i >= 1.0 {
        let d = (i % base) as usize;
        out.push(digits[d]);
        i = (i / base).trunc();
    }
    if neg {
        out.push(b'-');
    }
    out.reverse();
    String::from_utf8(out).expect("ascii")
}

/// Number#toFixed (§15.7.4.5).
pub fn number_to_fixed(v: f64, fraction_digits: i32) -> String {
    let p = fraction_digits.clamp(0, 20) as usize;
    format!("{v:.p$}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_int32_wraps() {
        // §9.5 examples
        assert_eq!(to_int32(4_294_967_296.0), 0);
        assert_eq!(to_int32(2_147_483_648.0), -2_147_483_648);
        assert_eq!(to_int32(-1.5), -1);
        assert_eq!(to_int32(f64::NAN), 0);
        assert_eq!(to_int32(f64::INFINITY), 0);
    }

    #[test]
    fn number_strings() {
        assert_eq!(number_to_string(5.0), "5");
        assert_eq!(number_to_string(0.5), "0.5");
        assert_eq!(number_to_string(-0.0), "0");
        assert_eq!(number_to_string(f64::NAN), "NaN");
        assert_eq!(number_to_string(f64::INFINITY), "Infinity");
        assert_eq!(number_to_string_radix(255.0, 16), "ff");
        assert_eq!(number_to_fixed(1.005, 2), "1.00"); // binary rounding, same as avmplus
    }

    #[test]
    fn parse_functions() {
        assert_eq!(parse_int("  42abc", 10), 42.0);
        assert_eq!(parse_int("0xFF", 0), 255.0);
        assert_eq!(parse_int("-10", 2), -2.0);
        assert!(parse_int("zz", 10).is_nan());
        assert_eq!(parse_float("3.5tail"), 3.5);
        assert_eq!(parse_float("1e2"), 100.0);
        assert!(parse_float("x").is_nan());
    }

    #[test]
    fn string_to_number_rules() {
        assert_eq!(string_to_number(""), 0.0);
        assert_eq!(string_to_number("  12 "), 12.0);
        assert_eq!(string_to_number("0x10"), 16.0);
        assert!(string_to_number("12x").is_nan());
    }
}
