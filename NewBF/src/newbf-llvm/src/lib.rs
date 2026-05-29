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
//! Lands in SPRINTS.md Sprint 07. LLVM is pinned to the portfolio major
//! (22.1) but the dependency is inactive until then. The JIT memory
//! manager + Win64 SEH registration are lifted from NewM2's `newm2-llvm`.
