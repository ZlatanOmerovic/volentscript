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
    /// Byte offset of the expando-map slot for `dynamic` classes
    /// (SPECS §3.2); `u32::MAX` = sealed.
    pub expando_off: u32,
    /// Number of namespaced members in the reflection table (P16).
    pub member_count: u32,
    /// Reflection table: `member_count` `VsMemberInfo` entries
    /// (namespace.rs layout contract).
    pub members: *const u8,
}

/// Expando storage: association list preserving insertion order (AS3-ish
/// enumeration order; small objects dominate).
pub type PropMap = Vec<(Vec<u16>, VsAny)>;

/// The expando map of a dynamic instance, if the slot was initialized.
///
/// # Safety
/// `obj` live.
unsafe fn expando<'x>(obj: *const u8) -> Option<&'x mut PropMap> {
    // SAFETY: caller contract; desc chain static.
    unsafe {
        let d = desc_of(obj);
        if d.expando_off == u32::MAX {
            return None;
        }
        let slot = obj.add(d.expando_off as usize) as *mut *mut PropMap;
        if (*slot).is_null() {
            let p = crate::gc::alloc(std::mem::size_of::<PropMap>(), crate::gc::Kind::PropMap)
                as *mut PropMap;
            p.write(PropMap::new());
            *slot = p;
        }
        Some(&mut **slot)
    }
}

/// Whether the object's class is dynamic.
pub fn is_dynamic(obj: *const u8) -> bool {
    if obj.is_null() {
        return false;
    }
    // SAFETY: live object.
    unsafe { desc_of(obj).expando_off != u32::MAX }
}

/// Dynamic property read (undefined when absent — §8.6.2.1 on expandos).
pub fn get_prop(obj: *const u8, name: &[u16]) -> VsAny {
    // SAFETY: object model contract.
    match unsafe { expando(obj) } {
        Some(map) => map
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| *v)
            .unwrap_or(VsAny::UNDEFINED),
        None => VsAny::UNDEFINED,
    }
}

/// Dynamic property write. Returns false on sealed receivers (caller
/// raises the ReferenceError).
pub fn set_prop(obj: *const u8, name: &[u16], value: VsAny) -> bool {
    // SAFETY: object model contract.
    match unsafe { expando(obj) } {
        Some(map) => {
            if let Some(entry) = map.iter_mut().find(|(k, _)| k == name) {
                entry.1 = value;
            } else {
                map.push((name.to_vec(), value));
            }
            true
        }
        None => false,
    }
}

/// `name in obj` over expandos.
pub fn has_prop(obj: *const u8, name: &[u16]) -> bool {
    // SAFETY: object model contract.
    unsafe { expando(obj) }.is_some_and(|m| m.iter().any(|(k, _)| k == name))
}

/// `delete obj.name` (§11.4.1 over expandos).
pub fn delete_prop(obj: *const u8, name: &[u16]) -> bool {
    // SAFETY: object model contract.
    match unsafe { expando(obj) } {
        Some(map) => {
            let before = map.len();
            map.retain(|(k, _)| k != name);
            map.len() != before
        }
        None => false,
    }
}

/// Enumeration over expandos: count / key / value at index.
pub fn prop_count(obj: *const u8) -> usize {
    // SAFETY: object model contract.
    unsafe { expando(obj) }.map_or(0, |m| m.len())
}

/// Key at enumeration index.
pub fn prop_key_at(obj: *const u8, i: usize) -> Option<Vec<u16>> {
    // SAFETY: object model contract.
    unsafe { expando(obj) }.and_then(|m| m.get(i).map(|(k, _)| k.clone()))
}

/// Value at enumeration index.
pub fn prop_value_at(obj: *const u8, i: usize) -> VsAny {
    // SAFETY: object model contract.
    unsafe { expando(obj) }
        .and_then(|m| m.get(i).map(|(_, v)| *v))
        .unwrap_or(VsAny::UNDEFINED)
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
