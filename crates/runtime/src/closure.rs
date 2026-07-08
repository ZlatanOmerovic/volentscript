//! Function values (SPECS §3.7): closures over cell environments, plain
//! functions, and bound method closures share one representation.
//!
//! Wrapper ABI (codegen synthesizes one wrapper per function used as a
//! value): `fn(env: *const *mut VsAny, this: *const u8, argc: u32,
//! args: *const VsAny, out: *mut VsAny)`.

use crate::any::VsAny;

/// A Function value.
#[repr(C)]
pub struct VsClosure {
    /// Boxed-ABI wrapper.
    pub wrapper: *const u8,
    /// Captured environment: array of cell pointers (may be null).
    pub env: *const *mut VsAny,
    /// Bound `this` (method closures) or null.
    pub this: *const u8,
}

/// Wrapper function type (see module docs).
pub type Wrapper =
    unsafe extern "C" fn(*const *mut VsAny, *const u8, u32, *const VsAny, *mut VsAny);

/// Allocates a new closure (GC blocks: the closure and its env array are
/// plain-word blocks; cells and `this` are kept alive by conservative
/// scan of those words).
pub fn new_closure(wrapper: *const u8, env: Vec<*mut VsAny>, this: *const u8) -> *const VsClosure {
    let env_ptr = if env.is_empty() {
        std::ptr::null()
    } else {
        let block = crate::gc::alloc(
            env.len() * std::mem::size_of::<*mut VsAny>(),
            crate::gc::Kind::Raw,
        ) as *mut *mut VsAny;
        // SAFETY: block sized for env.len() pointers.
        unsafe { std::ptr::copy_nonoverlapping(env.as_ptr(), block, env.len()) };
        block as *const *mut VsAny
    };
    let p =
        crate::gc::alloc(std::mem::size_of::<VsClosure>(), crate::gc::Kind::Raw) as *mut VsClosure;
    // SAFETY: fresh block of exactly VsClosure size.
    unsafe {
        p.write(VsClosure {
            wrapper,
            env: env_ptr,
            this,
        })
    };
    p
}

/// Invokes a Function value with boxed arguments.
///
/// # Safety
/// `c` must be a live closure built by `new_closure`; args live.
pub unsafe fn invoke(
    c: *const VsClosure,
    this_override: *const u8,
    argc: u32,
    args: *const VsAny,
    out: *mut VsAny,
) {
    if c.is_null() {
        crate::exc::throw_error(crate::exc::ErrorKind::Type, "call of null Function");
    }
    // SAFETY: caller contract; wrapper pointer set at creation.
    unsafe {
        let c = &*c;
        let f: Wrapper = std::mem::transmute(c.wrapper);
        let this = if this_override.is_null() {
            c.this
        } else {
            this_override
        };
        f(c.env, this, argc, args, out);
    }
}
