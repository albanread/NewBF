# NewBF IR

`newbf-ir` is a typed, SSA-shaped mid-level IR. Its shape mirrors Beef
IR's two-layer design (`E:\beef\IDEHelper\Compiler\BfIRBuilder.cpp`) —
backend-independent in form — but NewBF lowers it only to LLVM (the "Be"
x86 backend is dropped).

Each value carries its resolved type; each method carries an opt-level
attribute so mixed optimization (per-type/per-method opt levels) survives
into the LLVM per-function pass pipelines. The IR is the seam between
`newbf-sema` and `newbf-llvm`, and it is dumpable via `dump-ir`.

Stub — expanded at SPRINTS.md Sprint 06.
