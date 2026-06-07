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
pub mod guard;

#[cfg(windows)]
pub use crash_dump::ensure_stack_overflow_reserve_this_thread;
pub use crash_dump::{
    install_crash_handler, newbf_install_crash_handler, note_free, note_memory_guard_installed,
    update_guard_metrics,
};

pub use guard::sites::{format_site, register_alloc_sites, AllocSiteRaw};
pub use guard::{GuardMode, LeakReport, live_count, report_leaks, reset, set_guard_mode};

// ──────────────────────────────────────────────────────────────────── //
// Alloc-path C-ABI: route through the guard mode                       //
// ──────────────────────────────────────────────────────────────────── //
//
// The two stable C-ABI symbols the alloc path routes through (`new`/`delete`
// → `newbf_alloc`/`newbf_free`, memory-safety.md §4/§5). JIT'd and AOT'd Beef
// code calls these by name; the JIT resolves them as **ORC absolute symbols**
// (the host-EXE Rust addresses), registered in `OrcJit::from_ir` — see
// `newbf-llvm/src/jit.rs` and memory-safety.md §A0.
//
// MS-T1: the bodies now route through `guard::route_*`, selected by a runtime
// mode flag (memory-safety.md §A5). The **DEFAULT mode is Thunk** (straight
// malloc/free passthrough), so un-guarded callers — MS-T0's smoke test and the
// run-corpus, which don't enable the guard until MS-T3 — keep working
// unchanged. The Stomp guard activates only on `newbf_set_guard_mode(Stomp)`.
// The C-ABI signatures are byte-identical to MS-T0 (the ABI is the contract
// MS-T2's call sites depend on).

/// Allocate `size` bytes. `type_id` / `site_id` are guard metadata
/// (`StructId.0` for objects, `-1` for arrays/raw; the alloc-site index),
/// consumed by the stomp allocator in `Stomp` mode and ignored in `Thunk`
/// mode. Returns a front-aligned base pointer, or null on failure / negative
/// size.
///
/// # Safety
/// C-ABI export; the returned pointer must eventually be passed to
/// [`newbf_free`] **in the same guard mode**. Callers must not read/write past
/// `size` bytes (in `Stomp` mode an overrun past the page faults).
#[unsafe(no_mangle)]
pub extern "C" fn newbf_alloc(size: i64, type_id: i32, site_id: i32) -> *mut u8 {
    if size < 0 {
        return core::ptr::null_mut();
    }
    guard::route_alloc(size as usize, type_id, site_id as u32)
}

/// Free a pointer previously returned by [`newbf_alloc`]. A null pointer is a
/// no-op (matching `free(NULL)`). In `Stomp` mode the ledger maps the user
/// pointer to its allocation base and a double/wild free aborts with a crash
/// dump; in `Thunk` mode it forwards to `free`.
///
/// # Safety
/// C-ABI export; `ptr` must be null or a pointer returned by [`newbf_alloc`]
/// in the current guard mode and (in `Thunk` mode) not already freed.
#[unsafe(no_mangle)]
pub extern "C" fn newbf_free(ptr: *mut u8) {
    guard::route_free(ptr);
}

// ──────────────────────────────────────────────────────────────────── //
// Guard lifecycle C-ABI (memory-safety.md §A5/§A6, §4)                 //
// ──────────────────────────────────────────────────────────────────── //

/// Mode values for the C-ABI [`newbf_set_guard_mode`]: 0 = Thunk
/// (passthrough), 1 = Stomp (debug guard). Any other value selects Thunk.
///
/// # Safety
/// C-ABI export; no pointers, always safe to call.
#[unsafe(no_mangle)]
pub extern "C" fn newbf_set_guard_mode(mode: i32) {
    let m = if mode == 1 {
        GuardMode::Stomp
    } else {
        GuardMode::Thunk
    };
    set_guard_mode(m);
}

/// Clear the guard ledger and release all quarantined VM ranges (the in-process
/// corpus harness calls this between programs — memory-safety.md §4). No-op if
/// the guard was never used.
///
/// # Safety
/// C-ABI export; no pointers, always safe to call. Must not be called
/// concurrently with live guarded allocations of pointers still in use.
#[unsafe(no_mangle)]
pub extern "C" fn newbf_guard_reset() {
    guard::reset();
}

/// Print the current leak report (still-live, non-comptime allocations) to
/// stderr and return the count. The atexit/explicit leak report
/// (memory-safety.md §A4).
///
/// # Safety
/// C-ABI export; no pointers, always safe to call.
#[unsafe(no_mangle)]
pub extern "C" fn newbf_guard_report_leaks() -> u64 {
    use std::io::Write as _;
    let leaks = report_leaks();
    let mut stderr = std::io::stderr();
    if leaks.is_empty() {
        let _ = writeln!(stderr, "newbf guard: no leaks");
    } else {
        let _ = writeln!(stderr, "newbf guard: {} leak(s)", leaks.len());
        for l in &leaks {
            // MS-T7: name the leaking allocation's site when it resolves against
            // the registered table; otherwise show the bare site_id + address.
            match format_site(l.site_id) {
                Some(name) => {
                    let _ = writeln!(
                        stderr,
                        "  leak: {name} (ptr={:#018x} size={} type_id={})",
                        l.ptr, l.size, l.type_id
                    );
                }
                None => {
                    let _ = writeln!(
                        stderr,
                        "  leak: ptr={:#018x} size={} type_id={} site_id={}",
                        l.ptr, l.size, l.type_id, l.site_id
                    );
                }
            }
        }
    }
    let _ = stderr.flush();
    leaks.len() as u64
}

/// Register the module's `__newbf_alloc_sites` table (base pointer + entry
/// count) so a UAF / double-free / leak report names `<function> @ file:line`
/// (memory-safety.md §A7, MS-T7). The C-ABI seam: a host calls this once after
/// the module is JIT'd / loaded with the table's address (JIT hosts resolve it
/// via the same `lookup`; AOT can call this from the entry stub — MS-T3b). A
/// null `ptr` / `count == 0` clears the table.
///
/// # Safety
/// C-ABI export. `ptr` must point to `count` valid `{ char8* function, char8*
/// file, i32 line }` entries (the emitted `%struct.AllocSite` layout) that stay
/// valid for as long as any report may run. See [`register_alloc_sites`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn newbf_register_alloc_sites(ptr: *const AllocSiteRaw, count: i64) {
    let count = if count < 0 { 0 } else { count as usize };
    // SAFETY: forwarded per this export's contract.
    unsafe { register_alloc_sites(ptr, count) };
}

/// Enter a comptime evaluation scope: allocations until the matching
/// [`newbf_guard_exit_comptime`] are tagged comptime and excluded from leak
/// reports (memory-safety.md §A6). Called by `newbf-comptime` around the
/// per-call JIT in a later task.
///
/// # Safety
/// C-ABI export; no pointers, always safe to call. Must be paired with
/// [`newbf_guard_exit_comptime`].
#[unsafe(no_mangle)]
pub extern "C" fn newbf_guard_enter_comptime() {
    guard::guard_enter_comptime();
}

/// Exit a comptime evaluation scope (pair of [`newbf_guard_enter_comptime`]).
///
/// # Safety
/// C-ABI export; no pointers, always safe to call.
#[unsafe(no_mangle)]
pub extern "C" fn newbf_guard_exit_comptime() {
    guard::guard_exit_comptime();
}
