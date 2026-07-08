//! First-class namespace values + the reflection tables behind
//! runtime-computed qualification (ES4 draft namespaces, SPECS §5 P16).
//!
//! Namespaces are **interned by URI**: `Namespace("x") == Namespace("x")`
//! must hold (URI identity), and interning makes plain pointer equality
//! correct everywhere — boxed, unboxed, across `new Namespace` and
//! declared-namespace values. Interned objects are immortal (leaked, not
//! GC blocks); a program touches a handful.
//!
//! Qualified access `obj.q::name` resolves against per-class member
//! tables the backend emits into the class descriptor (offset/vtable
//! info per namespaced member; see codegen's layout contract).

use std::cell::RefCell;
use std::collections::HashMap;

use crate::any::{Tag, VsAny};
use crate::closure;
use crate::exc;
use crate::object::VsClassDesc;
use crate::string::VsString;

/// A namespace value: just its canonical URI (identity = pointer,
/// guaranteed by interning).
#[repr(C)]
pub struct VsNamespace {
    /// Canonical URI (immortal runtime string).
    pub uri: *const VsString,
}

thread_local! {
    static INTERNED: RefCell<HashMap<String, usize>> = RefCell::new(HashMap::new());
}

/// Interns `uri`, returning the canonical namespace object for it.
pub fn intern(uri: &str) -> *const VsNamespace {
    INTERNED.with(|m| {
        let mut m = m.borrow_mut();
        if let Some(&p) = m.get(uri) {
            return p as *const VsNamespace;
        }
        // Immortal: not a GC block (interned values never die), and the
        // uri string is leaked alongside it.
        let ns = Box::leak(Box::new(VsNamespace {
            uri: leak_string(uri),
        }));
        m.insert(uri.to_string(), ns as *const VsNamespace as usize);
        ns
    })
}

/// A non-GC (immortal) runtime string for interned namespace URIs.
fn leak_string(s: &str) -> *const VsString {
    let units: Vec<u16> = s.encode_utf16().collect();
    let len = units.len() as u32;
    let data = Box::leak(units.into_boxed_slice()).as_ptr();
    Box::leak(Box::new(VsString { len, data }))
}

/// One namespaced member in a class descriptor's reflection table.
/// Layout contract with codegen (keep in sync).
#[repr(C)]
pub struct VsMemberInfo {
    /// Canonical namespace URI of the member.
    pub uri: *const VsString,
    /// Raw (unqualified) member name.
    pub name: *const VsString,
    /// 0 = field, 1 = method.
    pub kind: u32,
    /// For fields: the VsAny tag the slot boxes to (0 = `*` slot,
    /// copied as-is).
    pub type_tag: u32,
    /// For fields: byte offset in the instance; for methods: the
    /// boxed-ABI wrapper function pointer (fn(env, this, argc, args,
    /// out)) as an integer.
    pub payload: u64,
}

/// Finds `(uri, name)` in the receiver's descriptor chain.
///
/// # Safety
/// `obj` live.
unsafe fn find_member(obj: *const u8, uri: &str, name: &str) -> Option<&'static VsMemberInfo> {
    // SAFETY: header word 0 is the descriptor (object model contract).
    let mut cur = unsafe { *(obj as *const *const VsClassDesc) };
    while !cur.is_null() {
        // SAFETY: descriptors are static data.
        let d = unsafe { &*cur };
        for i in 0..d.member_count as usize {
            // SAFETY: member_count/members are emitted together.
            let m = unsafe { &*(d.members as *const VsMemberInfo).add(i) };
            // SAFETY: table strings are static runtime strings.
            let (m_uri, m_name) = unsafe {
                (
                    crate::string::deref(m.uri).map(|s| s.to_rust()),
                    crate::string::deref(m.name).map(|s| s.to_rust()),
                )
            };
            if m_uri.as_deref() == Some(uri) && m_name.as_deref() == Some(name) {
                return Some(m);
            }
        }
        cur = d.parent;
    }
    None
}

/// Boxes a field slot at `obj + offset` according to its type tag.
///
/// # Safety
/// `obj` live; offset/tag from a descriptor table.
unsafe fn box_field(obj: *const u8, tag: u32, offset: u64) -> VsAny {
    // SAFETY: caller contract — the backend computed the offset from the
    // instance layout for exactly this tag's storage type.
    unsafe {
        let p = obj.add(offset as usize);
        match tag {
            0 => *(p as *const VsAny),
            2 => VsAny::boolean(*p != 0),
            3 => VsAny::int(*(p as *const i32)),
            4 => VsAny::uint(*(p as *const u32)),
            5 => VsAny::number(*(p as *const f64)),
            t => {
                let ptr = *(p as *const *const u8);
                if ptr.is_null() {
                    return VsAny::NULL;
                }
                VsAny {
                    tag: t,
                    data: ptr as u64,
                }
            }
        }
    }
}

/// `obj.q::name` read: field value boxed, or a bound Function value for
/// a method; missing member throws ReferenceError.
///
/// # Safety
/// Pointers live.
pub unsafe fn get(obj: *const u8, ns: &VsNamespace, name: &str, out: *mut VsAny) {
    // SAFETY: interned namespace strings are live.
    let uri = unsafe { crate::string::deref(ns.uri) }
        .map(|s| s.to_rust())
        .unwrap_or_default();
    // SAFETY: caller contract.
    match unsafe { find_member(obj, &uri, name) } {
        Some(m) if m.kind == 0 => {
            // SAFETY: table contract.
            unsafe { *out = box_field(obj, m.type_tag, m.payload) };
        }
        Some(m) => {
            let f = closure::new_closure(m.payload as usize as *const u8, Vec::new(), obj);
            // SAFETY: out is writable (caller contract).
            unsafe {
                *out = VsAny {
                    tag: Tag::Function as u32,
                    data: f as u64,
                }
            };
        }
        None => exc::throw_error(
            exc::ErrorKind::Reference,
            &format!("no member `{name}` in namespace `{uri}`"),
        ),
    }
}

/// `obj.q::name(args)` call (methods dispatch through their wrapper;
/// Function-typed fields are invoked as values).
///
/// # Safety
/// Pointers live; `args` points to `argc` boxed values.
pub unsafe fn call(
    obj: *const u8,
    ns: &VsNamespace,
    name: &str,
    argc: u32,
    args: *const VsAny,
    out: *mut VsAny,
) {
    // SAFETY: interned namespace strings are live.
    let uri = unsafe { crate::string::deref(ns.uri) }
        .map(|s| s.to_rust())
        .unwrap_or_default();
    // SAFETY: caller contract.
    match unsafe { find_member(obj, &uri, name) } {
        Some(m) if m.kind == 1 => {
            let wrapper: closure::Wrapper =
                // SAFETY: payload is a codegen wrapper for methods.
                unsafe { std::mem::transmute(m.payload as usize as *const u8) };
            // SAFETY: wrapper ABI (closure.rs module docs).
            unsafe { wrapper(std::ptr::null(), obj, argc, args, out) };
        }
        Some(m) => {
            // SAFETY: table contract.
            let v = unsafe { box_field(obj, m.type_tag, m.payload) };
            if v.tag() != Tag::Function {
                exc::throw_error(
                    exc::ErrorKind::Type,
                    &format!("`{name}` in namespace `{uri}` is not callable"),
                );
            }
            // SAFETY: Function payloads are live closures.
            unsafe {
                closure::invoke(
                    v.data as usize as *const closure::VsClosure,
                    std::ptr::null(),
                    argc,
                    args,
                    out,
                )
            };
        }
        None => exc::throw_error(
            exc::ErrorKind::Reference,
            &format!("no member `{name}` in namespace `{uri}`"),
        ),
    }
}
