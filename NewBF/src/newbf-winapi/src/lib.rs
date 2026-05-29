//! `newbf-winapi` — vendored Win32 API metadata for the FFI surface.
//!
//! The API *surface* (constants and function signatures) is vendored from
//! the shared `E:\windows_api` repository — a SQLite `windows_api.db` and
//! zstd `.pack` derived from Win32 metadata. At the winapi sprint a
//! `build.rs` reads a `data/windows_api.db` snapshot and emits a
//! postcard+zstd blob with a runtime lookup, exactly as NewOpenDylan's
//! `nod-winapi` does. The repository is consumed read-only.
//!
//! This crate is the API surface; the calling *machinery* (the
//! calling-convention dispatcher, callback bridge, buffer marshalling)
//! lives in `newbf-runtime`, lifted from NewCormanLisp.
//!
//! Lands in SPRINTS.md Sprint 27.
