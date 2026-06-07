//! The quarantining stomp allocator (memory-safety.md §A3, ported from
//! `E:\beef\BeefRT\rt\StompAlloc.cpp` with one deliberate divergence).
//!
//! ## How it works
//! A large address range is `reserve`d up front (and more on demand). Each
//! allocation commits `ceil((size + header) / PAGE)` pages, writes a header at
//! the page start, and returns a user pointer **front-aligned** just past the
//! header (MS-T1 front-aligns every kind; MS-T2 adds per-`AllocKind` page-end
//! shaping for objects). On free the pages are **decommitted but the address
//! range is never released or recycled** — so any later read/write through the
//! freed pointer hits decommitted memory and faults deterministically. This is
//! the quarantine divergence from the reference `StompAlloc.cpp` (which
//! recycles pages, making UAF only probabilistic — memory-safety.md §1
//! "Guarantee precision").
//!
//! ## Edge cases (ported exactly — memory-safety.md §A3)
//!   * **size == 0** and **size a page multiple**: the reference bumps the
//!     aligned offset by a page so the user region never starts exactly at the
//!     header (clobbering it) and a size-0 alloc still yields a faulting guard
//!     region. We allocate at least one extra slack page for these so there is
//!     always a valid, writable user pointer distinct from the header, and the
//!     pages still decommit on free.
//!
//! ## The free path is ledger-first
//! This allocator NEVER dereferences the page header to decide a free — the
//! header may already be decommitted (a double-free of a quarantined pointer).
//! The caller ([`super`]) consults the [`super::ledger::Ledger`] first; this
//! module only performs the VM decommit for a verdict the ledger already
//! approved.

use super::vm::{PAGE_SIZE, PROT_READWRITE, Vm};

/// Magic word written into the page header (for dump correlation only — never
/// read on the free path, which is ledger-first).
const HEADER_MAGIC: u32 = 0x5750_4F54; // "TOPW" little-endian-ish marker.

/// Per-allocation header at the start of the committed page region. It is NOT
/// the source of truth for free decisions (the ledger is); it exists so a
/// crash dump or debugger inspecting a *still-committed* page can correlate.
#[repr(C)]
#[derive(Clone, Copy)]
struct AllocHeader {
    num_pages: u32,
    magic: u32,
    site_id: u32,
    generation: u32,
}

/// One reserved VM region plus a cursor and a committed-page bitset. Regions
/// grow on demand; freed pages are never reused, so the cursor only advances.
struct Range {
    base: usize,
    /// Total pages reserved in this range.
    total_pages: usize,
    /// Next free page index (the cursor; never rewinds — quarantine).
    next_page: usize,
    /// One bit per page: committed (true) or not. Used for `reset` accounting
    /// and to know what to decommit.
    committed: Vec<bool>,
}

/// Result of a successful allocation.
pub struct Allocation {
    /// User pointer handed to the program.
    pub user_ptr: usize,
    /// True base of the committed region (page-aligned) — the ledger stores
    /// this so free can decommit the right pages.
    pub base: usize,
    /// Pages committed for this allocation.
    pub num_pages: usize,
}

/// The stomp allocator. Holds reserved ranges and grows them on demand. Not
/// internally synchronized — the [`super`] guard owns the `Mutex`.
pub struct StompAlloc<V: Vm> {
    vm: V,
    ranges: Vec<Range>,
    /// Pages reserved per new range (grown geometrically would be nicer, but a
    /// fixed large chunk keeps the math simple and matches StompAlloc).
    range_pages: usize,
    next_gen: u32,
}

impl<V: Vm> StompAlloc<V> {
    /// Create an allocator over `vm`. `range_pages` is how many pages each
    /// reserved range holds (a new range is reserved when the current one is
    /// exhausted).
    pub fn new(vm: V, range_pages: usize) -> Self {
        StompAlloc {
            vm,
            ranges: Vec::new(),
            range_pages,
            next_gen: 1,
        }
    }

    /// Total committed pages still live in all ranges (for tests).
    pub fn committed_pages(&self) -> usize {
        self.ranges
            .iter()
            .map(|r| r.committed.iter().filter(|&&c| c).count())
            .sum()
    }

    /// Number of reserved ranges (for tests — grows on demand).
    pub fn range_count(&self) -> usize {
        self.ranges.len()
    }

    /// Pages needed to satisfy `size` bytes including the header, with the
    /// size-0 / page-multiple slack bump applied.
    fn pages_for(&self, size: usize) -> usize {
        let header = core::mem::size_of::<AllocHeader>();
        // Total bytes the user region + header occupy. The header sits at the
        // page start; the user pointer follows it.
        let raw = size + header;
        // Round up to whole pages.
        let mut pages = raw.div_ceil(PAGE_SIZE).max(1);
        // size-0 / page-multiple bump (memory-safety.md §A3): if the user data
        // would end exactly on a page boundary (so a +1 overrun is the first
        // faulting byte but the header could be clobbered for size 0), add a
        // slack page so there is always a writable, header-distinct user region.
        // size == 0: `raw == header`, fits in one page, but the user pointer
        // (just past the header) is valid and writable for 0 bytes — still give
        // a page so the pointer is distinct and decommit works.
        if size == 0 || size.is_multiple_of(PAGE_SIZE) {
            pages += 1;
        }
        pages
    }

    /// Allocate `size` bytes. Returns `None` only if the VM fails to reserve or
    /// commit. The user pointer is front-aligned just past the header.
    pub fn alloc(&mut self, size: usize, site_id: u32) -> Option<Allocation> {
        let num_pages = self.pages_for(size);

        // Find a range with room, or reserve a new one.
        let (range_idx, page_idx) = match self.find_room(num_pages) {
            Some(loc) => loc,
            None => {
                self.reserve_range(num_pages)?;
                // The freshly reserved range is last; allocate at its start.
                let idx = self.ranges.len() - 1;
                (idx, 0)
            }
        };

        let range = &mut self.ranges[range_idx];
        let region_base = range.base + page_idx * PAGE_SIZE;
        // Commit the pages read-write.
        if !self
            .vm
            .commit(region_base as *mut u8, num_pages * PAGE_SIZE, PROT_READWRITE)
        {
            return None;
        }
        for p in page_idx..page_idx + num_pages {
            range.committed[p] = true;
        }
        // Advance the cursor past these pages — never reused (quarantine).
        if page_idx == range.next_page {
            range.next_page += num_pages;
        }

        let generation = self.next_gen;
        self.next_gen = self.next_gen.wrapping_add(1);

        // Write the header at the page start. SAFETY: the region is committed
        // read-write and `num_pages * PAGE >= header`.
        let header = AllocHeader {
            num_pages: num_pages as u32,
            magic: HEADER_MAGIC,
            site_id,
            generation,
        };
        unsafe {
            core::ptr::write(region_base as *mut AllocHeader, header);
        }

        let user_ptr = region_base + core::mem::size_of::<AllocHeader>();
        Some(Allocation {
            user_ptr,
            base: region_base,
            num_pages,
        })
    }

    /// Decommit (quarantine) `num_pages` at `base`. The caller has already
    /// validated the free via the ledger; this only does the VM op and clears
    /// the committed bits. The address range is **never released** — a later
    /// access faults. Returns `true` on success.
    pub fn quarantine(&mut self, base: usize, num_pages: usize) -> bool {
        let ok = self.vm.decommit(base as *mut u8, num_pages * PAGE_SIZE);
        // Clear the committed bits so accounting (committed_pages) is accurate
        // even though the pages stay reserved.
        for range in &mut self.ranges {
            if base >= range.base && base < range.base + range.total_pages * PAGE_SIZE {
                let start = (base - range.base) / PAGE_SIZE;
                for p in start..start + num_pages {
                    if p < range.committed.len() {
                        range.committed[p] = false;
                    }
                }
                break;
            }
        }
        ok
    }

    /// Release every reserved range back to the OS and forget them. Used by
    /// `reset` between corpus programs to bound address-space growth. After
    /// this the allocator behaves as freshly constructed.
    pub fn release_all(&mut self) {
        for range in self.ranges.drain(..) {
            self.vm
                .release(range.base as *mut u8, range.total_pages * PAGE_SIZE);
        }
        self.next_gen = 1;
    }

    /// Find a range with `num_pages` contiguous free pages at its cursor. We
    /// only ever allocate at the cursor (no in-range free-list — quarantine
    /// means freed pages are abandoned), so a range fits iff its remaining
    /// tail is large enough.
    fn find_room(&self, num_pages: usize) -> Option<(usize, usize)> {
        for (i, r) in self.ranges.iter().enumerate() {
            if r.next_page + num_pages <= r.total_pages {
                return Some((i, r.next_page));
            }
        }
        None
    }

    /// Reserve a new range big enough for at least `num_pages` (and the default
    /// `range_pages`). Returns `Some(())` on success.
    fn reserve_range(&mut self, num_pages: usize) -> Option<()> {
        let total_pages = self.range_pages.max(num_pages);
        let base = self.vm.reserve(total_pages * PAGE_SIZE);
        if base.is_null() {
            return None;
        }
        self.ranges.push(Range {
            base: base as usize,
            total_pages,
            next_page: 0,
            committed: vec![false; total_pages],
        });
        Some(())
    }
}
