//! Exceptions (SPECS §7): a setjmp/longjmp unwinding scheme — the
//! documented v1 choice (Itanium landing pads are a P8 upgrade path).
//!
//! Compiled code calls `_setjmp` directly (marked returns_twice by the
//! backend) and registers the buffer here; `vs_throw` longjmps to the most
//! recent handler. Handlers nest per thread (single-threaded runtime for
//! now — a `static mut`-free thread_local).
//!
//! Runtime-internal faults (TypeError/RangeError/null access) construct
//! real Error instances from descriptors the compiled program registers at
//! startup, so user `catch (e:TypeError)` works on them too.
//!
//! Layout contract with the prelude Error class (keep in sync with
//! driver's PRELUDE source): slot 0 = message:String?, slot 1 =
//! name:String?, slot 2 = errorID:int.

use std::cell::RefCell;

use crate::any::VsAny;
use crate::object::VsClassDesc;
use crate::string::VsString;

/// Registered error kinds, ordered as the codegen registration call sends
/// them: Error, TypeError, RangeError, ReferenceError, ArgumentError,
/// SyntaxError.
#[derive(Clone, Copy)]
#[allow(missing_docs, dead_code)] // Error reserved for future internal faults
pub enum ErrorKind {
    Error = 0,
    Type = 1,
    Range = 2,
}

struct ErrorTable {
    descs: Vec<*const VsClassDesc>,
    sizes: Vec<u64>,
}

thread_local! {
    /// Active setjmp buffers, innermost last.
    static HANDLERS: RefCell<Vec<*mut u8>> = const { RefCell::new(Vec::new()) };
    /// The in-flight exception between longjmp and the catch dispatch.
    static CURRENT: RefCell<VsAny> = const { RefCell::new(VsAny::UNDEFINED) };
    static ERRORS: RefCell<ErrorTable> = const {
        RefCell::new(ErrorTable {
            descs: Vec::new(),
            sizes: Vec::new(),
        })
    };
}

unsafe extern "C" {
    /// libc longjmp without signal-mask restore (matches `_setjmp`).
    #[link_name = "_longjmp"]
    fn c_longjmp(buf: *mut u8, val: i32) -> !;
}

/// Registers the compiled program's Error descriptors (called from the
/// script prologue).
pub fn register_errors(descs: &[*const VsClassDesc], sizes: &[u64]) {
    ERRORS.with(|t| {
        let mut t = t.borrow_mut();
        t.descs = descs.to_vec();
        t.sizes = sizes.to_vec();
    });
}

/// Pushes an active handler buffer.
pub fn push_handler(buf: *mut u8) {
    HANDLERS.with(|h| h.borrow_mut().push(buf));
}

/// Pops the innermost handler (normal try exit).
pub fn pop_handler() {
    HANDLERS.with(|h| {
        h.borrow_mut().pop();
    });
}

/// Takes the in-flight exception (catch dispatch).
pub fn take_current() -> VsAny {
    CURRENT.with(|c| std::mem::replace(&mut *c.borrow_mut(), VsAny::UNDEFINED))
}

/// Throws a boxed value: unwind to the innermost handler or die with the
/// value's display (uncaught).
pub fn throw(value: VsAny) -> ! {
    let handler = HANDLERS.with(|h| h.borrow_mut().pop());
    match handler {
        Some(buf) => {
            CURRENT.with(|c| *c.borrow_mut() = value);
            // SAFETY: buf was registered by compiled code via _setjmp and
            // its frame is still live (handlers pop on scope exit).
            unsafe { c_longjmp(buf, 1) }
        }
        None => {
            eprintln!("uncaught error: {}", crate::conv::any_to_display(value));
            std::process::exit(1)
        }
    }
}

/// Constructs a registered Error instance and throws it. Falls back to a
/// plain abort when the program registered no descriptors (pre-prologue
/// faults).
pub fn throw_error(kind: ErrorKind, message: &str) -> ! {
    let built = ERRORS.with(|t| {
        let t = t.borrow();
        let idx = kind as usize;
        if idx >= t.descs.len() {
            return None;
        }
        // SAFETY: descriptors/sizes come from codegen registration; the
        // Error layout contract fixes slots 0..2 (module docs).
        unsafe {
            let obj = crate::ffi::vs_alloc_object(t.descs[idx], t.sizes[idx]);
            let slots = obj.add(8) as *mut *const VsString;
            *slots = VsString::from_rust(message); // message
            *slots.add(1) = VsString::from_rust(match kind {
                ErrorKind::Error => "Error",
                ErrorKind::Type => "TypeError",
                ErrorKind::Range => "RangeError",
            }); // name
            Some(VsAny::object(obj))
        }
    });
    match built {
        Some(v) => throw(v),
        None => {
            eprintln!("TypeError: {message}");
            std::process::exit(1)
        }
    }
}
