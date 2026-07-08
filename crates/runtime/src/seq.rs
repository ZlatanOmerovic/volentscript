//! Array and `Vector.<T>` runtime objects (SPECS §3.10, §4.3).
//!
//! Array stores boxed elements (`VsAny`). Vector carries its instantiation
//! id — the reified element type (SPECS §4.2): `is Vector.<int>` compares
//! ids — and, per P23, an **unboxed** contiguous buffer for the numeric
//! element kinds (`Number`/`int`/`uint`). Numeric `Vector` elements are
//! stored as raw `f64`/`i32`/`u32` in a flat, `#[repr(C)]`-visible buffer
//! so compiled code can inline `v[i]` as a bounds-checked load/store with
//! no runtime call or boxing (avmplus stores typed Vectors unboxed too;
//! we mirror that layout). Non-numeric instantiations keep boxed storage.
//!
//! Array holes are stored as `undefined` (dense storage; enumeration
//! semantics for sparse arrays arrive with `for..in` in P6).

use std::cell::RefCell;

use crate::any::VsAny;
use crate::conv;

/// The Array runtime object.
pub struct VsArray {
    /// Elements (holes = undefined).
    pub data: RefCell<Vec<VsAny>>,
}

/// Element storage kind of a `Vector` (matches codegen's `vec_kind`).
/// `Boxed` holds `VsAny` (16 B); the numeric kinds hold raw scalars.
pub const VEC_BOXED: u32 = 0;
pub const VEC_F64: u32 = 1;
pub const VEC_I32: u32 = 2;
pub const VEC_U32: u32 = 3;

/// The Vector runtime object — flat, `#[repr(C)]` so codegen can read
/// `len` (offset 8) and `data` (offset 16) to inline numeric element
/// access. The buffer is a raw `malloc` allocation (NOT a GC block);
/// it is grown in place and freed with the header (`Drop`). Boxed
/// vectors keep their `VsAny`s here too, traced precisely by the GC.
#[repr(C)]
pub struct VsVector {
    /// Reified instantiation id (`Ty::Vector` payload).
    pub inst: u32,
    /// Element storage kind (`VEC_*`).
    pub kind: u32,
    /// Element count.
    pub len: u32,
    /// Allocated capacity in elements.
    pub cap: u32,
    /// Element buffer (`null` when `cap == 0`).
    pub data: *mut u8,
}

impl Drop for VsVector {
    fn drop(&mut self) {
        if !self.data.is_null() {
            // SAFETY: buffer was allocated by `alloc_buf` with this layout.
            unsafe {
                std::alloc::dealloc(self.data, buf_layout(self.cap, self.kind));
            }
        }
    }
}

/// Byte stride of one element for a storage kind.
#[inline]
pub fn vec_stride(kind: u32) -> usize {
    match kind {
        VEC_F64 => 8,
        VEC_I32 | VEC_U32 => 4,
        _ => std::mem::size_of::<VsAny>(),
    }
}

fn buf_layout(cap: u32, kind: u32) -> std::alloc::Layout {
    let stride = vec_stride(kind);
    // SAFETY: stride is a small power-of-two-friendly element size; cap*stride
    // cannot overflow isize for any vector we can actually allocate.
    unsafe { std::alloc::Layout::from_size_align_unchecked(cap as usize * stride, 8) }
}

impl VsVector {
    /// Reads element `i` (assumed in range) boxed as a `VsAny`.
    #[inline]
    pub fn get_any(&self, i: usize) -> VsAny {
        // SAFETY: `i < len` and the buffer holds `len` initialized elements.
        unsafe {
            match self.kind {
                VEC_F64 => VsAny::number(*(self.data as *const f64).add(i)),
                VEC_I32 => VsAny::int(*(self.data as *const i32).add(i)),
                VEC_U32 => VsAny::uint(*(self.data as *const u32).add(i)),
                _ => *(self.data as *const VsAny).add(i),
            }
        }
    }

    /// Writes `v` into element `i` (assumed in range), coercing to the
    /// storage kind (ES-262 §9.3/§9.5/§9.6 at the boundary).
    #[inline]
    pub fn set_any(&mut self, i: usize, v: VsAny) {
        // SAFETY: `i < len`; buffer sized for `kind`.
        unsafe {
            match self.kind {
                VEC_F64 => *(self.data as *mut f64).add(i) = conv::any_to_number(v),
                VEC_I32 => *(self.data as *mut i32).add(i) = conv::to_int32(conv::any_to_number(v)),
                VEC_U32 => {
                    *(self.data as *mut u32).add(i) = conv::to_uint32(conv::any_to_number(v))
                }
                _ => *(self.data as *mut VsAny).add(i) = v,
            }
        }
    }

    fn reserve(&mut self, need: u32) {
        if need <= self.cap {
            return;
        }
        let new_cap = need.max(self.cap.saturating_mul(2)).max(4);
        // SAFETY: realloc from the old layout to the new capacity's layout.
        unsafe {
            let stride = vec_stride(self.kind);
            let new = if self.data.is_null() {
                std::alloc::alloc(buf_layout(new_cap, self.kind))
            } else {
                std::alloc::realloc(
                    self.data,
                    buf_layout(self.cap, self.kind),
                    new_cap as usize * stride,
                )
            };
            assert!(!new.is_null(), "vector buffer allocation failed");
            self.data = new;
            self.cap = new_cap;
        }
    }

    /// Appends one boxed value, coercing to the storage kind.
    pub fn push_any(&mut self, v: VsAny) {
        self.reserve(self.len + 1);
        let i = self.len as usize;
        self.len += 1;
        self.set_any(i, v);
    }

    /// Removes and returns the last element, or `None` if empty.
    pub fn pop_any(&mut self) -> Option<VsAny> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        Some(self.get_any(self.len as usize))
    }

    /// Snapshots all elements boxed (for join/indexOf/toString/reverse —
    /// paths where per-element boxing cost is irrelevant).
    pub fn to_boxed(&self) -> Vec<VsAny> {
        (0..self.len as usize).map(|i| self.get_any(i)).collect()
    }

    /// Rebuilds the buffer from a boxed slice (used by mutating helpers
    /// that are simplest to express over `Vec<VsAny>`).
    pub fn refill(&mut self, items: &[VsAny]) {
        self.len = 0;
        self.reserve(items.len() as u32);
        for &it in items {
            self.push_any(it);
        }
    }

    /// Resizes to `n` elements; new slots are the kind's zero value.
    pub fn resize(&mut self, n: u32) {
        if n <= self.len {
            self.len = n;
            return;
        }
        self.reserve(n);
        let zero = match self.kind {
            VEC_F64 => VsAny::number(0.0),
            VEC_I32 => VsAny::int(0),
            VEC_U32 => VsAny::uint(0),
            _ => VsAny::UNDEFINED,
        };
        for i in self.len as usize..n as usize {
            self.set_any(i, zero);
        }
        self.len = n;
    }
}

/// Allocates a new array (GC block; elements traced precisely).
pub fn new_array(elements: Vec<VsAny>) -> *const VsArray {
    let p =
        crate::gc::alloc(std::mem::size_of::<VsArray>(), crate::gc::Kind::Array) as *mut VsArray;
    // SAFETY: fresh block of exactly VsArray size.
    unsafe {
        p.write(VsArray {
            data: RefCell::new(elements),
        })
    };
    p
}

/// Allocates a new vector of the given storage `kind` (GC block header;
/// the element buffer is a separate `malloc` allocation freed on sweep).
pub fn new_vector(inst: u32, kind: u32, elements: Vec<VsAny>) -> *const VsVector {
    let p =
        crate::gc::alloc(std::mem::size_of::<VsVector>(), crate::gc::Kind::Vector) as *mut VsVector;
    // SAFETY: fresh block of exactly VsVector size; buffer filled below.
    unsafe {
        p.write(VsVector {
            inst,
            kind,
            len: 0,
            cap: 0,
            data: std::ptr::null_mut(),
        });
        (*p).refill(&elements);
    };
    p
}

/// ToString for arrays/vectors: elements joined with "," (ES3 §15.4.4.2).
pub fn join(data: &[VsAny], sep: &str) -> String {
    data.iter()
        .map(|v| match v.tag() {
            // null/undefined stringify to "" inside join (§15.4.4.3).
            crate::any::Tag::Null | crate::any::Tag::Undefined => String::new(),
            _ => conv::any_to_display(*v),
        })
        .collect::<Vec<_>>()
        .join(sep)
}

/// ES3 §15.4.4.12 splice / §15.4.4.10 slice index normalization.
pub fn norm_index(v: f64, len: usize) -> usize {
    if v.is_nan() {
        return 0;
    }
    if v < 0.0 {
        ((len as f64 + v).max(0.0)) as usize
    } else {
        (v as usize).min(len)
    }
}
