//! `newbf-comptime` — the NewBF compile-time execution engine.
//!
//! **Comptime runs on the JIT, not an interpreter** — a deliberate reversal
//! of Beef, whose `CeMachine` is a bytecode VM. A `[Comptime]`/`const` method
//! lowers through the *same* `newbf-ir → newbf-llvm → ORC` pipeline as
//! application code (the IR is environment-agnostic by design) and is *called
//! during compilation*; its result folds back into the def graph. Invoked
//! re-entrantly from `newbf-sema` via a callback, so sema needn't depend on
//! the backend.
//!
//! Because it runs *native user code inside the compiler*, comptime needs the
//! crash machinery more than the application does: a comptime fault must
//! become a diagnostic, not a dead compiler. Safety = the Win64 SEH boundary
//! (`newbf-runtime::crash_dump`) + bounded execution (step/time/mem) around
//! each call — those arrive with the breadth work.
//!
//! **Sprint 08 = this tracer bullet:** JIT-evaluate a nullary `const`
//! function and read its value back, proving the loop end-to-end on the real
//! `OrcJit`. Breadth — the reflection/emit FFI table, the fixpoint worklist
//! (comptime can emit types that need resolving and trigger more comptime),
//! arg/result marshalling (incl. source-emit-and-reparse for generated type
//! bodies), and JITDylib + `ResourceTracker` world management — lands in
//! Sprints 19–21. Reference: `E:\beef\IDEHelper\Compiler\CeMachine.cpp`.

mod eval;
mod fold;

pub use eval::eval_const_i64;
pub use fold::fold_comptime;
