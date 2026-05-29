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
