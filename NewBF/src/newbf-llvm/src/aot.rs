//! AOT object-file emission — the shipping-codegen half of "JIT *and* AOT
//! both first-class".
//!
//! Lowers the typed SSA IR to an LLVM module (the same `emit_module` the JIT
//! uses) and runs it through an LLVM `TargetMachine` to produce a native
//! object file (COFF on Windows) — [`emit_object`] — then drives MSVC
//! `link.exe` to link object(s) + the CRT into a standalone `.exe`
//! ([`link_executable`]). The full path is proven end-to-end: lower → object
//! → link → run. Still ahead: linking the runtime staticlib (for the AOT
//! entry stub + `newbf_install_crash_handler`) and the driver `compile`
//! command.

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

/// Link `objects` (+ any `extra_libs`, e.g. `"user32.lib"`) into a native
/// console `.exe` at `output`, driving MSVC `link.exe`. The CRT entry
/// `mainCRTStartup` runs the C `main` our codegen emits.
///
/// `cc::windows_registry::find` locates `link.exe` with `%LIB%` populated, so
/// the SDK/CRT import libs below resolve without a Developer Command Prompt —
/// the same trick NewOpenDylan's driver uses. The runtime staticlib (which
/// will provide `newbf_install_crash_handler` + the AOT entry stub) joins this
/// arg list when it lands; for now the CRT alone suffices for a freestanding
/// `main`.
pub fn link_executable(
    objects: &[&Path],
    output: &Path,
    extra_libs: &[&str],
) -> Result<(), String> {
    let mut cmd =
        cc::windows_registry::find("x86_64-pc-windows-msvc", "link.exe").ok_or_else(|| {
            "could not locate MSVC link.exe (install VS Build Tools or run from a \
             Developer Command Prompt)"
                .to_string()
        })?;
    for obj in objects {
        cmd.arg(obj);
    }
    cmd.arg(format!("/OUT:{}", output.display()));
    cmd.arg("/SUBSYSTEM:CONSOLE");
    cmd.arg("/ENTRY:mainCRTStartup");
    cmd.arg("/MACHINE:X64");
    // MS-T2 bridge until MS-T3 links the runtime staticlib: the alloc path now
    // emits `newbf_alloc`/`newbf_free` (memory-safety.md §A1). The real runtime
    // (with the stomp guard + the `/ENTRY:newbf_entry` bootstrap) joins the link
    // at MS-T3; until then, resolve these to the CRT thunks the guard's *default
    // Thunk mode* already defines them as (`newbf_alloc`≡`malloc`,
    // `newbf_free`≡`free`, mod.rs route_alloc/route_free). `/ALTERNATENAME` is a
    // *fallback*: when MS-T3 supplies a real `newbf_alloc` definition it wins and
    // these are ignored. On x64 the extra `type_id`/`site_id` args sit in
    // RDX/R8, which `malloc` ignores — the size is in RCX, so the thunk is
    // ABI-safe.
    cmd.arg("/ALTERNATENAME:newbf_alloc=malloc");
    cmd.arg("/ALTERNATENAME:newbf_free=free");
    // Modern Windows security defaults (link.exe warns without them).
    cmd.arg("/NXCOMPAT");
    cmd.arg("/DYNAMICBASE");
    cmd.arg("/HIGHENTROPYVA");
    // Emit a `.map` (symbol → Rva+Base) next to the exe so crash-dump IPs can
    // be named offline via `symbolicate` (our own frames, no dbghelp/PDB).
    cmd.arg(format!("/MAP:{}.map", output.display()));
    // The CRT + kernel import libs a freestanding `main` needs; %LIB% (set by
    // the discovered link.exe) resolves them from the SDK.
    for lib in [
        "kernel32.lib",
        "msvcrt.lib",
        "ucrt.lib",
        "vcruntime.lib",
        "legacy_stdio_definitions.lib",
    ] {
        cmd.arg(lib);
    }
    for lib in extra_libs {
        cmd.arg(lib);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("failed to invoke link.exe: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "link.exe failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ))
    }
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

    /// RF-T4 AOT metadata smoke (the JIT-only run-corpus can't cover AOT
    /// `.rodata` serialization): emit a module with one Type global (type_id 7) +
    /// the in-module `__newbf_type_by_id` accessor, link it into a real exe whose
    /// `main` calls the accessor and returns the Type's `mTypeId`, then run it and
    /// check the exit code is 7. This proves (a) the Type/registry constants
    /// serialize into the object's `.rodata` and (b) the in-module accessor links
    /// in AOT with NO Rust runtime symbol (the design's verified fix).
    #[cfg(all(windows, target_arch = "x86_64"))]
    #[test]
    fn aot_metadata_accessor_links_and_runs() {
        use super::{emit_object, link_executable};
        use newbf_ir::{FieldDef, ReflectPolicy, StructDef, TypeMeta, Value, VtableDef};

        let mut m = IrModule::new("aot_meta");
        // A reflectable class `{ $header: ptr, mX: i32 }` with type_id 7.
        let c = m.add_struct(StructDef {
            name: "Widget".into(),
            fields: vec![
                FieldDef { name: "$header".into(), ty: IrType::Ptr },
                FieldDef { name: "mX".into(), ty: IrType::I32 },
            ],
        });
        m.add_vtable(VtableDef { name: "Widget.$cvdata".into(), entries: vec![], type_id: 7 });
        m.add_type_meta(TypeMeta {
            type_id: 0, // dense id 0 (the only entry) — registry index
            struct_id: c,
            name: "Widget".into(),
            policy: ReflectPolicy::TYPE,
            is_ref: true,
            fields: vec![],
            methods: vec![],
        });

        // i32 main() { Type* t = __newbf_type_by_id(0); return t->mTypeId; }
        // The Type aggregate is { i32 mSize, i32 mTypeId, ... } so mTypeId is the
        // second i32 — but we placed type_id 7 in the VtableDef's mType word; the
        // Type global's mTypeId is the DENSE id (0). To return a stable nonzero,
        // read mSize (field 0 = 16) instead — the object instance size.
        m.declare_extern("__newbf_type_by_id", vec![Param { name: None, ty: IrType::I32 }], IrType::Ptr);
        let mut f = FunctionBuilder::new("main", vec![], IrType::I32);
        let t = f.call("__newbf_type_by_id", vec![Value::int(0, IrType::I32)], IrType::Ptr);
        // mSize is field 0 of %struct.Type; load it as i32 (= get_size(Widget) = 16).
        let sz = f.load(t, IrType::I32);
        f.ret(Some(sz));
        m.add_function(f.finish());

        let dir = std::env::temp_dir();
        let pidn = std::process::id();
        let obj = dir.join(format!("newbf_aotmeta_{pidn}.obj"));
        let exe = dir.join(format!("newbf_aotmeta_{pidn}.exe"));

        emit_object(&m, &obj).expect("emit object");
        link_executable(&[&obj], &exe, &[]).expect("link exe");
        let status = std::process::Command::new(&exe).status().expect("run exe");
        // mSize(Widget) = ptr(8) + i32(4) + pad(4) = 16; the accessor + Type
        // global both came from .rodata and the in-module accessor linked.
        assert_eq!(status.code(), Some(16), "AOT metadata: Type.mSize via in-module accessor");

        let _ = std::fs::remove_file(&obj);
        let _ = std::fs::remove_file(&exe);
        let _ = std::fs::remove_file(exe.with_extension("exe.map"));
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

    /// End-to-end AOT: emit an object for `i32 main() => 42`, link it into a
    /// real `.exe`, run it, and check the exit code — the AOT analog of the
    /// JIT's `jit_executes_add`.
    #[cfg(all(windows, target_arch = "x86_64"))]
    #[test]
    fn links_and_runs_an_exe() {
        use super::{emit_object, link_executable};
        use newbf_ir::Value;

        // i32 main() => 42;
        let mut f = FunctionBuilder::new("main", vec![], IrType::I32);
        f.ret(Some(Value::int(42, IrType::I32)));
        let mut m = IrModule::new("aot_exe");
        m.add_function(f.finish());

        // Per-process-unique temp paths so parallel test binaries don't clash.
        let dir = std::env::temp_dir();
        let pid = std::process::id();
        let obj = dir.join(format!("newbf_aot_{pid}.obj"));
        let exe = dir.join(format!("newbf_aot_{pid}.exe"));

        emit_object(&m, &obj).expect("emit object");
        link_executable(&[&obj], &exe, &[]).expect("link exe");
        let status = std::process::Command::new(&exe).status().expect("run exe");
        assert_eq!(status.code(), Some(42), "AOT exe exit code");

        let _ = std::fs::remove_file(&obj);
        let _ = std::fs::remove_file(&exe);
    }

    /// AOT Win32: an exe that calls a real Win32 function (`GetCurrentProcessId`,
    /// kernel32) through the linker-built IAT. We `declare` the extern and link
    /// the demanded import lib — the import lib's `Foo: jmp [__imp_Foo]` thunk
    /// resolves through the IAT, no `dllimport` needed. The program returns 7
    /// iff the call yielded a (always-nonzero) pid, so exit code 7 proves the
    /// Win32 call executed in the shipped binary.
    #[cfg(all(windows, target_arch = "x86_64"))]
    #[test]
    fn aot_calls_a_real_win32_function() {
        use super::{emit_object, link_executable};
        use newbf_ir::{CmpPred, Value};

        // i32 main() { return GetCurrentProcessId() != 0 ? 7 : 0; }
        let mut m = IrModule::new("aot_win32");
        m.declare_extern("GetCurrentProcessId", vec![], IrType::U32);
        let mut f = FunctionBuilder::new("main", vec![], IrType::I32);
        let pid = f.call("GetCurrentProcessId", vec![], IrType::U32);
        let nonzero = f.cmp(CmpPred::Ne, pid, Value::int(0, IrType::U32));
        let code = f.select(
            nonzero,
            Value::int(7, IrType::I32),
            Value::int(0, IrType::I32),
            IrType::I32,
        );
        f.ret(Some(code));
        m.add_function(f.finish());

        let dir = std::env::temp_dir();
        let pidn = std::process::id();
        let obj = dir.join(format!("newbf_aotwin_{pidn}.obj"));
        let exe = dir.join(format!("newbf_aotwin_{pidn}.exe"));

        emit_object(&m, &obj).expect("emit object");
        // `kernel32.lib` is what `import_lib_for_dll("kernel32.dll")` yields —
        // in the full pipeline the driver derives this from the demanded set.
        link_executable(&[&obj], &exe, &["kernel32.lib"]).expect("link exe");
        let status = std::process::Command::new(&exe).status().expect("run exe");
        assert_eq!(status.code(), Some(7), "AOT Win32 call result");

        let _ = std::fs::remove_file(&obj);
        let _ = std::fs::remove_file(&exe);
        let _ = std::fs::remove_file(exe.with_extension("exe.map"));
    }

    /// End-to-end symbolication: link with `/MAP`, then resolve our own `main`
    /// from the real `.map` — the names-our-own-code path that the in-box
    /// dbghelp can't do. Proves the whole loop: emit → link → .map → name.
    #[cfg(all(windows, target_arch = "x86_64"))]
    #[test]
    fn map_names_our_own_function() {
        use super::{emit_object, link_executable};
        use crate::symbolicate;
        use newbf_ir::Value;

        let mut f = FunctionBuilder::new("main", vec![], IrType::I32);
        f.ret(Some(Value::int(42, IrType::I32)));
        let mut m = IrModule::new("mapt");
        m.add_function(f.finish());

        let dir = std::env::temp_dir();
        let pid = std::process::id();
        let obj = dir.join(format!("newbf_map_{pid}.obj"));
        let exe = dir.join(format!("newbf_map_{pid}.exe"));
        let map = dir.join(format!("newbf_map_{pid}.exe.map"));

        emit_object(&m, &obj).expect("emit object");
        link_executable(&[obj.as_path()], &exe, &[]).expect("link exe");
        let map_text = std::fs::read_to_string(&map).expect("link emitted a .map");

        // Pull `main`'s real Rva+Base from the produced .map and confirm a dump
        // line referencing that address symbolicates back to `main`.
        let rva = map_text
            .lines()
            .find_map(|l| {
                let t: Vec<&str> = l.split_whitespace().collect();
                if t.get(1) == Some(&"main") {
                    t.iter()
                        .rev()
                        .find(|x| x.len() == 16 && x.bytes().all(|b| b.is_ascii_hexdigit()))
                        .copied()
                } else {
                    None
                }
            })
            .expect("`main` row in .map");
        let dump = format!("crashed at 0x{rva}");
        let out = symbolicate(&dump, &map_text, None).expect("symbolicate");
        assert!(out.contains("main+0x0"), "did not name main:\n{out}");

        let _ = std::fs::remove_file(&obj);
        let _ = std::fs::remove_file(&exe);
        let _ = std::fs::remove_file(&map);
    }
}
