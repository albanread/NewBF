//! The tombstone ledger (memory-safety.md §A4 "Ledger + tombstones").
//!
//! A **side table** keyed by the *user pointer* the program holds. It is the
//! authoritative, deterministic signal for double-free / wild-free / leaks —
//! independent of page state. The cardinal rule (memory-safety.md §A3/§A4,
//! review correctness #7): on free we consult the **ledger first** and NEVER
//! dereference the (possibly already-decommitted) page header to decide. The
//! header lives in quarantined memory; reading it on the free path would
//! itself be a use-after-free.
//!
//! Tombstones are **never removed**: a freed entry transitions `Live → Freed`
//! and stays in the table forever, so a stale pointer always re-finds its
//! tombstone (the address is retired, never recycled). This is what makes
//! double-free detection deterministic for the lifetime of a run.

use std::collections::HashMap;

/// Tombstone state of a ledger entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    /// Allocated and not yet freed.
    Live,
    /// Freed — a persistent tombstone; the entry is kept so a later free of
    /// the same pointer is detected as a double-free.
    Freed,
}

/// Lifecycle phase an allocation was made in (memory-safety.md §A6). Comptime
/// allocations are excluded from the leak report and live-count assertions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// Ordinary application allocation.
    App,
    /// Made while `enter_comptime`/`exit_comptime` bracketed JIT'd comptime
    /// evaluation; excluded from leak reporting.
    Comptime,
}

/// One ledger entry. `base` is the true allocation base (the page-aligned
/// region start) so the stomp allocator can decommit/quarantine the right
/// pages even when the user pointer the program holds differs from the base
/// (objects are page-end-aligned; arrays/raw are front-aligned — but MS-T1's
/// allocator front-aligns everything, MS-T2 adds the per-kind shaping).
#[derive(Clone, Copy, Debug)]
pub struct AllocMeta {
    /// True allocation base (start of the committed page region).
    pub base: usize,
    /// Number of pages committed for this allocation (for decommit on free).
    pub num_pages: usize,
    /// User-visible size in bytes.
    pub size: usize,
    /// Alloc-site index (0 until MS-T7 wires the site table).
    pub site_id: u32,
    /// Beef `StructId.0` for objects, `-1` (as `u32::MAX`) for array/raw.
    pub type_id: i32,
    /// Generation counter (monotonic; aids correlation in dumps).
    pub generation: u32,
    /// Live / Freed (tombstone).
    pub state: State,
    /// Alloc-site index recorded at free time (for the double-free message).
    pub free_site: u32,
    /// App vs Comptime.
    pub phase: Phase,
}

/// The verdict of consulting the ledger on a free, computed **without** ever
/// touching the freed memory. The caller (stomp allocator) acts on it: a
/// `FirstFree` decommits/quarantines the pages; `DoubleFree`/`WildFree` route
/// to the abort+dump path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FreeVerdict {
    /// First free of a live allocation. Carries the base + page count so the
    /// allocator can quarantine the right region.
    FirstFree { base: usize, num_pages: usize },
    /// The pointer is already tombstoned — a double-free.
    DoubleFree,
    /// The pointer was never allocated (or never live) — a wild free.
    WildFree,
}

/// Running counters published to the crash-dump shadow state.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub live_allocs: u64,
    pub live_bytes: u64,
    pub total_allocs: u64,
    pub total_frees: u64,
}

/// One leak record returned by [`Ledger::leaks`].
#[derive(Clone, Copy, Debug)]
pub struct Leak {
    pub ptr: usize,
    pub size: usize,
    pub site_id: u32,
    pub type_id: i32,
}

/// The side-table ledger. Not thread-safe by itself — the [`super::mod`]-level
/// guard holds it behind a `Mutex` and publishes stats via the lock-free
/// crash-dump atomics after each op.
pub struct Ledger {
    /// User-ptr → meta. Tombstones are kept forever (never removed).
    entries: HashMap<usize, AllocMeta>,
    next_gen: u32,
    stats: Stats,
}

impl Default for Ledger {
    fn default() -> Self {
        Self::new()
    }
}

impl Ledger {
    pub fn new() -> Self {
        Ledger {
            entries: HashMap::new(),
            next_gen: 1,
            stats: Stats::default(),
        }
    }

    /// Record a fresh live allocation keyed by the user pointer `ptr`. Returns
    /// the generation assigned. Panics in debug if `ptr` is already a *live*
    /// key (the allocator must never hand out a live address twice — quarantine
    /// guarantees this); a tombstoned key being reused would also be a bug, but
    /// since pages are never recycled the allocator cannot produce it.
    // The arguments are the natural fields of a ledger entry (ptr + base + the
    // five metadata values); bundling them into a struct would just shuffle the
    // same data through an extra type for one internal caller.
    #[allow(clippy::too_many_arguments)]
    pub fn record_alloc(
        &mut self,
        ptr: usize,
        base: usize,
        num_pages: usize,
        size: usize,
        type_id: i32,
        site_id: u32,
        phase: Phase,
    ) -> u32 {
        debug_assert!(
            !matches!(self.entries.get(&ptr), Some(m) if m.state == State::Live),
            "ledger: re-recording a live pointer {ptr:#x} (quarantine violated)"
        );
        let generation = self.next_gen;
        self.next_gen = self.next_gen.wrapping_add(1);
        self.entries.insert(
            ptr,
            AllocMeta {
                base,
                num_pages,
                size,
                site_id,
                type_id,
                generation,
                state: State::Live,
                free_site: 0,
                phase,
            },
        );
        self.stats.total_allocs += 1;
        // Comptime allocations are still counted in live metrics (the dump
        // shows true heap state); they're only excluded from *leak reporting*.
        self.stats.live_allocs += 1;
        self.stats.live_bytes += size as u64;
        generation
    }

    /// Consult the ledger for a free of user pointer `ptr`, **without touching
    /// the freed memory**. On a valid first free, transitions the entry to
    /// `Freed` (a persistent tombstone) and updates counters; the caller then
    /// quarantines the returned page region. Double/wild frees leave the table
    /// unchanged and return the corresponding verdict for the abort path.
    pub fn note_free(&mut self, ptr: usize, free_site: u32) -> FreeVerdict {
        match self.entries.get_mut(&ptr) {
            None => FreeVerdict::WildFree,
            Some(meta) => match meta.state {
                State::Freed => FreeVerdict::DoubleFree,
                State::Live => {
                    meta.state = State::Freed;
                    meta.free_site = free_site;
                    let base = meta.base;
                    let num_pages = meta.num_pages;
                    let size = meta.size;
                    self.stats.total_frees += 1;
                    self.stats.live_allocs = self.stats.live_allocs.saturating_sub(1);
                    self.stats.live_bytes = self.stats.live_bytes.saturating_sub(size as u64);
                    FreeVerdict::FirstFree { base, num_pages }
                }
            },
        }
    }

    /// Look up an entry (for tests / dump correlation) without mutating.
    pub fn get(&self, ptr: usize) -> Option<&AllocMeta> {
        self.entries.get(&ptr)
    }

    /// Current counters.
    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// Number of currently-live entries (state == Live). O(n); for reporting,
    /// not the fast path.
    pub fn live_count(&self) -> usize {
        self.entries.values().filter(|m| m.state == State::Live).count()
    }

    /// Number of tombstones (state == Freed).
    pub fn freed_count(&self) -> usize {
        self.entries.values().filter(|m| m.state == State::Freed).count()
    }

    /// Walk still-live, non-comptime entries for the leak report
    /// (memory-safety.md §A4). Comptime allocations are excluded.
    pub fn leaks(&self) -> Vec<Leak> {
        self.entries
            .iter()
            .filter(|(_, m)| m.state == State::Live && m.phase != Phase::Comptime)
            .map(|(&ptr, m)| Leak {
                ptr,
                size: m.size,
                site_id: m.site_id,
                type_id: m.type_id,
            })
            .collect()
    }

    /// Snapshot every live entry's true base + page count (for `reset` to
    /// quarantine/release them before clearing the table).
    pub fn live_regions(&self) -> Vec<(usize, usize)> {
        self.entries
            .values()
            .filter(|m| m.state == State::Live)
            .map(|m| (m.base, m.num_pages))
            .collect()
    }

    /// Clear all entries and counters (used by `newbf_guard_reset` between
    /// corpus programs). The allocator releases the VM ranges separately.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.next_gen = 1;
        self.stats = Stats::default();
    }
}
