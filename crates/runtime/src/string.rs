//! The runtime string: immutable UTF-16 (SPECS §4.4 — UTF-16 storage by
//! default so all observable behavior matches AS3).

/// An immutable UTF-16 string. Compiled code holds `*const VsString`; a
/// null pointer is the AS3 `null` String.
#[repr(C)]
pub struct VsString {
    /// Number of UTF-16 code units.
    pub len: u32,
    /// Code units (allocation lives as long as the process; see crate docs
    /// on P3 memory management).
    pub data: *const u16,
}

impl VsString {
    /// Allocates a runtime string from UTF-16 code units (GC block; the
    /// unit buffer stays on the Rust heap and is freed on sweep).
    pub fn alloc(units: Vec<u16>) -> *const VsString {
        let len = u32::try_from(units.len()).expect("string too large");
        let data = Box::leak(units.into_boxed_slice()).as_ptr();
        let p = crate::gc::alloc(std::mem::size_of::<VsString>(), crate::gc::Kind::String)
            as *mut VsString;
        // SAFETY: fresh block of exactly VsString size.
        unsafe { p.write(VsString { len, data }) };
        p
    }

    /// Allocates from a Rust string.
    pub fn from_rust(s: &str) -> *const VsString {
        Self::alloc(s.encode_utf16().collect())
    }

    /// The code units. Safe accessor over the raw parts.
    pub fn units(&self) -> &[u16] {
        // SAFETY: `data`/`len` always come from `alloc`, which leaks the
        // boxed slice — the allocation is live and exactly `len` long.
        unsafe { std::slice::from_raw_parts(self.data, self.len as usize) }
    }

    /// Lossy conversion to a Rust string (unpaired surrogates → U+FFFD).
    pub fn to_rust(&self) -> String {
        String::from_utf16_lossy(self.units())
    }
}

/// First index of `nee` in `hay` at or after `from` (UTF-16 code-unit
/// subsequence search). An empty needle matches at `from` (clamped).
/// P26: keeps string ops on `u16` instead of transcoding to UTF-8.
pub fn find_units(hay: &[u16], nee: &[u16], from: usize) -> Option<usize> {
    if nee.is_empty() {
        return Some(from.min(hay.len()));
    }
    if from > hay.len() || nee.len() > hay.len() - from {
        return None;
    }
    hay[from..].windows(nee.len()).position(|w| w == nee).map(|p| from + p)
}

/// UTF-16-native case mapping (ES-262 §15.5.4.16/§15.5.4.18). ASCII strings
/// take a byte-wise fast path; others decode surrogate pairs to scalars,
/// apply the Unicode default case mapping, and re-encode — never touching
/// UTF-8.
pub fn case_units(u: &[u16], upper: bool) -> Vec<u16> {
    if u.iter().all(|&c| c < 0x80) {
        return u
            .iter()
            .map(|&c| {
                let b = c as u8;
                u16::from(if upper {
                    b.to_ascii_uppercase()
                } else {
                    b.to_ascii_lowercase()
                })
            })
            .collect();
    }
    let mut out = Vec::with_capacity(u.len());
    let mut buf = [0u16; 2];
    for ch in char::decode_utf16(u.iter().copied()) {
        let c = ch.unwrap_or('\u{FFFD}');
        if upper {
            for m in c.to_uppercase() {
                out.extend_from_slice(m.encode_utf16(&mut buf));
            }
        } else {
            for m in c.to_lowercase() {
                out.extend_from_slice(m.encode_utf16(&mut buf));
            }
        }
    }
    out
}

/// Reads a possibly-null string pointer. `None` = AS3 `null`.
///
/// # Safety
/// `ptr` must be null or a pointer returned by [`VsString::alloc`].
pub unsafe fn deref<'a>(ptr: *const VsString) -> Option<&'a VsString> {
    // SAFETY: contract above; allocations are never freed in P3.
    unsafe { ptr.as_ref() }
}
