//! `newbf-comptime` — the NewBF compile-time execution engine.
//!
//! A genuine interpreter (the one part of the system that is not the
//! JIT) that runs `[Comptime]` methods, const-evaluates expressions, and
//! generates types/members during compilation. Invoked re-entrantly from
//! `newbf-sema` via a callback to avoid a circular crate dependency.
//! Emits a comptime evaluation trace report.
//!
//! Lands in SPRINTS.md Sprints 19–21. Reference: `E:\beef\IDEHelper\
//! Compiler\CeMachine.cpp` and `CeDebugger.cpp`.
