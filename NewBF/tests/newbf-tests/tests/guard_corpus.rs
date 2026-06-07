//! `guard_corpus` — the child-process memory-guard harness (MS-T3, the marquee
//! deliverable; memory-safety.md R4 / §8 "Track A — guard_corpus").
//!
//! ## Why a child process (R4)
//! A memory fault or the guard's deliberate `abort()` *terminates the process*:
//! the SEH filter returns `EXCEPTION_CONTINUE_SEARCH`, so a UAF propagates to
//! WER and kills the runner (memory-safety.md §2, §7). The in-process,
//! value-checking run-corpus therefore CANNOT observe a UAF or a double-free —
//! it would die with the program. The ONLY way to *observe* the guard is to run
//! each program in a spawned child (`guard_runner`, the binary in this package)
//! in Stomp mode with the crash handler installed, and have THIS parent inspect
//! the child's exit code / crash status.
//!
//! ## Exit-code classification (observed empirically on Windows x64)
//! `std::process::ExitStatus::code()` returns the raw process exit status:
//!   * **clean**           → `0`.
//!   * **ACCESS_VIOLATION** → `0xC0000005` as i32 = `-1073741819` (the page is
//!                            decommitted + quarantined; the post-delete load
//!                            faults at the offending instruction).
//!   * **guard abort**      → `std::process::abort()` on Windows fail-fasts via
//!                            `__fastfail`, exiting `0xC0000409`
//!                            (`STATUS_STACK_BUFFER_OVERRUN`) as i32 =
//!                            `-1073740791`. (NOT the C `abort()` code 3 / 134 —
//!                            Rust's abort is a fail-fast on this target; we
//!                            match the observed value.)
//!   * **leak**             → the `leakcheck` runner exits `LEAK_EXIT` (42) when
//!                            `live_count() != 0` after a balanced run.
//!
//! These three behaviors — UAF faults, double-free aborts, balanced run leaves
//! the ledger at zero — ARE the MS first-slice milestone proof.

#![cfg(all(windows, target_arch = "x86_64"))]

use std::path::PathBuf;
use std::process::Command;

/// `STATUS_ACCESS_VIOLATION` (0xC0000005) as the i32 a faulting child reports.
const ACCESS_VIOLATION: i32 = 0xC000_0005_u32 as i32; // -1073741819
/// `STATUS_STACK_BUFFER_OVERRUN` (0xC0000409) — the status `std::process::abort`
/// fail-fasts with on Windows x64; the guard's double/wild-free abort path.
const GUARD_ABORT: i32 = 0xC000_0409_u32 as i32; // -1073740791
/// Sentinel `guard_runner` exit for "ran clean but the ledger had live entries".
const LEAK_EXIT: i32 = 42;

fn guard_corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../beef-tests/guard-corpus")
}

/// Spawn `guard_runner <mode> <file>` and return its exit code. Cargo sets
/// `CARGO_BIN_EXE_guard_runner` for this crate's own binary, so we drive the
/// freshly-built runner without hard-coding a target path.
fn run_child(mode: &str, file: &str) -> i32 {
    run_child_captured(mode, file).0
}

/// As [`run_child`], but also captures the child's stderr (where the guard's
/// double-free abort + the leak report write the MS-T7 named site). Returns
/// `(exit_code, stderr)`.
fn run_child_captured(mode: &str, file: &str) -> (i32, String) {
    let runner = env!("CARGO_BIN_EXE_guard_runner");
    let path = guard_corpus_dir().join(file);
    let out = Command::new(runner)
        .arg(mode)
        .arg(&path)
        .output()
        .unwrap_or_else(|e| panic!("spawn {runner} {mode} {}: {e}", path.display()));
    let code = out
        .status
        .code()
        .unwrap_or_else(|| panic!("{file}: child terminated without an exit code: {:?}", out.status));
    (code, String::from_utf8_lossy(&out.stderr).into_owned())
}

/// `uaf_after_delete.bf` — read a field after `delete`. The quarantined page
/// faults: the child must exit with ACCESS_VIOLATION. THIS is the observable
/// UAF the in-process harness cannot catch.
#[test]
fn uaf_after_delete_faults_with_access_violation() {
    let code = run_child("run", "uaf_after_delete.bf");
    assert_eq!(
        code, ACCESS_VIOLATION,
        "uaf_after_delete: expected ACCESS_VIOLATION ({ACCESS_VIOLATION:#010x} = {ACCESS_VIOLATION}), \
         got {code} ({:#010x})",
        code as u32
    );
}

/// `double_free.bf` — `delete` twice. The ledger tombstone is hit on the second
/// free → the guard aborts. The child must exit with the guard-abort status
/// (NOT a clean 0, NOT an access violation).
#[test]
fn double_free_aborts_via_guard() {
    let code = run_child("run", "double_free.bf");
    assert_eq!(
        code, GUARD_ABORT,
        "double_free: expected guard abort ({GUARD_ABORT:#010x} = {GUARD_ABORT}), \
         got {code} ({:#010x})",
        code as u32
    );
    // Make absolutely sure we did not silently succeed or merely fault.
    assert_ne!(code, 0, "double_free must not exit cleanly");
    assert_ne!(code, ACCESS_VIOLATION, "double_free is an abort, not a fault");
}

/// **MS-T7 PROOF** — the double-free abort report NAMES the offending `new`'s
/// site as `<function> @ file:line`, not just an address. `double_free.bf` does
/// `Node p = new Node();` on line 12 inside `Program.Main`, so the guard's abort
/// message (written to the child's stderr) must contain `Program.Main`, the
/// source file name, and `:12`. This is the site-table → registration →
/// resolution round-trip working end-to-end (sema records the site, the backend
/// emits `__newbf_alloc_sites`, the runner registers it, the guard resolves the
/// tombstoned alloc's `site_id`).
#[test]
fn double_free_report_names_the_alloc_site() {
    let (code, stderr) = run_child_captured("run", "double_free.bf");
    assert_eq!(
        code, GUARD_ABORT,
        "double_free: expected guard abort, got {code} ({:#010x}); stderr:\n{stderr}",
        code as u32
    );
    assert!(
        stderr.contains("double free"),
        "expected a double-free abort line; stderr:\n{stderr}"
    );
    // The named site: function, file, and the `new`'s line (12).
    assert!(
        stderr.contains("Program.Main"),
        "MS-T7: abort report must name the enclosing function `Program.Main`; \
         stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("double_free.bf:12"),
        "MS-T7: abort report must name the offending `new`'s file:line \
         (double_free.bf:12); stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("Program.Main @ ") && stderr.contains(":12"),
        "MS-T7: abort report must read `<function> @ file:line`; stderr:\n{stderr}"
    );
}

/// `no_leak_balanced.bf` — balanced `new`/`delete`. The child exits cleanly AND
/// the ledger has zero live entries (the `leakcheck` runner exits 0 iff
/// `live_count() == 0`). Proves the no-false-leak side of the guard.
#[test]
fn no_leak_balanced_exits_clean_with_zero_live() {
    let code = run_child("leakcheck", "no_leak_balanced.bf");
    assert_eq!(
        code, 0,
        "no_leak_balanced: expected clean exit with ledger==0, got {code} \
         ({})",
        if code == LEAK_EXIT {
            "LEAK_EXIT — the ledger still had live allocations"
        } else {
            "unexpected exit code"
        }
    );
}
