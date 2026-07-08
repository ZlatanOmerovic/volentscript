//! RegExp (ES3 §15.10, AS3 RegExp class), backed by `fancy-regex` — a
//! backtracking engine, required because ES3 patterns include
//! backreferences and lazy quantifiers that DFA engines reject.
//!
//! Strings are UTF-16 at the language level; the engine works on UTF-8.
//! Each operation converts the subject once and maps indices back to
//! UTF-16 units (`lastIndex`, `search`, match positions are all
//! UTF-16-unit values per the spec).

use std::cell::Cell;

use crate::any::VsAny;
use crate::exc;
use crate::gc;
use crate::seq;
use crate::string::VsString;

/// Flag bits (source order: g i m s x).
pub mod flag {
    #![allow(missing_docs)]
    pub const GLOBAL: u8 = 1;
    pub const IGNORE_CASE: u8 = 2;
    pub const MULTILINE: u8 = 4;
    pub const DOTALL: u8 = 8;
    pub const EXTENDED: u8 = 16;
}

/// The RegExp runtime object (GC kind RegExp: `source` is traced, the
/// compiled program is dropped on sweep).
pub struct VsRegExp {
    /// Pattern source text (GC string).
    pub source: *const VsString,
    /// Flag bits (see [`flag`]).
    pub flags: u8,
    /// `lastIndex` in UTF-16 units (§15.10.6.2 global-exec state).
    pub last_index: Cell<usize>,
    /// Compiled program.
    pub compiled: fancy_regex::Regex,
}

/// Compiles and allocates a RegExp. Bad patterns/flags throw a catchable
/// SyntaxError (§15.10.4.1; thrown at construction — literals construct
/// per evaluation, so a bad literal pattern throws when first reached).
pub fn new(pattern: &str, flags_text: &str) -> *const VsRegExp {
    let mut flags = 0u8;
    for c in flags_text.chars() {
        flags |= match c {
            'g' => flag::GLOBAL,
            'i' => flag::IGNORE_CASE,
            'm' => flag::MULTILINE,
            's' => flag::DOTALL,
            'x' => flag::EXTENDED,
            other => exc::throw_error(
                exc::ErrorKind::Syntax,
                &format!("invalid regular expression flag `{other}`"),
            ),
        };
    }
    // Inline-flag prefix: fancy-regex spellings of i/m/s/x (§15.10.2
    // semantics; `g` is not a pattern flag — it drives exec/match/replace).
    let mut prefix = String::new();
    for (bit, ch) in [
        (flag::IGNORE_CASE, 'i'),
        (flag::MULTILINE, 'm'),
        (flag::DOTALL, 's'),
        (flag::EXTENDED, 'x'),
    ] {
        if flags & bit != 0 {
            prefix.push(ch);
        }
    }
    let translated = if prefix.is_empty() {
        pattern.to_string()
    } else {
        format!("(?{prefix}){pattern}")
    };
    let compiled = match fancy_regex::Regex::new(&translated) {
        Ok(r) => r,
        Err(e) => exc::throw_error(
            exc::ErrorKind::Syntax,
            &format!("invalid regular expression /{pattern}/: {e}"),
        ),
    };
    let p = gc::alloc(std::mem::size_of::<VsRegExp>(), gc::Kind::RegExp) as *mut VsRegExp;
    // SAFETY: fresh block of exactly VsRegExp size.
    unsafe {
        p.write(VsRegExp {
            source: VsString::from_rust(pattern),
            flags,
            last_index: Cell::new(0),
            compiled,
        })
    };
    p
}

/// UTF-16 unit count of the UTF-8 prefix `&s[..byte]`.
fn utf16_of_byte(s: &str, byte: usize) -> usize {
    s[..byte].chars().map(char::len_utf16).sum()
}

/// Byte offset of UTF-16 unit index `target` (None if past the end).
fn byte_of_utf16(s: &str, target: usize) -> Option<usize> {
    if target == 0 {
        return Some(0);
    }
    let mut units = 0;
    for (byte, c) in s.char_indices() {
        if units == target {
            return Some(byte);
        }
        units += c.len_utf16();
    }
    (units >= target).then_some(s.len())
}

/// One engine match against `text` starting at UTF-16 unit `start16`.
/// Returns the captures (group 0 always present) or None.
fn find_at<'t>(re: &VsRegExp, text: &'t str, start16: usize) -> Option<fancy_regex::Captures<'t>> {
    let byte = byte_of_utf16(text, start16)?;
    match re.compiled.captures_from_pos(text, byte) {
        Ok(c) => c,
        // Backtracking limits and similar runtime faults are errors.
        Err(e) => exc::throw_error(exc::ErrorKind::Error, &format!("regular expression: {e}")),
    }
}

/// §15.10.6.2 exec: match Array (group 0 + groups, unmatched = undefined)
/// or null; advances `lastIndex` when global.
pub fn exec(re: &VsRegExp, subject: &VsString) -> *const seq::VsArray {
    let text = subject.to_rust();
    let global = re.flags & flag::GLOBAL != 0;
    let start = if global { re.last_index.get() } else { 0 };
    let caps = match find_at(re, &text, start) {
        Some(c) => c,
        None => {
            if global {
                re.last_index.set(0);
            }
            return std::ptr::null();
        }
    };
    let m0 = caps.get(0).expect("group 0");
    if global {
        re.last_index.set(utf16_of_byte(&text, m0.end()));
    }
    let items: Vec<VsAny> = caps
        .iter()
        .map(|g| match g {
            Some(m) => VsAny::string(VsString::from_rust(m.as_str())),
            None => VsAny::UNDEFINED,
        })
        .collect();
    seq::new_array(items)
}

/// §15.10.6.3 test (same lastIndex behavior as exec).
pub fn test(re: &VsRegExp, subject: &VsString) -> bool {
    let text = subject.to_rust();
    let global = re.flags & flag::GLOBAL != 0;
    let start = if global { re.last_index.get() } else { 0 };
    match find_at(re, &text, start) {
        Some(caps) => {
            if global {
                let end = caps.get(0).expect("group 0").end();
                re.last_index.set(utf16_of_byte(&text, end));
            }
            true
        }
        None => {
            if global {
                re.last_index.set(0);
            }
            false
        }
    }
}

/// §15.5.4.10 String.match: non-global = exec shape; global = Array of
/// all full matches (empty-match positions advance by one; lastIndex
/// resets), null when nothing matched.
pub fn string_match(subject: &VsString, re: &VsRegExp) -> *const seq::VsArray {
    if re.flags & flag::GLOBAL == 0 {
        return exec(re, subject);
    }
    let text = subject.to_rust();
    re.last_index.set(0);
    let mut items: Vec<VsAny> = Vec::new();
    let mut start16 = 0usize;
    let len16: usize = text.chars().map(char::len_utf16).sum();
    while start16 <= len16 {
        let Some(caps) = find_at(re, &text, start16) else {
            break;
        };
        let m = caps.get(0).expect("group 0");
        items.push(VsAny::string(VsString::from_rust(m.as_str())));
        let end16 = utf16_of_byte(&text, m.end());
        start16 = if end16 == start16 { end16 + 1 } else { end16 };
    }
    if items.is_empty() {
        std::ptr::null()
    } else {
        seq::new_array(items)
    }
}

/// §15.5.4.12 String.search: UTF-16 index of the first match or -1
/// (ignores global/lastIndex).
pub fn string_search(subject: &VsString, re: &VsRegExp) -> i32 {
    let text = subject.to_rust();
    match find_at(re, &text, 0) {
        Some(caps) => utf16_of_byte(&text, caps.get(0).expect("group 0").start()) as i32,
        None => -1,
    }
}

/// §15.5.4.11 String.replace with a RegExp pattern: all occurrences when
/// global, else the first. `$$ $& $1..$99` substitutions in `repl`.
pub fn string_replace(subject: &VsString, re: &VsRegExp, repl: &VsString) -> *const VsString {
    let text = subject.to_rust();
    let repl = repl.to_rust();
    let global = re.flags & flag::GLOBAL != 0;
    let mut out = String::new();
    let mut cursor = 0usize; // byte position consumed so far
    loop {
        let caps = match re.compiled.captures_from_pos(&text, cursor) {
            Ok(Some(c)) => c,
            Ok(None) => break,
            Err(e) => exc::throw_error(exc::ErrorKind::Error, &format!("regular expression: {e}")),
        };
        let m = caps.get(0).expect("group 0");
        out.push_str(&text[cursor..m.start()]);
        substitute(&mut out, &repl, &caps);
        cursor = if m.end() == m.start() {
            // Empty match: emit the next char and step past it.
            match text[m.end()..].chars().next() {
                Some(c) => {
                    out.push(c);
                    m.end() + c.len_utf8()
                }
                None => {
                    cursor = m.end();
                    break;
                }
            }
        } else {
            m.end()
        };
        if !global {
            break;
        }
        if cursor > text.len() {
            break;
        }
    }
    out.push_str(&text[cursor.min(text.len())..]);
    VsString::from_rust(&out)
}

/// `$`-substitution into `out` (§15.5.4.11 Table 22: $$, $&, $n, $nn).
fn substitute(out: &mut String, repl: &str, caps: &fancy_regex::Captures) {
    let bytes = repl.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'$' => {
                    out.push('$');
                    i += 2;
                }
                b'&' => {
                    out.push_str(caps.get(0).map(|m| m.as_str()).unwrap_or(""));
                    i += 2;
                }
                b'0'..=b'9' => {
                    // Two-digit group first ($nn), then single ($n).
                    let d1 = (bytes[i + 1] - b'0') as usize;
                    let two = (i + 2 < bytes.len() && bytes[i + 2].is_ascii_digit())
                        .then(|| d1 * 10 + (bytes[i + 2] - b'0') as usize)
                        .filter(|&n| n >= 1 && n < caps.len());
                    if let Some(n) = two {
                        out.push_str(caps.get(n).map(|m| m.as_str()).unwrap_or(""));
                        i += 3;
                    } else if d1 >= 1 && d1 < caps.len() {
                        out.push_str(caps.get(d1).map(|m| m.as_str()).unwrap_or(""));
                        i += 2;
                    } else {
                        out.push('$');
                        i += 1;
                    }
                }
                _ => {
                    out.push('$');
                    i += 1;
                }
            }
        } else {
            let c = repl[i..].chars().next().expect("char");
            out.push(c);
            i += c.len_utf8();
        }
    }
}

/// `/source/flags` display (§15.10.6.4).
pub fn to_display(re: &VsRegExp) -> String {
    // SAFETY: source is a live GC string set at construction.
    let src = unsafe { crate::string::deref(re.source) }
        .map(|s| s.to_rust())
        .unwrap_or_default();
    let mut s = format!("/{src}/");
    for (bit, ch) in [
        (flag::GLOBAL, 'g'),
        (flag::IGNORE_CASE, 'i'),
        (flag::MULTILINE, 'm'),
        (flag::DOTALL, 's'),
        (flag::EXTENDED, 'x'),
    ] {
        if re.flags & bit != 0 {
            s.push(ch);
        }
    }
    s
}
