//! A compilation module — a flat list of functions (definitions and
//! extern declarations). Aggregates/globals arrive with later sprints.
//!
//! The module is **environment-agnostic**: it carries no notion of "app"
//! vs. "comptime". Which world a module is lowered/JIT'd into is decided by
//! the lowering + JIT layer (the `world`-parameterized pipeline), not baked
//! into the IR — so the same IR serves both.

use crate::func::Function;

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Module {
    pub name: String,
    pub funcs: Vec<Function>,
}

impl Module {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            funcs: Vec::new(),
        }
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
