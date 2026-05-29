//! `newbf-sema` — the NewBF semantic core.
//!
//! Builds definitions (types, methods, fields, namespaces), then does
//! name + type resolution, generic instantiation, dispatch resolution,
//! definite-assignment, and the manual-memory delete-flow (ownership)
//! checks. Calls into `newbf-comptime` for compile-time evaluation, and
//! emits the defs/types/dispatch/generic reports.
//!
//! This is the bulk of the compiler. Lands across SPRINTS.md Sprints
//! 05, 12–18. Reference: `E:\beef\IDEHelper\Compiler\BfModule.cpp`,
//! `BfExprEvaluator.cpp`, `BfModuleTypeUtils.cpp`, `BfDefBuilder.cpp`,
//! `BfSystem.cpp`.
