//! `newbf-runtime` — the NewBF runtime. **Manual memory, no GC.**
//!
//! This is the inverse of the rest of the portfolio's tracing-GC bet,
//! and it is Beef's signature. The runtime provides:
//!
//!   - **An explicit heap** — `new`/`delete`, `scope` (scope-bound)
//!     allocations, allocator-qualified alloc/free, and append
//!     allocations, with first-class custom allocators.
//!   - **The debug memory guard** (on by default in debug, stripped in
//!     release):
//!       * **stomp allocator** — guard-page-per-allocation, unmap on
//!         free, so use-after-free / overrun faults deterministically at
//!         the offending access (port of `E:\beef\BeefRT\rt\
//!         StompAlloc.cpp`);
//!       * **leak ledger** — per-allocation site tracking + a real-time
//!         leak report;
//!       * **double-free guard** — freed objects are marked and a second
//!         delete is caught.
//!   - **Reflection metadata** emission/lookup, and the **FFI machinery**
//!     (calling-convention dispatcher, callback bridge, buffer
//!     marshalling) lifted from NewCormanLisp.
//!
//! The only OS dependency is a virtual-memory + threads shim (the stomp
//! allocator needs guard pages). Release builds fall through to a fast
//! allocator. Beef's optional conservative GC (`corlib GC.bf`) is a later,
//! opt-in mode — never the default.
//!
//! Lands in SPRINTS.md Sprints 09–11. Reference: `E:\beef\BeefRT\rt\`.
//!
//! **Live now:** the signal-safe Win64 crash-dump handler ([`crash_dump`]) —
//! the SEH consumer for `trap`/`debugtrap` and any fault, so a
//! crash-under-development prints a dump instead of dying silently. Its
//! memory-guard section is a hook the stomp allocator fills in Sprints 09–11.

mod crash_dump;

#[cfg(windows)]
pub use crash_dump::ensure_stack_overflow_reserve_this_thread;
pub use crash_dump::{
    install_crash_handler, newbf_install_crash_handler, note_free, note_memory_guard_installed,
    update_guard_metrics,
};

// ──────────────────────────────────────────────────────────────────── //
// Alloc-path C-ABI thunks                                              //
// ──────────────────────────────────────────────────────────────────── //
//
// The two stable C-ABI symbols the alloc path will route through
// (`new`/`delete` → `newbf_alloc`/`newbf_free`, memory-safety.md §4/§5).
// JIT'd and AOT'd Beef code calls these by name; the JIT resolves them as
// **ORC absolute symbols** (the host-EXE Rust addresses), registered in
// `OrcJit::from_ir` — see `newbf-llvm/src/jit.rs` and memory-safety.md §A0.
//
// For MS-T0 these are plain `malloc`/`free` wrappers — purely a *resolution
// seam*. The real stomp allocator / ledger (MS-T1) replaces the bodies
// without changing these signatures (the ABI is the contract).

unsafe extern "C" {
    fn malloc(size: usize) -> *mut u8;
    fn free(ptr: *mut u8);
}

/// Allocate `size` bytes. `type_id` / `site_id` are guard metadata
/// (`StructId.0` for objects, `-1` for arrays/raw; the alloc-site index) —
/// accepted and ignored by this MS-T0 thunk, consumed by the stomp
/// allocator in MS-T1. Returns a front-aligned base pointer (malloc-like),
/// or null on failure. A `size` of 0 is forwarded to `malloc` (which may
/// return null or a unique pointer — the stomp allocator handles the
/// size-0 / page-multiple edge cases in MS-T1).
///
/// # Safety
/// C-ABI export; the returned pointer must eventually be passed to
/// [`newbf_free`]. Callers must not read/write past `size` bytes.
#[unsafe(no_mangle)]
pub extern "C" fn newbf_alloc(size: i64, _type_id: i32, _site_id: i32) -> *mut u8 {
    if size < 0 {
        return core::ptr::null_mut();
    }
    // SAFETY: `malloc` is the CRT allocator; `size as usize` is non-negative.
    unsafe { malloc(size as usize) }
}

/// Free a pointer previously returned by [`newbf_alloc`]. A null pointer is
/// a no-op (matching `free(NULL)`). The ledger (MS-T1) will map the user
/// pointer to its real allocation base; this thunk forwards directly.
///
/// # Safety
/// C-ABI export; `ptr` must be a pointer returned by [`newbf_alloc`] and not
/// already freed (double-free is UB in this thunk — MS-T1's guard detects it).
#[unsafe(no_mangle)]
pub extern "C" fn newbf_free(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: `ptr` is non-null and (per the C-ABI contract) came from
    // `newbf_alloc`'s `malloc`.
    unsafe { free(ptr) }
}
