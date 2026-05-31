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

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Module {
    pub name: String,
    pub structs: Vec<StructDef>,
    pub funcs: Vec<Function>,
}

impl Module {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            structs: Vec::new(),
            funcs: Vec::new(),
        }
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
