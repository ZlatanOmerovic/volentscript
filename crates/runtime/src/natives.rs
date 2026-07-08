//! P7 stdlib natives: JSON, URI encoding, System/File helpers
//! (SPECS §6 — the Redtamarin-shaped CLI surface, not `flash.*`).

use crate::any::{Tag, VsAny};
use crate::conv;
use crate::object;
use crate::seq;
use crate::string::VsString;

/// JSON.stringify over the dynamic universe (ES5-shaped §15.12.3 subset):
/// numbers/strings/booleans/null, Arrays, Vectors, dynamic-object
/// expandos. Sealed instances stringify as "{}" (reflection is P8);
/// undefined and Functions become null inside arrays, and are skipped as
/// object members.
pub fn stringify(v: VsAny, depth: usize) -> Option<String> {
    if depth > 128 {
        conv::type_error("JSON.stringify: structure too deep (cycle?)");
    }
    Some(match v.tag() {
        Tag::Undefined | Tag::Function | Tag::RegExp | Tag::Date => return None,
        Tag::Null => "null".to_string(),
        Tag::Boolean => if v.data != 0 { "true" } else { "false" }.to_string(),
        Tag::Int | Tag::UInt => conv::any_to_display(v),
        Tag::Number => {
            let f = f64::from_bits(v.data);
            if f.is_finite() {
                conv::number_to_string(f)
            } else {
                "null".to_string()
            }
        }
        Tag::String => {
            // SAFETY: string payloads live.
            let s = unsafe { crate::string::deref(v.as_string_ptr()) }
                .map(|s| s.to_rust())
                .unwrap_or_default();
            quote(&s)
        }
        Tag::Array => {
            // SAFETY: array payloads live.
            let items = unsafe { &*v.as_array_ptr() }.data.borrow().clone();
            json_array(&items, depth)
        }
        Tag::Vector => {
            // SAFETY: vector payloads live.
            let items = unsafe { &*v.as_vector_ptr() }.data.borrow().clone();
            json_array(&items, depth)
        }
        Tag::Object => {
            let obj = v.as_object_ptr();
            let mut parts = Vec::new();
            for i in 0..object::prop_count(obj) {
                let Some(key) = object::prop_key_at(obj, i) else {
                    continue;
                };
                let value = object::prop_value_at(obj, i);
                if let Some(vs) = stringify(value, depth + 1) {
                    parts.push(format!("{}:{vs}", quote(&String::from_utf16_lossy(&key))));
                }
            }
            format!("{{{}}}", parts.join(","))
        }
    })
}

fn json_array(items: &[VsAny], depth: usize) -> String {
    let parts: Vec<String> = items
        .iter()
        .map(|it| stringify(*it, depth + 1).unwrap_or_else(|| "null".to_string()))
        .collect();
    format!("[{}]", parts.join(","))
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// JSON.parse producing dynamic Objects / Arrays / primitives. Objects are
/// instances of the prelude-registered Error[0]'s ancestor... no: plain
/// `Object` — constructed through the registered Object descriptor is not
/// available here, so parse builds "bare" dynamic objects via the same
/// allocation path codegen uses (`vs_json_new_object` callback set at
/// startup).
pub struct JsonParser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> JsonParser<'a> {
    /// New parser over UTF-8 text.
    pub fn new(text: &'a str) -> Self {
        JsonParser {
            bytes: text.as_bytes(),
            pos: 0,
        }
    }

    fn ws(&mut self) {
        while matches!(self.bytes.get(self.pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn fail(&self) -> ! {
        crate::exc::throw_error(
            crate::exc::ErrorKind::Type,
            &format!("JSON.parse: unexpected input at offset {}", self.pos),
        )
    }

    /// Parses one value (entry point checks trailing garbage).
    pub fn parse(&mut self, make_object: impl Fn() -> *const u8 + Copy) -> VsAny {
        self.ws();
        let v = self.value(make_object);
        self.ws();
        if self.pos != self.bytes.len() {
            self.fail();
        }
        v
    }

    fn value(&mut self, make_object: impl Fn() -> *const u8 + Copy) -> VsAny {
        self.ws();
        match self.bytes.get(self.pos) {
            Some(b'n') => {
                self.lit("null");
                VsAny::NULL
            }
            Some(b't') => {
                self.lit("true");
                VsAny::boolean(true)
            }
            Some(b'f') => {
                self.lit("false");
                VsAny::boolean(false)
            }
            Some(b'"') => {
                let s = self.string();
                VsAny::string(VsString::from_rust(&s))
            }
            Some(b'[') => {
                self.pos += 1;
                let mut items = Vec::new();
                self.ws();
                if self.bytes.get(self.pos) == Some(&b']') {
                    self.pos += 1;
                } else {
                    loop {
                        items.push(self.value(make_object));
                        self.ws();
                        match self.bytes.get(self.pos) {
                            Some(b',') => self.pos += 1,
                            Some(b']') => {
                                self.pos += 1;
                                break;
                            }
                            _ => self.fail(),
                        }
                    }
                }
                VsAny::array(seq::new_array(items))
            }
            Some(b'{') => {
                self.pos += 1;
                let obj = make_object();
                self.ws();
                if self.bytes.get(self.pos) == Some(&b'}') {
                    self.pos += 1;
                } else {
                    loop {
                        self.ws();
                        if self.bytes.get(self.pos) != Some(&b'"') {
                            self.fail();
                        }
                        let key = self.string();
                        self.ws();
                        if self.bytes.get(self.pos) != Some(&b':') {
                            self.fail();
                        }
                        self.pos += 1;
                        let value = self.value(make_object);
                        let key16: Vec<u16> = key.encode_utf16().collect();
                        object::set_prop(obj, &key16, value);
                        self.ws();
                        match self.bytes.get(self.pos) {
                            Some(b',') => self.pos += 1,
                            Some(b'}') => {
                                self.pos += 1;
                                break;
                            }
                            _ => self.fail(),
                        }
                    }
                }
                VsAny::object(obj)
            }
            Some(c) if c.is_ascii_digit() || *c == b'-' => {
                let start = self.pos;
                if self.bytes.get(self.pos) == Some(&b'-') {
                    self.pos += 1;
                }
                while self.bytes.get(self.pos).is_some_and(|b| {
                    b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-')
                }) {
                    self.pos += 1;
                }
                let text = std::str::from_utf8(&self.bytes[start..self.pos]).unwrap_or("");
                match text.parse::<f64>() {
                    Ok(n) => VsAny::number(n),
                    Err(_) => self.fail(),
                }
            }
            _ => self.fail(),
        }
    }

    fn lit(&mut self, word: &str) {
        if self.bytes[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
        } else {
            self.fail();
        }
    }

    fn string(&mut self) -> String {
        self.pos += 1; // opening quote
        let mut out = String::new();
        loop {
            match self.bytes.get(self.pos) {
                None => self.fail(),
                Some(b'"') => {
                    self.pos += 1;
                    return out;
                }
                Some(b'\\') => {
                    self.pos += 1;
                    match self.bytes.get(self.pos) {
                        Some(b'"') => out.push('"'),
                        Some(b'\\') => out.push('\\'),
                        Some(b'/') => out.push('/'),
                        Some(b'n') => out.push('\n'),
                        Some(b't') => out.push('\t'),
                        Some(b'r') => out.push('\r'),
                        Some(b'b') => out.push('\u{0008}'),
                        Some(b'f') => out.push('\u{000C}'),
                        Some(b'u') => {
                            let hex = self
                                .bytes
                                .get(self.pos + 1..self.pos + 5)
                                .and_then(|h| std::str::from_utf8(h).ok())
                                .and_then(|h| u32::from_str_radix(h, 16).ok());
                            match hex {
                                Some(code) => {
                                    out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                                    self.pos += 4;
                                }
                                None => self.fail(),
                            }
                        }
                        _ => self.fail(),
                    }
                    self.pos += 1;
                }
                Some(&b) if b < 0x80 => {
                    out.push(b as char);
                    self.pos += 1;
                }
                Some(_) => {
                    // Multi-byte UTF-8: copy the full scalar.
                    let rest = std::str::from_utf8(&self.bytes[self.pos..]).unwrap_or("\u{FFFD}");
                    let c = rest.chars().next().unwrap_or('\u{FFFD}');
                    out.push(c);
                    self.pos += c.len_utf8();
                }
            }
        }
    }
}

/// encodeURIComponent (§15.1.3.4 unreserved set).
pub fn encode_uri_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// decodeURIComponent (§15.1.3.2 subset: %XX byte sequences).
pub fn decode_uri_component(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() + 1 {
            if let Some(h) = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(h);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// escape (ES3 §B.2.1).
pub fn escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        let u = c as u32;
        if c.is_ascii_alphanumeric() || matches!(c, '@' | '*' | '_' | '+' | '-' | '.' | '/') {
            out.push(c);
        } else if u < 256 {
            out.push_str(&format!("%{u:02X}"));
        } else {
            out.push_str(&format!("%u{u:04X}"));
        }
    }
    out
}

/// unescape (ES3 §B.2.2).
pub fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if b.get(i + 1) == Some(&b'u') {
                if let Some(code) = b
                    .get(i + 2..i + 6)
                    .and_then(|h| std::str::from_utf8(h).ok())
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                {
                    out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                    i += 6;
                    continue;
                }
            } else if let Some(code) = b
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u32::from_str_radix(h, 16).ok())
            {
                out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                i += 3;
                continue;
            }
        }
        // Copy one scalar.
        let rest = std::str::from_utf8(&b[i..]).unwrap_or("\u{FFFD}");
        let c = rest.chars().next().unwrap_or('\u{FFFD}');
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// Math.random (§15.8.2.14): xorshift64* seeded from the clock — no
/// crypto claims, documented.
pub fn random() -> f64 {
    use std::cell::Cell;
    thread_local! {
        static STATE: Cell<u64> = const { Cell::new(0) };
    }
    STATE.with(|s| {
        let mut x = s.get();
        if x == 0 {
            x = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x9E3779B97F4A7C15)
                | 1;
        }
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        s.set(x);
        let bits = x.wrapping_mul(0x2545F4914F6CDD1D);
        (bits >> 11) as f64 / (1u64 << 53) as f64
    })
}
