//! `newbf-llvm` — the NewBF backend: LLVM lowering, JIT, and AOT.
//!
//! Lowers `newbf-ir` to LLVM IR and then to machine code along two
//! paths that share everything up to the final step:
//!
//!   - **JIT** — for the REPL, hot code swapping, and fast iteration.
//!   - **AOT** — emit an object file and link a standalone native
//!     executable (a first-class v1 target, not deferred).
//!
//! Per-method opt levels (mixed optimization) are realized through
//! per-function LLVM pass pipelines. Emits LLVM IR, mixed-opt, and asm
//! reports.
//!
//! Sprint 07: IR→LLVM lowering + the `dump-llvm` report (this file's
//! [`emit_module`]). The JIT memory manager + Win64 SEH registration are
//! lifted from NewM2's `newm2-llvm` when the ORC JIT spike lands.

mod aot;
mod jit;
mod jit_mm;
mod lower;
mod mapsym;

pub use aot::{emit_object, emit_object_to_memory, link_executable};
pub use jit::OrcJit;
pub use lower::{emit_module, lower_to_string, verify_module};
pub use mapsym::symbolicate;

// Guard lifecycle re-export (memory-safety.md §A5/§A4, MS-T3). The guard's
// MODE/ledger atomics live in the `newbf-runtime` instance linked into the
// HOST process (driver, run-corpus harness, comptime). Those hosts already
// depend on `newbf-llvm`; re-exporting the lifecycle here lets them flip the
// guard mode, reset the ledger between programs, and read the live-count
// WITHOUT taking a new direct Cargo dep on `newbf-runtime` (the JIT'd Beef
// code's `newbf_alloc`/`newbf_free` resolve via the MS-T0 absolute-symbol
// seam, so the host only needs the *lifecycle* — set the mode, reset, report).
pub use newbf_runtime::{
    GuardMode, LeakReport, install_crash_handler, live_count, report_leaks, reset as guard_reset,
    set_guard_mode,
};
