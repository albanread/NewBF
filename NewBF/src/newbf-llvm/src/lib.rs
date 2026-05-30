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
