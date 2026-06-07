//! The "it prints" end-to-end test: compile a Beef program that calls
//! `puts("…")` to a native exe, run it, capture stdout, and assert it
//! printed. Exercises string-literal lowering (→ a constant global) + a CRT
//! call through the full AOT path (lower → object → entry stub → link → run).

#![cfg(all(windows, target_arch = "x86_64"))]

use newbf_ir::{FunctionBuilder, IrType, Module as IrModule, Value};
use newbf_lexer::FileId;
use newbf_parser::parse_file;
use newbf_sema::{SourceFile, analyze, lower_program};

/// Emit the C `main` entry stub forwarding to the Beef `*.Main` (mirrors the
/// driver's `compile`; kept local so the test doesn't depend on the binary).
fn add_main_stub(module: &mut IrModule) {
    let entry = module
        .funcs
        .iter()
        .find(|f| !f.is_extern && f.name.ends_with(".Main"))
        .map(|f| (f.name.clone(), f.ret));
    let mut f = FunctionBuilder::new("main", vec![], IrType::I32);
    let code = match entry {
        Some((name, ret)) => {
            let r = f.call(name, vec![], ret);
            if ret == IrType::I32 {
                r
            } else {
                Value::int(0, IrType::I32)
            }
        }
        None => Value::int(0, IrType::I32),
    };
    f.ret(Some(code));
    module.add_function(f.finish());
}

#[test]
fn compiles_a_hello_world_that_prints() {
    let src = "class Program {\n\
               \x20   public static int32 Main() {\n\
               \x20       puts(\"NEWBF_HELLO_42\");\n\
               \x20       return 0;\n\
               \x20   }\n\
               }";
    let (unit, pdiags) = parse_file(src, FileId(0));
    assert!(pdiags.is_empty(), "parse diagnostics: {pdiags:?}");
    let files = [SourceFile {
        file: FileId(0),
        src,
        unit: &unit,
        name: "",
    }];
    let program = analyze(&files);
    let mut module = lower_program(&files, &program);
    add_main_stub(&mut module);

    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let obj = dir.join(format!("newbf_hello_{pid}.obj"));
    let exe = dir.join(format!("newbf_hello_{pid}.exe"));

    newbf_llvm::emit_object(&module, &obj).expect("emit object");
    newbf_llvm::link_executable(&[obj.as_path()], &exe, &[]).expect("link exe");
    let out = std::process::Command::new(&exe).output().expect("run exe");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("NEWBF_HELLO_42"),
        "program did not print the marker; stdout = {stdout:?}, status = {:?}",
        out.status.code()
    );
    assert_eq!(out.status.code(), Some(0), "expected exit 0");

    let _ = std::fs::remove_file(&obj);
    let _ = std::fs::remove_file(&exe);
    let _ = std::fs::remove_file(exe.with_extension("exe.map"));
}
