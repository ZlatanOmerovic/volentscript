//! Array and `Vector.<T>` runtime objects (SPECS §3.10, §4.3).
//!
//! Both store boxed elements (`VsAny`). Vector additionally carries its
//! instantiation id — the reified element type (SPECS §4.2): `is
//! Vector.<int>` compares ids. Typed element access converts at the
//! boundary in compiled code (uniform storage; the monomorphization
//! boundary is documented in the MIR crate).
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

/// The Vector runtime object.
pub struct VsVector {
    /// Reified instantiation id (`Ty::Vector` payload).
    pub inst: u32,
    /// Elements, uniformly boxed.
    pub data: RefCell<Vec<VsAny>>,
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

/// Allocates a new vector (GC block).
pub fn new_vector(inst: u32, elements: Vec<VsAny>) -> *const VsVector {
    let p =
        crate::gc::alloc(std::mem::size_of::<VsVector>(), crate::gc::Kind::Vector) as *mut VsVector;
    // SAFETY: fresh block of exactly VsVector size.
    unsafe {
        p.write(VsVector {
            inst,
            data: RefCell::new(elements),
        })
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
