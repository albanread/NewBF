//! `aot_parity` — JIT-vs-AOT parity for the live memory guard (MS-T3b, the
//! final wave-2 task; memory-safety.md §8 "Track A — JIT vs AOT parity", §A7).
//!
//! MS-T3a made the guard live in the **JIT** host (`guard_corpus` proves
//! uaf→AV, double_free→abort, no_leak→clean via the child-process runner).
//! MS-T3b makes it live in **AOT** too: `aot.rs::link_executable` links the
//! `newbf-runtime` staticlib, and `emit_module_aot` plants a `.CRT$XCU` guard
//! bootstrap (install crash handler + set guard mode + register the alloc-site
//! table) that runs BEFORE the program's `main`. This harness compiles two
//! guard-corpus programs to **real AOT executables** and inspects their exit
//! codes — the AOT analog of the JIT `guard_corpus` gate.
//!
//! ## Why child processes (same R4 reason as guard_corpus)
//! The guard's deliberate `abort()` (and any fault) *terminates the process*.
//! Here the program IS a separate spawned `.exe`, so its abort kills the child,
//! not this test — and we read the child's exit status.
//!
//! ## Exit-code classification (Windows x64, matching guard_corpus.rs)
//!   * **clean**       → the program's own `i32` return value (a normal exit).
//!   * **guard abort** → `std::process::abort()` fail-fasts via `__fastfail`,
//!                       exiting `0xC0000409` (`STATUS_STACK_BUFFER_OVERRUN`).
//!
//! ## What this proves
//!   * **debug double_free AOT → aborts** (non-zero abort status): the linked
//!     runtime's ledger + the `.CRT$XCU` bootstrap that set Stomp mode are LIVE
//!     in the shipped binary — a double-free hits the tombstone and aborts.
//!   * **no_leak_balanced AOT → clean exit** (the program's return value, not a
//!     crash): the runtime staticlib link + bootstrap do NOT break a correct
//!     program (CRT init preserved; balanced new/delete runs to completion).

#![cfg(all(windows, target_arch = "x86_64"))]

use std::path::PathBuf;
use std::process::Command;

use newbf_lexer::FileId;
use newbf_llvm::{emit_object, link_executable};
use newbf_parser::parse_file;
use newbf_sema::SourceFile;

/// `STATUS_STACK_BUFFER_OVERRUN` (0xC0000409) — the status `std::process::abort`
/// fail-fasts with on Windows x64; the guard's double/wild-free abort path.
/// (Same constant the JIT `guard_corpus` harness asserts.)
const GUARD_ABORT: i32 = 0xC000_0409_u32 as i32; // -1073740791
/// `STATUS_ACCESS_VIOLATION` (0xC0000005) — a faulting child (not expected for
/// these two programs, used only to give a clearer assertion message).
const ACCESS_VIOLATION: i32 = 0xC000_0005_u32 as i32; // -1073741819

fn guard_corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../beef-tests/guard-corpus")
}

/// Compile `file` (a guard-corpus `.bf`) to a native AOT `.exe` via the real
/// `emit_object` + `link_executable` path (the runtime staticlib + the
/// `.CRT$XCU` guard bootstrap come in here), run it as a child, and return its
/// exit code. Mirrors how the `newbf-llvm` AOT tests build+run an exe, but on a
/// real `.bf` driven through the full parse → emit → AOT pipeline.
fn compile_and_run_aot(file: &str) -> i32 {
    let path = guard_corpus_dir().join(file);
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));

    let (unit, pdiags) = parse_file(&src, FileId(0));
    assert!(pdiags.is_empty(), "{file}: parse diagnostics: {pdiags:?}");
    let files = [SourceFile {
        file: FileId(0),
        src: &src,
        unit: &unit,
        name: file,
    }];
    // Same front of the pipeline as the JIT harness; the generator-free guard
    // programs take the no-op emission fast path.
    let (mut module, _emit) =
        newbf_comptime::run_emission(&files).expect("comptime emission succeeds");
    // The lowered entry is `Program.Main`, but the CRT's `mainCRTStartup` calls a
    // C `main`. Add the same thin `i32 main()` stub forwarding to `*.Main` that
    // the driver's `compile` command emits, so the exe links + its exit code is
    // `Main`'s return value.
    add_main_stub(&mut module);

    // Per-process-unique temp paths so parallel test binaries don't clash.
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let stem = file.replace('.', "_");
    let obj = dir.join(format!("newbf_aotpar_{pid}_{stem}.obj"));
    let exe = dir.join(format!("newbf_aotpar_{pid}_{stem}.exe"));

    emit_object(&module, &obj).expect("emit object");
    link_executable(&[&obj], &exe, &[]).expect("link exe");

    let status = Command::new(&exe)
        .status()
        .unwrap_or_else(|e| panic!("{file}: run {}: {e}", exe.display()));
    let code = status
        .code()
        .unwrap_or_else(|| panic!("{file}: child terminated without an exit code: {status:?}"));

    let _ = std::fs::remove_file(&obj);
    let _ = std::fs::remove_file(&exe);
    let _ = std::fs::remove_file(exe.with_extension("exe.map"));
    code
}

/// Emit a C `i32 main()` entry stub forwarding to the Beef entry point (a
/// lowered `*.Main` function) so the linked exe has the CRT-expected entry and
/// its exit code is `Main`'s return value. A copy of the driver `compile`
/// command's `add_main_stub` (newbf-driver/src/main.rs) — the parity test drives
/// the same AOT path the driver does.
fn add_main_stub(module: &mut newbf_ir::Module) {
    use newbf_ir::{FunctionBuilder, IrType, Value};

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

/// **Debug AOT parity (Stomp).** `double_free.bf` compiled AOT → the guard
/// bootstrap (debug compiler ⇒ Stomp mode) is live in the binary, so the second
/// `delete` hits the ledger tombstone and the guard `abort()`s. The exe must
/// exit with the guard-abort status, NOT a clean 0.
///
/// (Debug-only: a release build of the compiler ships Thunk mode — plain
/// malloc/free — where a double-free does not abort. The wave gate is the live
/// guard, which a debug toolchain provides.)
#[test]
#[cfg(debug_assertions)]
fn double_free_aot_aborts() {
    let code = compile_and_run_aot("double_free.bf");
    assert_eq!(
        code, GUARD_ABORT,
        "double_free AOT: expected guard abort ({GUARD_ABORT:#010x} = {GUARD_ABORT}), \
         got {code} ({:#010x}). The linked runtime staticlib + the .CRT$XCU Stomp \
         bootstrap must make a double-free abort in the shipped binary.",
        code as u32
    );
    assert_ne!(code, 0, "double_free AOT must not exit cleanly");
    assert_ne!(
        code, ACCESS_VIOLATION,
        "double_free AOT is a deliberate guard abort, not a fault"
    );
}

/// **Clean AOT parity.** `no_leak_balanced.bf` compiled AOT → a balanced
/// new/delete runs to completion and the exe exits cleanly with the program's
/// own return value (`r = p.value = 7`), NOT a crash/abort. This proves the
/// runtime staticlib link + the `.CRT$XCU` bootstrap (and thus the preserved CRT
/// init) do not break a correct program — in Stomp (debug) or Thunk (release).
#[test]
fn no_leak_balanced_aot_exits_clean() {
    let code = compile_and_run_aot("no_leak_balanced.bf");
    // The program returns `r` = the node's `value` field (7). A clean exit means
    // the exit code equals that return value — definitively not a fault/abort.
    assert_eq!(
        code, 7,
        "no_leak_balanced AOT: expected a clean exit with the program's return \
         value (7), got {code} ({:#010x}). The runtime link + guard bootstrap must \
         not break a correct program (CRT init preserved).",
        code as u32
    );
    assert_ne!(code, GUARD_ABORT, "no_leak_balanced must not abort");
    assert_ne!(code, ACCESS_VIOLATION, "no_leak_balanced must not fault");
}
