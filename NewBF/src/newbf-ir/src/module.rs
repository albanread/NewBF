//! A compilation module — a flat list of functions (definitions and
//! extern declarations). Aggregates/globals arrive with later sprints.
//!
//! The module is **environment-agnostic**: it carries no notion of "app"
//! vs. "comptime". Which world a module is lowered/JIT'd into is decided by
//! the lowering + JIT layer (the `world`-parameterized pipeline), not baked
//! into the IR — so the same IR serves both.

use crate::func::Function;
use crate::ty::{IrType, StructId};

/// One field of a [`StructDef`]: its source name (for reports) and IR type.
#[derive(Clone, PartialEq, Debug)]
pub struct FieldDef {
    pub name: String,
    pub ty: IrType,
}

/// An aggregate type's layout: a name and its ordered fields. Concrete
/// offsets/sizes are derived by the backend from the field types (LLVM struct
/// layout); the IR keeps only the logical field order. Referenced from
/// [`IrType::Struct`](crate::IrType::Struct) by index into [`Module::structs`].
#[derive(Clone, PartialEq, Debug)]
pub struct StructDef {
    pub name: String,
    pub fields: Vec<FieldDef>,
}

/// A class ClassVData: a named global `%ClassVData = { i32 mType, [N x ptr] }`
/// holding the dense type-id word plus an ordered array of virtual-slot function
/// pointers. RF-T2: `new` stores its address into the object's `$header` (every
/// `StructKind::Ref` id gets one — entries empty ⇒ `[0 x ptr]`); a virtual /
/// interface call reaches the vtable via a struct-GEP into field 1
/// (`VtableBase`) then loads the slot and calls it indirectly. (Pre-RF-T2 this
/// was a bare `[N x ptr]` array; the `i32 mType` prefix shifted every slot, so
/// all dispatch routes through `VtableBase`.)
#[derive(Clone, PartialEq, Debug)]
pub struct VtableDef {
    /// Global symbol name (e.g. `"Dog.$cvdata"`), referenced by `GlobalAddr`.
    pub name: String,
    /// Slot → implementing function name, in slot order.
    pub entries: Vec<String>,
    /// The class's dense runtime **type-id** — the `i32 mType` word that leads
    /// the `ClassVData { i32 mType, [N x ptr] }` header (the reflection registry
    /// index). **RF-T1 only declares the field (default `0`); RF-T3 populates it
    /// (name-sorted dense ids), and RF-T2/RF-T5 read it** when reworking the
    /// header ABI / lowering `GetType`.
    pub type_id: u32,
}

/// The per-type reflection **strip policy** — a bitflags-like set deciding which
/// metadata tables a type emits. Computed by sema from `[Reflect(flags)]` /
/// `[AlwaysInclude]` + a module default (RF-T3); consumed by the backend
/// (RF-T4) to gate field/method table emission. `TYPE` (name+id+size) is the
/// always-on minimum.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ReflectPolicy(pub u32);

impl ReflectPolicy {
    pub const NONE: ReflectPolicy = ReflectPolicy(0);
    pub const FIELDS: ReflectPolicy = ReflectPolicy(1);
    pub const METHODS: ReflectPolicy = ReflectPolicy(2);
    /// Always-on minimum (name + id + size).
    pub const TYPE: ReflectPolicy = ReflectPolicy(4);
    pub const ALL: ReflectPolicy = ReflectPolicy(7);

    /// `true` when `self` includes every bit of `b`.
    pub fn has(self, b: ReflectPolicy) -> bool {
        self.0 & b.0 == b.0
    }
}

/// One reflected field of a [`TypeMeta`]: its source name, IR type, and ordinal
/// field index in the struct body. Emitted (policy-gated) as a `%FieldInfo`
/// constant by the backend (RF-T4/RF-T6).
#[derive(Clone, PartialEq, Debug)]
pub struct FieldMeta {
    pub name: String,
    pub ty: IrType,
    pub field_index: u32,
}

/// One reflected method of a [`TypeMeta`]: its source name, mangled symbol, and
/// parameter count. Emitted (policy-gated) as a `%MethodInfo` constant by the
/// backend (RF-T7).
#[derive(Clone, PartialEq, Debug)]
pub struct MethodMeta {
    pub name: String,
    pub symbol: String,
    pub param_count: u32,
}

/// One per reflectable type. Recorded by sema (RF-T3) into [`Module::type_meta`]
/// and emitted (policy-gated) as a constant `%struct.Type` global by the backend
/// (RF-T4). Owned data only — no lifetimes, so [`IrType`] stays `Copy` and the
/// metadata lives entirely on the [`Module`].
#[derive(Clone, PartialEq, Debug)]
pub struct TypeMeta {
    /// The dense runtime type-id (matches the owning [`VtableDef::type_id`]).
    pub type_id: u32,
    /// For the backend to compute instance size + field offsets at emit time.
    pub struct_id: StructId,
    /// The simple type name, e.g. `"Dog"`.
    pub name: String,
    pub policy: ReflectPolicy,
    /// Class (heap, has `ClassVData`) vs value struct.
    pub is_ref: bool,
    /// Empty unless `policy.has(FIELDS)`.
    pub fields: Vec<FieldMeta>,
    /// Empty unless `policy.has(METHODS)`.
    pub methods: Vec<MethodMeta>,
}

/// A mutable module global (a `static` field). Emitted zero-initialized.
#[derive(Clone, PartialEq, Debug)]
pub struct GlobalDef {
    pub name: String,
    pub ty: IrType,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Module {
    pub name: String,
    pub structs: Vec<StructDef>,
    pub funcs: Vec<Function>,
    pub vtables: Vec<VtableDef>,
    pub globals: Vec<GlobalDef>,
    /// Mangled symbols of `[Comptime]` functions — code meant to run *at compile
    /// time*, not in the final program. The comptime evaluator JIT-evaluates each
    /// and folds its call sites into literals (then drops the function), so this
    /// list is what tells the fold pass which functions are compile-time only.
    /// Empty for any module without `[Comptime]` methods.
    pub comptime: Vec<String>,
    /// Per-type reflection metadata, one [`TypeMeta`] per reflectable type.
    /// **Default empty** — a program with no reflectable types pays nothing.
    /// Populated by sema (RF-T3); emitted as Type globals by the backend
    /// (RF-T4). Dead until those tasks wire it.
    pub type_meta: Vec<TypeMeta>,
}

impl Module {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            structs: Vec::new(),
            funcs: Vec::new(),
            vtables: Vec::new(),
            globals: Vec::new(),
            comptime: Vec::new(),
            type_meta: Vec::new(),
        }
    }

    /// Register a class vtable global.
    pub fn add_vtable(&mut self, def: VtableDef) {
        self.vtables.push(def);
    }

    /// Record one type's reflection metadata. Sema calls this (RF-T3) after
    /// monomorphization + vtable layout, so field indices and method symbols are
    /// final; the backend (RF-T4) iterates `type_meta` to emit Type globals.
    pub fn add_type_meta(&mut self, meta: TypeMeta) {
        self.type_meta.push(meta);
    }

    /// Register a mutable module global (a `static` field).
    pub fn add_global(&mut self, g: GlobalDef) {
        self.globals.push(g);
    }

    /// Register an aggregate layout, returning its [`StructId`] handle.
    pub fn add_struct(&mut self, def: StructDef) -> StructId {
        let id = StructId(self.structs.len() as u32);
        self.structs.push(def);
        id
    }

    /// The layout behind a [`StructId`] (ids come only from
    /// [`add_struct`](Self::add_struct) on this same module).
    pub fn struct_def(&self, id: StructId) -> &StructDef {
        &self.structs[id.0 as usize]
    }

    pub fn add_function(&mut self, f: Function) {
        self.funcs.push(f);
    }

    /// Declare a body-less external function (FFI import / runtime shim).
    pub fn declare_extern(
        &mut self,
        name: impl Into<String>,
        params: Vec<crate::func::Param>,
        ret: crate::ty::IrType,
    ) {
        self.funcs.push(Function {
            name: name.into(),
            params,
            ret,
            blocks: Vec::new(),
            insts: Vec::new(),
            is_extern: true,
        });
    }
}
