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

/// Reads a possibly-null string pointer. `None` = AS3 `null`.
///
/// # Safety
/// `ptr` must be null or a pointer returned by [`VsString::alloc`].
pub unsafe fn deref<'a>(ptr: *const VsString) -> Option<&'a VsString> {
    // SAFETY: contract above; allocations are never freed in P3.
    unsafe { ptr.as_ref() }
}
