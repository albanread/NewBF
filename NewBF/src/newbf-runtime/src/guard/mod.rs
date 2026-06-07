//! The debug memory guard: quarantining stomp allocator + tombstone ledger,
//! behind a runtime mode switch (memory-safety.md §A3–A6).
//!
//! ## Mode (memory-safety.md §A5)
//! `newbf_alloc`/`newbf_free` are **always present**; behavior is chosen by a
//! relaxed-atomic mode flag, NOT by how `newbf-runtime` itself was compiled:
//!   * **`Thunk`** (the DEFAULT) — straight `malloc`/`free` passthrough, no
//!     guard. This keeps un-guarded callers working: MS-T0's smoke test and the
//!     run-corpus (which don't enable the guard until MS-T3) stay green.
//!   * **`Stomp`** — the debug guard: every alloc gets its own quarantined page
//!     region; every free is checked against the ledger (double-free/wild-free
//!     → abort + crash dump); leaks are reportable.
//!
//! The guard activates ONLY when something calls `newbf_set_guard_mode(Stomp)`.
//!
//! ## Ledger-first, lock-free dump (memory-safety.md §A4, review correctness #7)
//! The free path consults the ledger before touching any page, so it never
//! dereferences a decommitted header. The crash-dump path reads ONLY the
//! lock-free atomics in `crash_dump` — it never takes this module's lock and
//! never walks the ledger. The abort-on-double-free path publishes the atomics,
//! releases the lock, then aborts.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use crate::crash_dump::{note_free, note_memory_guard_installed, update_guard_metrics};

pub mod ledger;
pub mod sites;
pub mod stomp;
pub mod vm;

use ledger::{FreeVerdict, Ledger, Phase};
use stomp::StompAlloc;
use vm::HostVm;

// ──────────────────────────────────────────────────────────────────── //
// Mode flag (memory-safety.md §A5)                                     //
// ──────────────────────────────────────────────────────────────────── //

/// Guard mode, read on the alloc/free fast path via a relaxed atomic.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GuardMode {
    /// Release passthrough: plain malloc/free, no guard. **The default.**
    Thunk = 0,
    /// Debug guard: quarantining stomp allocator + ledger.
    Stomp = 1,
}

/// CRITICAL: default is `Thunk` (passthrough) so nothing currently green breaks
/// (MS-T0 smoke test, run-corpus). The guard only activates on an explicit
/// `newbf_set_guard_mode(Stomp)`.
static MODE: AtomicU8 = AtomicU8::new(GuardMode::Thunk as u8);

/// Default pages per reserved VM range (16 MiB at 4 KiB pages). Large enough
/// that the corpus rarely reserves a second range; quarantine never recycles,
/// so `reset` releases ranges between programs to bound growth.
const DEFAULT_RANGE_PAGES: usize = 4096;

#[inline]
fn current_mode() -> GuardMode {
    match MODE.load(Ordering::Relaxed) {
        x if x == GuardMode::Stomp as u8 => GuardMode::Stomp,
        _ => GuardMode::Thunk,
    }
}

/// Set the guard mode. Idempotent; safe to call multiple times. Marks the
/// crash-dump shadow state "guard installed" the first time `Stomp` is set so
/// the dump shows heap metrics instead of "(memory guard not installed)".
pub fn set_guard_mode(mode: GuardMode) {
    MODE.store(mode as u8, Ordering::Relaxed);
    if mode == GuardMode::Stomp {
        note_memory_guard_installed();
    }
}

// ──────────────────────────────────────────────────────────────────── //
// Comptime phase bit (memory-safety.md §A6)                            //
// ──────────────────────────────────────────────────────────────────── //
//
// A nesting depth (not a bool) so re-entrant comptime JIT calls compose. While
// > 0, allocations are tagged `Phase::Comptime` and excluded from leak reports.
// Process-wide (the comptime JIT is single-threaded per call); a relaxed atomic
// matches the lock-free publishing discipline.

static COMPTIME_DEPTH: AtomicUsize = AtomicUsize::new(0);

fn enter_comptime() {
    COMPTIME_DEPTH.fetch_add(1, Ordering::Relaxed);
}

fn exit_comptime() {
    // saturating: never underflow if mis-paired.
    let prev = COMPTIME_DEPTH.load(Ordering::Relaxed);
    if prev > 0 {
        COMPTIME_DEPTH.fetch_sub(1, Ordering::Relaxed);
    }
}

fn current_phase() -> Phase {
    if COMPTIME_DEPTH.load(Ordering::Relaxed) > 0 {
        Phase::Comptime
    } else {
        Phase::App
    }
}

// ──────────────────────────────────────────────────────────────────── //
// The global guard (stomp allocator + ledger behind one Mutex)         //
// ──────────────────────────────────────────────────────────────────── //

/// Combined guard state, generic over the VM so unit tests can drive the exact
/// same alloc/free/abort logic with a deterministic `MockVm` (no global state,
/// no real address space) while production uses the `HostVm`. v1 uses a single
/// `Mutex` (memory-safety.md §4 "Concurrency / re-entrancy"); the crash-dump
/// path stays lock-free.
pub(crate) struct Guard<V: vm::Vm> {
    stomp: StompAlloc<V>,
    ledger: Ledger,
}

impl<V: vm::Vm> Guard<V> {
    pub(crate) fn with_vm(vm: V, range_pages: usize) -> Self {
        Guard {
            stomp: StompAlloc::new(vm, range_pages),
            ledger: Ledger::new(),
        }
    }

    /// Publish current ledger counters to the lock-free crash-dump atomics.
    /// Called while the lock is held (the values are read out before the
    /// store), but the *dump* never reads back through the lock.
    fn publish(&self) {
        let s = self.ledger.stats();
        update_guard_metrics(s.live_allocs, s.live_bytes, s.total_allocs, s.total_frees);
    }

    /// Allocate under the guard, recording the allocation in the ledger keyed
    /// by the returned user pointer. Returns null on VM failure. `phase` is the
    /// caller-resolved comptime tag.
    pub(crate) fn alloc(&mut self, size: usize, type_id: i32, site_id: u32, phase: Phase) -> *mut u8 {
        match self.stomp.alloc(size, site_id) {
            None => core::ptr::null_mut(),
            Some(a) => {
                self.ledger
                    .record_alloc(a.user_ptr, a.base, a.num_pages, size, type_id, site_id, phase);
                self.publish();
                a.user_ptr as *mut u8
            }
        }
    }

    /// Ledger-first free check + quarantine. Returns `Err` (without aborting)
    /// for double/wild frees — the **testable seam**: a unit test asserts the
    /// classification without the process aborting. The freed page is never
    /// dereferenced (the verdict comes purely from the side table).
    pub(crate) fn check_free(&mut self, ptr: usize, free_site: u32) -> Result<(), FreeError> {
        let verdict = self.ledger.note_free(ptr, free_site);
        let result = match verdict {
            FreeVerdict::FirstFree { base, num_pages } => {
                self.stomp.quarantine(base, num_pages);
                Ok(())
            }
            FreeVerdict::DoubleFree => Err(FreeError::DoubleFree),
            FreeVerdict::WildFree => Err(FreeError::WildFree),
        };
        note_free(ptr);
        self.publish();
        result
    }

    /// Clear the ledger and release all quarantined VM ranges.
    pub(crate) fn reset(&mut self) {
        self.stomp.release_all();
        self.ledger.clear();
        self.publish();
    }

    /// MS-T7: the `site_id` recorded at the original `new` for the ledger entry
    /// keyed by `ptr` (live OR tombstoned — entries are never removed), so the
    /// double-free abort can name the offending allocation site. `None` for a
    /// wild free (pointer never allocated).
    pub(crate) fn alloc_site_of(&self, ptr: usize) -> Option<u32> {
        self.ledger.get(ptr).map(|m| m.site_id)
    }
}

// `OnceLock<Mutex<Guard>>` — lazily built on first guarded use.
static GUARD: std::sync::OnceLock<Mutex<Guard<HostVm>>> = std::sync::OnceLock::new();

fn guard() -> &'static Mutex<Guard<HostVm>> {
    GUARD.get_or_init(|| Mutex::new(Guard::with_vm(vm::host_vm(), DEFAULT_RANGE_PAGES)))
}

// ──────────────────────────────────────────────────────────────────── //
// Free verdict for the abort path (testable seam)                      //
// ──────────────────────────────────────────────────────────────────── //

/// Why a free aborts. Returned by [`check_free`] so a unit test can assert the
/// ledger's detection WITHOUT killing the test runner: the abort itself lives
/// in [`free`], which calls `check_free` then aborts only on `Err`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FreeError {
    /// The pointer is already tombstoned.
    DoubleFree,
    /// The pointer was never allocated.
    WildFree,
}

// ──────────────────────────────────────────────────────────────────── //
// Stomp alloc / free (mode == Stomp path), over the global guard       //
// ──────────────────────────────────────────────────────────────────── //

/// Allocate under the global stomp guard. Resolves the comptime phase, then
/// delegates to [`Guard::alloc`]. The lock is released before returning.
fn stomp_alloc(size: usize, type_id: i32, site_id: u32) -> *mut u8 {
    let phase = current_phase();
    let mut g = guard().lock().unwrap();
    g.alloc(size, type_id, site_id, phase)
}

/// The guarded free: ledger-first check (under the lock), then — if the ledger
/// reports a double/wild free — release the lock and abort with a crash dump.
/// The dump reads only the lock-free atomics already published by `check_free`.
fn stomp_free(ptr: *mut u8) {
    if ptr.is_null() {
        return; // free(NULL) is a no-op.
    }
    let (result, site_id) = {
        let mut g = guard().lock().unwrap();
        // MS-T7: on a double-free the offending pointer still has its (now
        // tombstoned) ledger entry recording the original `new`'s `site_id`. Read
        // it under the lock (the entry is never removed) so the abort message can
        // name `<function> @ file:line` of that `new`. `None` for a wild free
        // (no entry) — the address is the only available locator.
        let site_id = g.alloc_site_of(ptr as usize);
        let result = g.check_free(ptr as usize, 0);
        (result, site_id)
        // lock released here (scope end) before any abort.
    };
    if let Err(kind) = result {
        abort_on_bad_free(ptr as usize, kind, site_id);
    }
}

/// Abort + crash dump on a detected double/wild free. The lock is already
/// released (check_free returned) and metrics already published, so the dump
/// (lock-free atomics only) is safe. We write a short message then `abort()` —
/// the SEH filter / panic hook is not involved; this is a deliberate,
/// diagnosed termination, not a fault.
#[cold]
fn abort_on_bad_free(ptr: usize, kind: FreeError, site_id: Option<u32>) -> ! {
    use std::io::Write as _;
    let msg = match kind {
        FreeError::DoubleFree => "double free",
        FreeError::WildFree => "wild free (pointer never allocated)",
    };
    // MS-T7: name the offending allocation site (`<function> @ file:line`) when
    // its `site_id` resolves against the registered table; otherwise fall back to
    // the bare address (the address is always available; the name needs both a
    // recorded site and a registered table — debug only).
    let named = site_id.and_then(sites::format_site);
    // Best-effort stderr line; the crash-dump atomics already carry the heap
    // state and last-free address for a fuller picture.
    let mut stderr = std::io::stderr();
    let _ = match &named {
        Some(name) => writeln!(
            stderr,
            "newbf guard: {msg} of {ptr:#018x} (allocated at {name}) -> abort"
        ),
        None => writeln!(stderr, "newbf guard: {msg} of {ptr:#018x} -> abort"),
    };
    let _ = stderr.flush();
    std::process::abort();
}

// ──────────────────────────────────────────────────────────────────── //
// Mode-routed public alloc / free (called by lib.rs C-ABI exports)     //
// ──────────────────────────────────────────────────────────────────── //

unsafe extern "C" {
    fn malloc(size: usize) -> *mut u8;
    fn free(ptr: *mut u8);
}

/// Route an allocation through the current mode. `size` is the i64 from the
/// C-ABI (already validated non-negative by the caller). `type_id`/`site_id`
/// are guard metadata.
pub fn route_alloc(size: usize, type_id: i32, site_id: u32) -> *mut u8 {
    match current_mode() {
        GuardMode::Thunk => {
            // Passthrough. SAFETY: CRT malloc; size is a valid usize.
            unsafe { malloc(size) }
        }
        GuardMode::Stomp => stomp_alloc(size, type_id, site_id),
    }
}

/// Route a free through the current mode. `ptr` is null or a pointer previously
/// returned by [`route_alloc`] in the **same mode** (mixing modes across an
/// alloc/free pair is a caller bug). This wrapper does not dereference `ptr`
/// itself — `Thunk` hands the raw pointer to the CRT `free`, and `Stomp` only
/// uses the integer address to consult the ledger (the page is never
/// dereferenced — ledger-first). The validity contract is the C-ABI boundary's
/// ([`crate::newbf_free`]), so this dispatcher is kept safe-to-call and the
/// pointer-passing lint is allowed deliberately.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn route_free(ptr: *mut u8) {
    match current_mode() {
        GuardMode::Thunk => {
            if ptr.is_null() {
                return;
            }
            // SAFETY: per the contract, `ptr` came from `malloc` (Thunk mode).
            unsafe { free(ptr) }
        }
        GuardMode::Stomp => stomp_free(ptr),
    }
}

// ──────────────────────────────────────────────────────────────────── //
// Lifecycle API (memory-safety.md §4 reset / §A4 leak report / §A6)    //
// ──────────────────────────────────────────────────────────────────── //

/// Clear the ledger and release all quarantined VM ranges. Used by the
/// in-process corpus harness between programs so the global ledger is clean and
/// address-space growth is bounded (memory-safety.md §4). No-op effect on the
/// mode flag and the comptime depth (those are process-level policy).
pub fn reset() {
    // Only meaningful if the guard was ever built; if it wasn't, nothing to do.
    if let Some(m) = GUARD.get() {
        m.lock().unwrap().reset();
    }
}

/// A leak record surfaced to the report API.
#[derive(Clone, Copy, Debug)]
pub struct LeakReport {
    pub ptr: usize,
    pub size: usize,
    pub site_id: u32,
    pub type_id: i32,
}

/// Walk still-live, non-comptime entries and return them (the on-demand leak
/// report, memory-safety.md §A4). Returns an empty vec if the guard was never
/// used. Does NOT abort — leaks are reported, not fatal.
pub fn report_leaks() -> Vec<LeakReport> {
    match GUARD.get() {
        None => Vec::new(),
        Some(m) => {
            let g = m.lock().unwrap();
            g.ledger
                .leaks()
                .into_iter()
                .map(|l| LeakReport {
                    ptr: l.ptr,
                    size: l.size,
                    site_id: l.site_id,
                    type_id: l.type_id,
                })
                .collect()
        }
    }
}

/// Number of currently-live (un-freed, non-tombstoned) ledger entries, for the
/// guard_corpus harness's `live == N` assertions.
pub fn live_count() -> usize {
    match GUARD.get() {
        None => 0,
        Some(m) => m.lock().unwrap().ledger.live_count(),
    }
}

// ──────────────────────────────────────────────────────────────────── //
// Comptime bracket (memory-safety.md §A6), re-exported as C-ABI in lib  //
// ──────────────────────────────────────────────────────────────────── //

/// Begin a comptime evaluation scope: allocations until the matching
/// [`guard_exit_comptime`] are tagged `Phase::Comptime` and excluded from leak
/// reports.
pub fn guard_enter_comptime() {
    enter_comptime();
}

/// End a comptime evaluation scope.
pub fn guard_exit_comptime() {
    exit_comptime();
}

// ──────────────────────────────────────────────────────────────────── //
// Tests                                                                 //
// ──────────────────────────────────────────────────────────────────── //

#[cfg(test)]
mod tests;
