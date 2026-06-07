//! `guard_runner` — the child process for the `guard_corpus` harness (MS-T3,
//! memory-safety.md R4 / §8).
//!
//! A fault or a deliberate guard `abort()` *kills the process* — the SEH filter
//! returns `EXCEPTION_CONTINUE_SEARCH` so a UAF propagates to WER and terminates
//! (memory-safety.md §2, §7 "Harness can't catch a fault in-process"). The
//! in-process value-checking run-corpus therefore CANNOT observe a UAF; this
//! tiny runner is spawned once per guard program so the parent
//! (`guard_corpus.rs`) can inspect the child's exit code / crash status.
//!
//! It JIT-compiles a single `.bf` file in **Stomp** mode with the crash handler
//! installed, then either:
//!   * `run <file.bf>`       — calls `Program.Main` and exits 0 (the value is
//!                             irrelevant; we care about fault/abort vs. clean).
//!                             A UAF faults (ACCESS_VIOLATION); a double-free
//!                             hits the ledger tombstone and `abort()`s.
//!   * `leakcheck <file.bf>` — calls `Program.Main`, then asserts the ledger is
//!                             balanced: exit 0 iff `live_count() == 0`, else
//!                             exit `LEAK_EXIT` so the parent sees the leak.
//!
//! The guard lifecycle (`install_crash_handler`/`set_guard_mode`/`live_count`/
//! `guard_reset`) comes through `newbf-llvm`'s re-export, so this runner needs
//! no direct `newbf-runtime` dep; the JIT'd Beef code's `newbf_alloc`/
//! `newbf_free` resolve via the MS-T0 absolute-symbol seam in `OrcJit::from_ir`.

use newbf_lexer::FileId;
use newbf_llvm::{GuardMode, OrcJit};
use newbf_parser::parse_file;
use newbf_sema::{SourceFile, analyze, lower_program};

/// Exit code the parent reads for "ran clean but the ledger still had live
/// allocations" (a leak). Distinct from a fault/abort and from a clean 0.
const LEAK_EXIT: i32 = 42;
/// Exit code for a usage / compile error in the runner itself (kept off the
/// fault/abort/leak codes so the parent never mis-classifies it).
const USAGE_EXIT: i32 = 3;

fn main() {
    // Arm the SEH crash dump first: a UAF in JIT'd code must produce a dump and
    // terminate (ACCESS_VIOLATION) rather than die silently.
    newbf_llvm::install_crash_handler();
    // The whole point of this runner: the quarantining stomp guard is ON, so a
    // post-delete read faults (decommitted page) and a double-free aborts.
    newbf_llvm::set_guard_mode(GuardMode::Stomp);

    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_default();
    let path = match args.next() {
        Some(p) => p,
        None => {
            eprintln!("usage: guard_runner <run|leakcheck> <file.bf>");
            std::process::exit(USAGE_EXIT);
        }
    };

    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("guard_runner: cannot read {path}: {e}");
            std::process::exit(USAGE_EXIT);
        }
    };

    let main = compile_to_main(&src);

    match mode.as_str() {
        "run" => {
            // Call Main. A UAF inside faults → ACCESS_VIOLATION kills us here.
            // A double-free hits the ledger tombstone → guard `abort()`.
            let _ = main();
            // Reached only on a clean run.
            std::process::exit(0);
        }
        "leakcheck" => {
            let _ = main();
            // Balanced new/delete ⇒ the ledger has zero live entries.
            let live = newbf_llvm::live_count();
            // Release VM ranges / clear the ledger (parity with the in-process
            // harness's between-program discipline; harmless here at exit).
            newbf_llvm::guard_reset();
            if live == 0 {
                std::process::exit(0);
            } else {
                eprintln!("guard_runner: {live} live allocation(s) after run (leak)");
                std::process::exit(LEAK_EXIT);
            }
        }
        other => {
            eprintln!("guard_runner: unknown mode {other:?}");
            std::process::exit(USAGE_EXIT);
        }
    }
}

/// Parse → analyze → lower → comptime-emit → JIT, returning a callable
/// `Program.Main` (a nullary `i32` fn). Mirrors `run_corpus.rs::run`.
fn compile_to_main(src: &str) -> extern "C" fn() -> i32 {
    let (unit, pdiags) = parse_file(src, FileId(0));
    assert!(pdiags.is_empty(), "parse diagnostics: {pdiags:?}");
    let files = [SourceFile {
        file: FileId(0),
        src,
        unit: &unit,
    }];
    let program = analyze(&files);
    let module = lower_program(&files, &program);
    let (module, _emit) =
        newbf_comptime::run_emission(module).expect("comptime emission succeeds");
    // Leak the JIT so the JIT'd code memory stays mapped for the whole process
    // lifetime (we never tear it down — the process exits right after the run).
    let jit = Box::leak(Box::new(
        OrcJit::from_ir(&module).expect("jit builds"),
    ));
    let addr = jit.lookup("Program.Main").expect("Program.Main resolves");
    // SAFETY: guard-corpus entries are `static int32 Main()` — nullary `i32`.
    unsafe { std::mem::transmute::<u64, extern "C" fn() -> i32>(addr) }
}
