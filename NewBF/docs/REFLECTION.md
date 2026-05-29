# Reflection

Beef emits runtime type metadata (`Type`, `System.Reflection`) under a
**strip policy** — `[Reflect]`, always-include, or strip — so release
builds pay only for the reflection they use.

In NewBF, reflection metadata is emitted by `newbf-llvm`/`newbf-runtime`
and the *set of metadata a build emits* is itself a human-reviewable phase
report (so a reviewer can see exactly what reflection a release build
carries). Reflection underpins dynamic dispatch, serialization, and the
IDE's type inspector.

Stub — expanded at SPRINTS.md Sprints 22–23.
