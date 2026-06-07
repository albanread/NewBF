//! `newbf-corlib` — the NewBF standard library.
//!
//! Holds the Beef-side standard library as runnable `.bf` source (ported
//! from `E:\beef\BeefLibs\corlib\src\`), compiled by our own compiler —
//! *not* compiler bootstrap. The `System` namespace: `Object`,
//! `ValueType`, `Type`, `String`, the collections, `Math`, `Result`,
//! reflection, IO, threading, and the allocator interfaces.
//!
//! The `.bf` sources live under `bf/` (added during the corlib port,
//! SPRINTS.md Sprint 28+); this crate is the thin Rust shim that locates
//! and registers them with the driver.

/// The standard-library prelude: `(filename, source)` for each `bf/*.bf`,
/// embedded at compile time. The compiler prepends these (parsed) before the
/// user's program and lowers them together — composed at the AST, lowered once
/// (see `docs/STDLIB.md`). Order is dependency-respecting (lowest layer first).
pub fn prelude() -> &'static [(&'static str, &'static str)] {
    &[
        ("Internal.bf", include_str!("../bf/Internal.bf")),
        ("String.bf", include_str!("../bf/String.bf")),
        // System.Reflection.FieldInfo — a reflected field's metadata (RF-T6). A
        // value `struct` whose layout mirrors the emitted `%struct.FieldInfo`.
        // `Type.mFields` is `FieldInfo*`, so it must register BEFORE `Type.bf`.
        ("FieldInfo.bf", include_str!("../bf/FieldInfo.bf")),
        // System.Type — the reflection metatype (RF-T4). A value `struct` whose
        // layout mirrors the emitted `%struct.Type`; uses `char8*` + `FieldInfo`,
        // so it lands after Internal/String/FieldInfo and before any consumer.
        ("Type.bf", include_str!("../bf/Type.bf")),
        // System.Compiler — the comptime emission surface (comptime-breadth
        // §3.2, CB-T3). A `static class` whose `EmitTypeBody(text)` call sema
        // rewrites to the `__newbf_ct_emit` host shim; rides the prelude like
        // Type.bf (a duplicate corpus `Compiler` is skipped at registration).
        ("Compiler.bf", include_str!("../bf/Compiler.bf")),
        ("Console.bf", include_str!("../bf/Console.bf")),
        ("Pool.bf", include_str!("../bf/Pool.bf")),
        ("Handle.bf", include_str!("../bf/Handle.bf")),
        ("List.bf", include_str!("../bf/List.bf")),
        ("Probe.bf", include_str!("../bf/Probe.bf")),
        ("Option.bf", include_str!("../bf/Option.bf")),
        ("Math.bf", include_str!("../bf/Math.bf")),
    ]
}
