//! The native object model (SPECS §7): class descriptors, instance
//! headers, `is`/`as` on class hierarchies, interface method tables.
//!
//! Layout contract with codegen (keep in sync with `codegen::llvm`):
//! an instance starts with a pointer to its class descriptor; the
//! descriptor starts with the fields below, followed by the vtable
//! (which the runtime never inspects).

use crate::any::{Tag, VsAny};
use crate::conv;
use crate::string::VsString;

/// One implemented interface: its dense id and the method table.
#[repr(C)]
pub struct VsIfacePair {
    /// Interface id (dense per program).
    pub id: u32,
    /// Padding for pointer alignment.
    pub _pad: u32,
    /// Method table: one function pointer per interface method slot.
    pub table: *const *const u8,
}

/// Class descriptor prefix (codegen appends the vtable after these fields).
#[repr(C)]
pub struct VsClassDesc {
    /// Dense class id (diagnostics only; identity is the pointer).
    pub type_id: u32,
    /// Padding for pointer alignment.
    pub _pad: u32,
    /// Parent descriptor; null for the Object root.
    pub parent: *const VsClassDesc,
    /// Qualified class name.
    pub name: *const VsString,
    /// Number of implemented interfaces.
    pub iface_count: u32,
    /// Padding.
    pub _pad2: u32,
    /// Implemented interfaces (flattened).
    pub ifaces: *const VsIfacePair,
    /// `toString():String` implementation (C ABI: fn(this) -> VsString*);
    /// null = default "[object Name]".
    pub to_string: *const u8,
}

/// Reads the descriptor of a live object.
///
/// # Safety
/// `obj` must be a live object allocated by `vs_alloc_object`.
unsafe fn desc_of<'a>(obj: *const u8) -> &'a VsClassDesc {
    // SAFETY: header word 0 is the descriptor pointer (layout contract).
    unsafe { &**(obj as *const *const VsClassDesc) }
}

/// Whether `obj`'s class is or extends the class identified by `target`.
pub fn is_class(obj: *const u8, target: *const VsClassDesc) -> bool {
    if obj.is_null() {
        return false;
    }
    // SAFETY: non-null objects are live (P3+ never frees).
    let mut cur = unsafe { desc_of(obj) } as *const VsClassDesc;
    while !cur.is_null() {
        if std::ptr::eq(cur, target) {
            return true;
        }
        // SAFETY: descriptor chain is static data.
        cur = unsafe { (*cur).parent };
    }
    false
}

/// Whether `obj` implements interface `iface_id` (checks the whole
/// ancestor chain — each descriptor lists its own flattened set).
pub fn is_iface(obj: *const u8, iface_id: u32) -> bool {
    iface_table(obj, iface_id).is_some()
}

/// Finds the method table for `iface_id` on `obj`, if implemented.
pub fn iface_table(obj: *const u8, iface_id: u32) -> Option<*const *const u8> {
    if obj.is_null() {
        return None;
    }
    // SAFETY: live object; descriptors/pairs are static data.
    unsafe {
        let mut cur = desc_of(obj) as *const VsClassDesc;
        while !cur.is_null() {
            let d = &*cur;
            for i in 0..d.iface_count as usize {
                let pair = &*d.ifaces.add(i);
                if pair.id == iface_id {
                    return Some(pair.table);
                }
            }
            cur = d.parent;
        }
    }
    None
}

/// ToString for object values: the class's `toString` override, else
/// "[object Name]" (ES3 §15.2.4.2 shape with the AS3 class name).
pub fn object_to_display(obj: *const u8) -> String {
    if obj.is_null() {
        return "null".to_string();
    }
    // SAFETY: live object.
    let d = unsafe { desc_of(obj) };
    if !d.to_string.is_null() {
        // SAFETY: layout contract — to_string is fn(*const u8) -> *const VsString.
        let f: extern "C" fn(*const u8) -> *const VsString =
            unsafe { std::mem::transmute(d.to_string) };
        let s = f(obj);
        // SAFETY: runtime strings are live or null.
        return match unsafe { crate::string::deref(s) } {
            Some(s) => s.to_rust(),
            None => "null".to_string(),
        };
    }
    // SAFETY: descriptor name is a static runtime string.
    let name = unsafe { crate::string::deref(d.name) }
        .map(|s| s.to_rust())
        .unwrap_or_default();
    format!("[object {name}]")
}

/// `is`-style check of a boxed value against a class descriptor.
pub fn any_is_class(v: VsAny, target: *const VsClassDesc) -> bool {
    v.tag() == Tag::Object && is_class(v.as_object_ptr(), target)
}

/// AVM2 `coerce` to a class: null/undefined pass through as null; a
/// matching object passes; anything else is a TypeError (abort until P6).
pub fn any_coerce_class(v: VsAny, target: *const VsClassDesc) -> *const u8 {
    match v.tag() {
        Tag::Null | Tag::Undefined => std::ptr::null(),
        Tag::Object if is_class(v.as_object_ptr(), target) => v.as_object_ptr(),
        _ => conv::type_error(&format!(
            "cannot convert {} to {}",
            conv::any_to_display(v),
            // SAFETY: static descriptor.
            unsafe { crate::string::deref((*target).name) }
                .map(|s| s.to_rust())
                .unwrap_or_default()
        )),
    }
}

/// Same, interface target.
pub fn any_coerce_iface(v: VsAny, iface_id: u32) -> *const u8 {
    match v.tag() {
        Tag::Null | Tag::Undefined => std::ptr::null(),
        Tag::Object if is_iface(v.as_object_ptr(), iface_id) => v.as_object_ptr(),
        _ => conv::type_error(&format!(
            "cannot convert {} to interface",
            conv::any_to_display(v)
        )),
    }
}
