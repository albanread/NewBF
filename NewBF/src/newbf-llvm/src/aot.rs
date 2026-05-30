//! AOT object-file emission — the shipping-codegen half of "JIT *and* AOT
//! both first-class".
//!
//! Lowers the typed SSA IR to an LLVM module (the same `emit_module` the JIT
//! uses) and runs it through an LLVM `TargetMachine` to produce a native
//! object file (COFF on Windows). Linking the object(s) + the runtime into a
//! standalone `.exe` (lld / the MSVC linker) and the driver `compile` command
//! are the next step; this is the codegen foundation, verified by emitting a
//! real object and checking its container format.

use std::path::Path;
use std::sync::Once;

use inkwell::OptimizationLevel;
use inkwell::context::Context;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine, TargetTriple,
};
use newbf_ir::Module as IrModule;

use crate::lower::emit_module;

fn init_native_target() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        Target::initialize_native(&InitializationConfig::default())
            .expect("LLVM native target init failed");
    });
}

/// A `TargetMachine` for the host, plus its triple.
fn host_target_machine() -> Result<(TargetMachine, TargetTriple), String> {
    init_native_target();
    let triple = TargetMachine::get_default_triple();
    let target = Target::from_triple(&triple).map_err(|e| e.to_string())?;
    let tm = target
        .create_target_machine(
            &triple,
            "generic",
            "",
            OptimizationLevel::Default,
            RelocMode::Default,
            CodeModel::Default,
        )
        .ok_or_else(|| "failed to create host target machine".to_string())?;
    Ok((tm, triple))
}

/// Lower `ir` and emit a native object file as an in-memory byte buffer
/// (host target). The bytes are a COFF object on Windows, ELF on Linux, etc.
pub fn emit_object_to_memory(ir: &IrModule) -> Result<Vec<u8>, String> {
    let ctx = Context::create();
    let module = emit_module(&ctx, ir);
    let (tm, triple) = host_target_machine()?;
    // The object's triple + data layout must match the target machine or the
    // linker (and LLVM's own layout assumptions) disagree with the codegen.
    module.set_triple(&triple);
    module.set_data_layout(&tm.get_target_data().get_data_layout());
    let buf = tm
        .write_to_memory_buffer(&module, FileType::Object)
        .map_err(|e| e.to_string())?;
    Ok(buf.as_slice().to_vec())
}

/// Lower `ir` and write a native object file to `path` (host target).
pub fn emit_object(ir: &IrModule, path: &Path) -> Result<(), String> {
    let bytes = emit_object_to_memory(ir)?;
    std::fs::write(path, bytes).map_err(|e| format!("writing {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::emit_object_to_memory;
    use newbf_ir::{BinOp, FunctionBuilder, IrType, Module as IrModule, Param};

    fn add_module() -> IrModule {
        let mut f = FunctionBuilder::new(
            "add",
            vec![
                Param {
                    name: Some("a".into()),
                    ty: IrType::I64,
                },
                Param {
                    name: Some("b".into()),
                    ty: IrType::I64,
                },
            ],
            IrType::I64,
        );
        let (a, b) = (f.param(0), f.param(1));
        let s = f.bin(BinOp::Add, a, b, IrType::I64);
        f.ret(Some(s));
        let mut m = IrModule::new("aot_add");
        m.add_function(f.finish());
        m
    }

    #[test]
    fn emits_a_native_object() {
        let obj = emit_object_to_memory(&add_module()).expect("object emits");
        assert!(!obj.is_empty(), "empty object");
        // On x64 Windows the container is COFF; its first field (Machine) is
        // IMAGE_FILE_MACHINE_AMD64 = 0x8664, little-endian → bytes 64 86.
        #[cfg(all(windows, target_arch = "x86_64"))]
        assert_eq!(&obj[..2], &[0x64, 0x86], "not an x64 COFF object");
    }
}
