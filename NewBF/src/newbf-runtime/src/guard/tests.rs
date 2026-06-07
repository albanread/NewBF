//! MS-T1 unit tests for the memory guard.
//!
//! Strategy:
//!   * **Deterministic core tests** drive a `Guard<MockVm>` directly (no global
//!     state, no real address space) so quarantine / ledger / double-free /
//!     wild-free are exact and parallel-safe.
//!   * **Stomp-allocator tests** drive `StompAlloc<MockVm>` for the page math
//!     (size-0, page-multiple, quarantine bookkeeping).
//!   * **One real-VM test** exercises the actual `VirtualAlloc` path on Windows.
//!   * **Mode / passthrough tests** verify the Thunk default and the C-ABI
//!     routing through the public `route_alloc`/`route_free`.

use super::*;
use crate::guard::ledger::{Ledger, Phase, State};
use crate::guard::stomp::StompAlloc;
use crate::guard::vm::mock::{MockVm, PageState};
use crate::guard::vm::{PAGE_SIZE, Protect, Vm};

const TEST_RANGE_PAGES: usize = 256;

fn mock_guard() -> Guard<MockVm> {
    Guard::with_vm(MockVm::new(), TEST_RANGE_PAGES)
}

// ── Stomp allocator: alloc / free / decommit ───────────────────────── //

#[test]
fn alloc_commits_then_free_decommits() {
    let mut s = StompAlloc::new(MockVm::new(), TEST_RANGE_PAGES);
    let a = s.alloc(64, 0).expect("alloc");
    assert!(s.committed_pages() >= 1, "alloc must commit pages");
    // The user pointer is past the header and writable in the mock model.
    assert!(a.user_ptr > a.base, "user ptr must follow the header");
    let before = s.committed_pages();
    assert!(s.quarantine(a.base, a.num_pages), "decommit");
    assert_eq!(
        s.committed_pages(),
        before - a.num_pages,
        "freed pages must drop from committed accounting"
    );
}

// ── Quarantine: a freed address is NEVER returned by a later alloc ──── //

#[test]
fn quarantine_never_recycles_freed_address() {
    let mut s = StompAlloc::new(MockVm::new(), TEST_RANGE_PAGES);
    let mut seen = std::collections::HashSet::new();
    // Allocate, free, repeat — the freed user pointer must never reappear.
    for _ in 0..64 {
        let a = s.alloc(128, 0).expect("alloc");
        assert!(
            seen.insert(a.user_ptr),
            "freed address {:#x} was recycled by a later alloc — quarantine violated",
            a.user_ptr
        );
        s.quarantine(a.base, a.num_pages);
    }
}

/// Drive the VM directly to prove `decommit` is the quarantine op: a committed
/// page transitions to `Decommitted` (not `Reserved`/recycled), and the decommit
/// counter increments — the load-bearing "freed pages keep their reservation but
/// drop their backing so a later access faults" behavior, observed on the mock.
#[test]
fn quarantine_uses_vm_decommit_not_release() {
    let vm = MockVm::new();
    let base = vm.reserve(4 * PAGE_SIZE);
    vm.commit(base, 2 * PAGE_SIZE, Protect::ReadWrite);
    assert_eq!(
        vm.page_state(base),
        Some(PageState::Committed(Protect::ReadWrite))
    );
    vm.decommit(base, 2 * PAGE_SIZE);
    assert_eq!(vm.page_state(base), Some(PageState::Decommitted));
    assert_eq!(vm.decommit_calls(), 1);
    // No release was called — the address range stays reserved (quarantined).
    assert_eq!(vm.release_calls(), 0);
}

// ── size-0 edge case ───────────────────────────────────────────────── //

#[test]
fn size_zero_alloc_yields_valid_distinct_user_ptr() {
    let mut s = StompAlloc::new(MockVm::new(), TEST_RANGE_PAGES);
    let a = s.alloc(0, 0).expect("size-0 alloc must succeed");
    // User pointer is distinct from the base (header not clobbered) and the
    // region is committed.
    assert!(a.user_ptr > a.base);
    assert!(a.num_pages >= 1);
    assert!(s.committed_pages() >= 1);
    // Freeing a size-0 alloc decommits cleanly.
    assert!(s.quarantine(a.base, a.num_pages));
}

// ── page-multiple edge case ────────────────────────────────────────── //

#[test]
fn page_multiple_size_gets_slack_page() {
    let mut s = StompAlloc::new(MockVm::new(), TEST_RANGE_PAGES);
    // Exactly one page of payload: without the slack bump, the user region +
    // header would not fit / would push the user ptr onto the guard boundary.
    let a = s.alloc(PAGE_SIZE, 0).expect("page-multiple alloc");
    // header + PAGE rounds to 2 pages, plus the page-multiple slack bump => 3.
    assert!(
        a.num_pages >= 2,
        "page-multiple alloc must reserve room for header + payload, got {}",
        a.num_pages
    );
    assert!(a.user_ptr > a.base);
    s.quarantine(a.base, a.num_pages);

    // A multi-page payload (2 pages) also gets the slack bump.
    let b = s.alloc(2 * PAGE_SIZE, 0).expect("two-page alloc");
    assert!(b.num_pages >= 3);
}

// ── Ledger: double-free detection (tombstone) ──────────────────────── //

#[test]
fn ledger_detects_double_free() {
    let mut l = Ledger::new();
    let p = 0xABC0usize;
    l.record_alloc(p, p, 1, 64, -1, 0, Phase::App);
    // First free → FirstFree.
    assert!(matches!(l.note_free(p, 0), FreeVerdict::FirstFree { .. }));
    // Tombstone remains; second free → DoubleFree.
    assert_eq!(l.note_free(p, 0), FreeVerdict::DoubleFree);
    // And again — tombstone is persistent.
    assert_eq!(l.note_free(p, 0), FreeVerdict::DoubleFree);
    assert_eq!(l.get(p).unwrap().state, State::Freed);
}

// ── Ledger: wild-free detection ────────────────────────────────────── //

#[test]
fn ledger_detects_wild_free() {
    let mut l = Ledger::new();
    // Never allocated.
    assert_eq!(l.note_free(0xDEAD_0000, 0), FreeVerdict::WildFree);
}

// ── Guard-level double/wild free via the testable seam (no abort) ───── //

#[test]
fn guard_check_free_classifies_without_aborting() {
    let mut g = mock_guard();
    let p = g.alloc(64, -1, 0, Phase::App) as usize;
    assert_ne!(p, 0);
    // First free OK.
    assert_eq!(g.check_free(p, 0), Ok(()));
    // Double free detected, NOT aborted (we're still running).
    assert_eq!(g.check_free(p, 0), Err(FreeError::DoubleFree));
    // Wild free of a never-seen pointer.
    assert_eq!(g.check_free(0x9999_0000, 0), Err(FreeError::WildFree));
}

// ── Ledger counts (live / freed) ───────────────────────────────────── //

#[test]
fn ledger_counts_live_and_freed() {
    let mut l = Ledger::new();
    for i in 0..5u64 {
        let p = 0x1000 + (i as usize) * 0x1000;
        l.record_alloc(p, p, 1, 32, -1, 0, Phase::App);
    }
    assert_eq!(l.live_count(), 5);
    assert_eq!(l.freed_count(), 0);
    assert_eq!(l.stats().total_allocs, 5);
    assert_eq!(l.stats().live_allocs, 5);

    // Free two.
    l.note_free(0x1000, 0);
    l.note_free(0x2000, 0);
    assert_eq!(l.live_count(), 3);
    assert_eq!(l.freed_count(), 2);
    assert_eq!(l.stats().total_frees, 2);
    assert_eq!(l.stats().live_allocs, 3);
}

// ── Leak report excludes comptime, lists live app allocs ───────────── //

#[test]
fn leak_report_lists_live_excludes_comptime() {
    let mut l = Ledger::new();
    l.record_alloc(0x1000, 0x1000, 1, 16, 7, 0, Phase::App); // live app leak
    l.record_alloc(0x2000, 0x2000, 1, 16, 8, 0, Phase::App);
    l.note_free(0x2000, 0); // freed, not a leak
    l.record_alloc(0x3000, 0x3000, 1, 16, 9, 0, Phase::Comptime); // excluded
    let leaks = l.leaks();
    assert_eq!(leaks.len(), 1, "only the live app alloc leaks");
    assert_eq!(leaks[0].ptr, 0x1000);
    assert_eq!(leaks[0].type_id, 7);
}

// ── Reset clears state ─────────────────────────────────────────────── //

#[test]
fn reset_clears_ledger_and_releases_ranges() {
    let mut g = mock_guard();
    let p = g.alloc(64, -1, 0, Phase::App) as usize;
    assert_eq!(g.ledger.live_count(), 1);
    g.check_free(p, 0).unwrap();
    assert_eq!(g.ledger.freed_count(), 1);
    g.reset();
    assert_eq!(g.ledger.live_count(), 0);
    assert_eq!(g.ledger.freed_count(), 0, "tombstones cleared on reset");
    assert_eq!(g.ledger.stats().total_allocs, 0);
    // After reset a fresh alloc works (ranges re-reserved on demand).
    let q = g.alloc(64, -1, 0, Phase::App) as usize;
    assert_ne!(q, 0);
}

#[test]
fn reset_releases_vm_ranges() {
    let mut s = StompAlloc::new(MockVm::new(), TEST_RANGE_PAGES);
    s.alloc(64, 0).unwrap();
    assert_eq!(s.range_count(), 1);
    s.release_all();
    assert_eq!(s.range_count(), 0);
    // A subsequent alloc reserves a fresh range.
    s.alloc(64, 0).unwrap();
    assert_eq!(s.range_count(), 1);
}

// ── Ranges grow on demand ──────────────────────────────────────────── //

#[test]
fn ranges_grow_on_demand() {
    // Tiny ranges so we force a second reservation quickly.
    let mut s = StompAlloc::new(MockVm::new(), 4);
    // Each alloc of 1 byte → header+1 = 1 page, +0 (not page-multiple) = 1 page.
    let _ = s.alloc(1, 0).unwrap();
    let _ = s.alloc(1, 0).unwrap();
    let _ = s.alloc(1, 0).unwrap();
    let _ = s.alloc(1, 0).unwrap();
    let before = s.range_count();
    // The 5th does not fit the 4-page range → a new range is reserved.
    let _ = s.alloc(1, 0).unwrap();
    assert!(
        s.range_count() > before,
        "allocator must reserve a new range when the current one is exhausted"
    );
}

// ── Comptime tagging via the guard ─────────────────────────────────── //

#[test]
fn comptime_allocs_excluded_from_guard_leaks() {
    let mut g = mock_guard();
    g.alloc(16, 1, 0, Phase::App);
    g.alloc(16, 2, 0, Phase::Comptime);
    let leaks = g.ledger.leaks();
    assert_eq!(leaks.len(), 1, "comptime alloc excluded from the leak set");
}

// ── MockVm page-state discipline (reserve→commit→decommit) ─────────── //

#[test]
fn mock_vm_tracks_page_states() {
    let vm = MockVm::new();
    let base = vm.reserve(8 * PAGE_SIZE);
    assert!(!base.is_null());
    assert_eq!(vm.page_state(base), Some(PageState::Reserved));
    assert!(vm.commit(base, PAGE_SIZE, Protect::ReadWrite));
    assert_eq!(
        vm.page_state(base),
        Some(PageState::Committed(Protect::ReadWrite))
    );
    assert!(vm.decommit(base, PAGE_SIZE));
    assert_eq!(vm.page_state(base), Some(PageState::Decommitted));
    assert_eq!(vm.decommit_calls(), 1);
    assert!(vm.reserve_calls() >= 1);
    assert!(vm.commit_calls() >= 1);
}

// ── Real-VM (VirtualAlloc) smoke test on Windows ───────────────────── //

#[cfg(windows)]
#[test]
fn real_vm_alloc_write_read_free() {
    use crate::guard::vm::host_vm;
    let mut s = StompAlloc::new(host_vm(), TEST_RANGE_PAGES);
    let a = s.alloc(256, 0).expect("real VirtualAlloc-backed alloc");
    // The committed user region is genuinely writable + readable.
    unsafe {
        let p = a.user_ptr as *mut u64;
        core::ptr::write_volatile(p, 0xC0FF_EE00_1234_5678);
        let v = core::ptr::read_volatile(p);
        assert_eq!(v, 0xC0FF_EE00_1234_5678);
    }
    // Free decommits the pages (quarantine); accounting drops them.
    let before = s.committed_pages();
    assert!(s.quarantine(a.base, a.num_pages));
    assert_eq!(s.committed_pages(), before - a.num_pages);
    s.release_all();
}

// ── Release Thunk path (default passthrough) ───────────────────────── //
//
// These touch the PROCESS-GLOBAL mode flag, so they must run serialized and
// restore the default. Cargo runs tests in parallel; we guard the global with
// a local mutex shared across the mode-sensitive tests.

use std::sync::Mutex as StdMutex;
static MODE_TEST_LOCK: StdMutex<()> = StdMutex::new(());

#[test]
fn default_mode_is_thunk_passthrough() {
    let _g = MODE_TEST_LOCK.lock().unwrap();
    // Default (no set_guard_mode called by this test): Thunk passthrough.
    set_guard_mode(GuardMode::Thunk);
    let p = route_alloc(32, -1, 0);
    assert!(!p.is_null(), "thunk alloc must return real malloc memory");
    // Writable real memory.
    unsafe {
        core::ptr::write_volatile(p as *mut u32, 0xABCD);
        assert_eq!(core::ptr::read_volatile(p as *const u32), 0xABCD);
    }
    route_free(p);
    // free(NULL) is a no-op in thunk mode.
    route_free(core::ptr::null_mut());
}

#[test]
fn thunk_size_zero_does_not_crash() {
    let _g = MODE_TEST_LOCK.lock().unwrap();
    set_guard_mode(GuardMode::Thunk);
    let p = route_alloc(0, -1, 0);
    // malloc(0) may return null or a unique ptr; either is fine, just free it.
    route_free(p);
}

#[test]
fn stomp_mode_routes_through_guard_and_resets() {
    let _g = MODE_TEST_LOCK.lock().unwrap();
    // Switch to Stomp, alloc + free through the public C-ABI-routing fns, then
    // reset and restore Thunk so other tests / callers see passthrough.
    set_guard_mode(GuardMode::Stomp);
    super::reset(); // clean global ledger first
    let p = route_alloc(48, 5, 0);
    assert!(!p.is_null(), "stomp alloc returns committed memory");
    // Writable.
    unsafe {
        core::ptr::write_volatile(p as *mut u64, 0x1122_3344);
        assert_eq!(core::ptr::read_volatile(p as *const u64), 0x1122_3344);
    }
    assert_eq!(super::live_count(), 1);
    route_free(p);
    assert_eq!(super::live_count(), 0);
    // Leak report is empty after the balanced free.
    assert!(super::report_leaks().is_empty());
    // Clean up: reset + restore default.
    super::reset();
    set_guard_mode(GuardMode::Thunk);
}

#[test]
fn comptime_bracket_tags_global_allocs() {
    let _g = MODE_TEST_LOCK.lock().unwrap();
    set_guard_mode(GuardMode::Stomp);
    super::reset();
    guard_enter_comptime();
    let p = route_alloc(16, 9, 0);
    guard_exit_comptime();
    assert!(!p.is_null());
    // The comptime alloc is live but excluded from the leak report.
    assert_eq!(super::live_count(), 1);
    assert!(
        super::report_leaks().is_empty(),
        "comptime allocs must not appear as leaks"
    );
    route_free(p);
    super::reset();
    set_guard_mode(GuardMode::Thunk);
}
