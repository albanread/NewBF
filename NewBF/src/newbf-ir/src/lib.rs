//! `newbf-ir` — the NewBF mid-level IR.
//!
//! A typed, SSA-shaped intermediate representation, backend-independent
//! in shape (mirroring Beef IR's two-layer design) but lowered only to
//! LLVM. Each value carries its resolved type; each method carries its
//! opt-level attribute (for mixed optimization). Emits a `dump-ir`
//! report.
//!
//! Lands in SPRINTS.md Sprint 06. Reference: `E:\beef\IDEHelper\Compiler\
//! BfIRBuilder.cpp`.
