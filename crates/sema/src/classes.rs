//! Class and interface registry: nominal type identities (SPECS §4.5),
//! slot layout, vtable construction, override and conformance checking
//! (SPECS §3.2, §3.4, §3.5).
//!
//! Object model follows the AVM2 traits design (avm2overview.pdf §4.1
//! "Traits"): a class has a fixed set of typed slots and a method table;
//! subclasses append. We emit no ABC — the layout maps to native structs
//! and vtables in codegen.

use ast::Visibility;
use span::Span;

use crate::tast::{FnId, TExpr};
use crate::ty::Ty;

/// Identity of a class (index into the registry).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClassId(pub u32);

/// Identity of an interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IfaceId(pub u32);

/// The implicit root class (SPECS §3.10 `Object`).
pub const OBJECT: ClassId = ClassId(0);

/// Instance member kind in the vtable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum VKind {
    Method,
    Getter,
    Setter,
}

/// A callable signature (methods, accessors, constructors).
#[derive(Debug, Clone)]
pub struct Sig {
    /// Parameter types.
    pub params: Vec<Ty>,
    /// Per-parameter `T?` flags (SPECS §4.1), parallel to `params`.
    pub params_nullable: Vec<bool>,
    /// Arguments before the first defaulted parameter.
    pub required: usize,
    /// Trailing `...rest`.
    pub variadic: bool,
    /// Return type.
    pub ret: Ty,
    /// `T?` return.
    pub ret_nullable: bool,
}

impl Default for Sig {
    fn default() -> Self {
        Sig {
            params: Vec::new(),
            params_nullable: Vec::new(),
            required: 0,
            variadic: false,
            ret: Ty::Void,
            ret_nullable: false,
        }
    }
}

impl Sig {
    /// Signature identity for override/conformance checks: AS3 requires the
    /// override to match the inherited signature exactly.
    pub fn matches(&self, other: &Sig) -> bool {
        self.params == other.params
            && self.params_nullable == other.params_nullable
            && self.required == other.required
            && self.variadic == other.variadic
            && self.ret == other.ret
            && self.ret_nullable == other.ret_nullable
    }
}

/// One instance field.
#[derive(Debug)]
pub struct FieldInfo {
    /// Field name.
    pub name: String,
    /// Declared type.
    pub ty: Ty,
    /// Declared `T?` (SPECS §4.1).
    pub nullable: bool,
    /// `const` — writable only inside the defining class's constructor.
    pub is_const: bool,
    /// Access control.
    pub visibility: Visibility,
    /// Initializer (runs in the constructor prologue, after `super`).
    pub init: Option<TExpr>,
    /// Slot index in the instance layout (parent slots first).
    pub slot: usize,
    /// Defining class.
    pub defined_in: ClassId,
}

/// One vtable entry.
#[derive(Debug, Clone)]
pub struct VMethod {
    /// Member name.
    pub name: String,
    /// Method / getter / setter.
    pub kind: VKind,
    /// Checked signature.
    pub sig: Sig,
    /// Body function (the current implementation for this class).
    pub fn_id: FnId,
    /// `final` — no further overrides.
    pub is_final: bool,
    /// Access control of the introducing declaration.
    pub visibility: Visibility,
    /// Class that introduced this vtable slot.
    pub introduced_in: ClassId,
}

/// One static field.
#[derive(Debug)]
pub struct StaticField {
    /// Field name.
    pub name: String,
    /// Declared type.
    pub ty: Ty,
    /// Declared `T?` (SPECS §4.1).
    pub nullable: bool,
    /// `const`.
    pub is_const: bool,
    /// Access control.
    pub visibility: Visibility,
    /// Initializer (runs in program prologue, declaration order).
    pub init: Option<TExpr>,
    /// Index into the class's static storage.
    pub index: usize,
}

/// One static method.
#[derive(Debug)]
pub struct StaticMethod {
    /// Method name.
    pub name: String,
    /// Signature.
    pub sig: Sig,
    /// Body function.
    pub fn_id: FnId,
    /// Access control.
    pub visibility: Visibility,
}

/// A registered class.
#[derive(Debug)]
pub struct ClassInfo {
    /// Simple name.
    pub name: String,
    /// Package path (empty = default package).
    pub package: Vec<String>,
    /// Superclass (`None` only for Object itself).
    pub parent: Option<ClassId>,
    /// All implemented interfaces, transitively flattened.
    pub interfaces: Vec<IfaceId>,
    /// `final` class.
    pub is_final: bool,
    /// `dynamic` class (expando behavior — parsed; runtime support is P7).
    pub is_dynamic: bool,
    /// Own fields (inherited fields live on ancestors).
    pub fields: Vec<FieldInfo>,
    /// Total instance slots including inherited (== next child's base).
    pub total_slots: usize,
    /// Full vtable, inherited entries first (overrides replace in place).
    pub vtable: Vec<VMethod>,
    /// Constructor body + signature; `None` = default constructor.
    pub ctor: Option<FnId>,
    /// Constructor signature (empty default when `ctor` is `None`).
    pub ctor_sig: Sig,
    /// Static fields.
    pub static_fields: Vec<StaticField>,
    /// Static methods.
    pub static_methods: Vec<StaticMethod>,
    /// Declaration site.
    pub span: Span,
}

impl ClassInfo {
    /// Fully qualified name for diagnostics.
    pub fn qualified(&self) -> String {
        if self.package.is_empty() {
            self.name.clone()
        } else {
            format!("{}.{}", self.package.join("."), self.name)
        }
    }
}

/// One interface method/accessor signature.
#[derive(Debug, Clone)]
pub struct IfaceMethod {
    /// Member name.
    pub name: String,
    /// Method / getter / setter.
    pub kind: VKind,
    /// Signature.
    pub sig: Sig,
}

/// A registered interface.
#[derive(Debug)]
pub struct IfaceInfo {
    /// Simple name.
    pub name: String,
    /// Package path.
    pub package: Vec<String>,
    /// Directly extended interfaces.
    pub extends: Vec<IfaceId>,
    /// Flattened method list (inherited first) — the interface dispatch
    /// table order.
    pub methods: Vec<IfaceMethod>,
    /// Declaration site.
    pub span: Span,
}

/// The type registry built during declaration collection.
#[derive(Debug, Default)]
pub struct Registry {
    /// All classes; index = ClassId. Entry 0 is Object.
    pub classes: Vec<ClassInfo>,
    /// All interfaces; index = IfaceId.
    pub ifaces: Vec<IfaceInfo>,
}

impl Registry {
    /// Creates the registry with the implicit Object root (SPECS §3.10).
    /// Object's `toString` is provided by the runtime (default
    /// "[object Class]"), so its vtable is empty here — trace/ToString go
    /// through the class descriptor.
    pub fn with_object_root() -> Registry {
        Registry {
            classes: vec![ClassInfo {
                name: "Object".to_string(),
                package: Vec::new(),
                parent: None,
                interfaces: Vec::new(),
                is_final: false,
                is_dynamic: true,
                fields: Vec::new(),
                total_slots: 0,
                vtable: Vec::new(),
                ctor: None,
                ctor_sig: Sig::default(),
                static_fields: Vec::new(),
                static_methods: Vec::new(),
                span: Span::new(span::SourceId(0), 0, 0),
            }],
            ifaces: Vec::new(),
        }
    }

    /// Whether `sub` is `sup` or a descendant of it.
    pub fn is_subclass(&self, sub: ClassId, sup: ClassId) -> bool {
        let mut cur = Some(sub);
        while let Some(c) = cur {
            if c == sup {
                return true;
            }
            cur = self.classes[c.0 as usize].parent;
        }
        false
    }

    /// Whether `class_id` implements `iface` (transitively).
    pub fn implements(&self, class_id: ClassId, iface: IfaceId) -> bool {
        let mut cur = Some(class_id);
        while let Some(c) = cur {
            if self.classes[c.0 as usize].interfaces.contains(&iface) {
                return true;
            }
            cur = self.classes[c.0 as usize].parent;
        }
        false
    }

    /// Whether interface `sub` extends (or is) `sup`.
    pub fn iface_extends(&self, sub: IfaceId, sup: IfaceId) -> bool {
        if sub == sup {
            return true;
        }
        self.ifaces[sub.0 as usize]
            .extends
            .iter()
            .any(|&e| self.iface_extends(e, sup))
    }

    /// Looks up a field by its slot index, walking the ancestor chain.
    pub fn field_by_slot(&self, class_id: ClassId, slot: usize) -> Option<&FieldInfo> {
        let mut cur = Some(class_id);
        while let Some(c) = cur {
            let info = &self.classes[c.0 as usize];
            if let Some(f) = info.fields.iter().find(|f| f.slot == slot) {
                return Some(f);
            }
            cur = info.parent;
        }
        None
    }

    /// Looks up an instance field by name, walking the ancestor chain.
    pub fn find_field(&self, class_id: ClassId, name: &str) -> Option<&FieldInfo> {
        let mut cur = Some(class_id);
        while let Some(c) = cur {
            let info = &self.classes[c.0 as usize];
            if let Some(f) = info.fields.iter().find(|f| f.name == name) {
                return Some(f);
            }
            cur = info.parent;
        }
        None
    }

    /// Looks up a vtable entry by name and kind.
    pub fn find_vmethod(
        &self,
        class_id: ClassId,
        name: &str,
        kind: VKind,
    ) -> Option<(usize, &VMethod)> {
        self.classes[class_id.0 as usize]
            .vtable
            .iter()
            .enumerate()
            .find(|(_, m)| m.name == name && m.kind == kind)
    }

    /// Looks up an interface method by name and kind (flattened order).
    pub fn find_iface_method(
        &self,
        iface: IfaceId,
        name: &str,
        kind: VKind,
    ) -> Option<(usize, &IfaceMethod)> {
        self.ifaces[iface.0 as usize]
            .methods
            .iter()
            .enumerate()
            .find(|(_, m)| m.name == name && m.kind == kind)
    }

    /// Static field/method lookup (statics do not inherit in AS3).
    pub fn find_static_field(&self, class_id: ClassId, name: &str) -> Option<&StaticField> {
        self.classes[class_id.0 as usize]
            .static_fields
            .iter()
            .find(|f| f.name == name)
    }

    /// Static method lookup.
    pub fn find_static_method(&self, class_id: ClassId, name: &str) -> Option<&StaticMethod> {
        self.classes[class_id.0 as usize]
            .static_methods
            .iter()
            .find(|m| m.name == name)
    }
}
