//! `newbf-tests` — Rust-side unit and integration tests.
//!
//! Drives the NewBF pipeline end-to-end and asserts on the per-phase
//! reports. Per the testing policy in PLAN.md, when a test validates the
//! compiler/JIT/runtime/AOT, the substantive workload is *Beef* source;
//! Rust only orchestrates and captures reports.
