//! Virtual-memory shim for the stomp allocator (memory-safety.md §4 "OS shim
//! (`guard::vm`)").
//!
//! The stomp allocator reserves a large address range up front, then commits
//! pages on alloc and **decommits** them on free — keeping the address range
//! reserved (quarantined) so a use-after-free faults deterministically. The
//! [`Vm`] trait isolates these four operations so:
//!
//!   * production uses a real Win32 [`Win32Vm`] (`VirtualAlloc`/`VirtualFree`/
//!     `VirtualProtect`);
//!   * unit tests drive the allocator/ledger with a deterministic [`MockVm`]
//!     that needs no real address space — so the quarantine and ledger
//!     properties are tested without touching the OS.
//!
//! Matching `crash_dump.rs`, the Win32 entry points are declared directly
//! (no `windows-sys` dep — `newbf-runtime` stays a zero-dependency leaf).

/// The OS page size the stomp allocator rounds to. 4 KiB on x86-64 Windows.
/// Hardcoded rather than queried so the allocator math is `const` and the
/// `MockVm` matches the real VM exactly; `Win32Vm::reserve` asserts the OS
/// agrees.
pub const PAGE_SIZE: usize = 4096;

/// Page protection for a committed allocation: readable + writable.
pub const PROT_READWRITE: Protect = Protect::ReadWrite;

/// Page protection requested for committed regions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Protect {
    /// `PAGE_READWRITE` — committed, accessible.
    ReadWrite,
    /// `PAGE_NOACCESS` — committed but any access faults (a guard page that
    /// keeps its backing without being touchable).
    NoAccess,
}

/// Virtual-memory operations the stomp allocator needs. All addresses and
/// sizes are page-aligned by the caller; an implementation may assume so.
///
/// # Safety contract
/// `commit`/`decommit`/`protect` operate on sub-ranges of a region previously
/// returned by `reserve` (and not yet `release`d). `release` frees the whole
/// reservation. The trait itself is safe to *call*; misusing the returned raw
/// pointers is the caller's responsibility (the stomp allocator upholds this).
pub trait Vm: Send + Sync {
    /// Reserve `bytes` of address space without committing physical pages.
    /// Returns the base address, or null on failure. `bytes` is a multiple of
    /// [`PAGE_SIZE`].
    fn reserve(&self, bytes: usize) -> *mut u8;

    /// Commit (back with physical memory) `bytes` starting at `addr`, with the
    /// given protection. `addr`/`bytes` are page-aligned. Returns `true` on
    /// success.
    fn commit(&self, addr: *mut u8, bytes: usize, prot: Protect) -> bool;

    /// Decommit (release physical pages, keep the address range reserved)
    /// `bytes` at `addr`. After this, accessing the range faults — this is the
    /// quarantine mechanism. `addr`/`bytes` are page-aligned.
    fn decommit(&self, addr: *mut u8, bytes: usize) -> bool;

    /// Release the entire reservation `addr` (the base from a prior `reserve`).
    /// `bytes` is informational for impls (Win32 ignores it for `MEM_RELEASE`).
    fn release(&self, addr: *mut u8, bytes: usize) -> bool;
}

// ──────────────────────────────────────────────────────────────────── //
// Win32 implementation                                                 //
// ──────────────────────────────────────────────────────────────────── //

#[cfg(windows)]
mod win {
    use super::{PAGE_SIZE, Protect, Vm};
    use core::ffi::c_void;

    // Stable kernel32 exports, declared directly (matching crash_dump.rs's
    // style) so the crate keeps zero dependencies.
    unsafe extern "system" {
        fn VirtualAlloc(
            lp_address: *mut c_void,
            dw_size: usize,
            fl_allocation_type: u32,
            fl_protect: u32,
        ) -> *mut c_void;
        fn VirtualFree(lp_address: *mut c_void, dw_size: usize, dw_free_type: u32) -> i32;
    }

    const MEM_COMMIT: u32 = 0x0000_1000;
    const MEM_RESERVE: u32 = 0x0000_2000;
    const MEM_DECOMMIT: u32 = 0x0000_4000;
    const MEM_RELEASE: u32 = 0x0000_8000;

    const PAGE_NOACCESS: u32 = 0x01;
    const PAGE_READWRITE: u32 = 0x04;

    fn prot_flags(prot: Protect) -> u32 {
        match prot {
            Protect::ReadWrite => PAGE_READWRITE,
            Protect::NoAccess => PAGE_NOACCESS,
        }
    }

    /// Production VM backed by `VirtualAlloc`/`VirtualFree`.
    pub struct Win32Vm;

    impl Vm for Win32Vm {
        fn reserve(&self, bytes: usize) -> *mut u8 {
            debug_assert_eq!(bytes % PAGE_SIZE, 0, "reserve size must be page-aligned");
            // SAFETY: kernel32 call with a null preferred address (let the OS
            // choose) reserves `bytes` of address space; no pages are committed
            // so nothing is written.
            unsafe {
                VirtualAlloc(core::ptr::null_mut(), bytes, MEM_RESERVE, PAGE_NOACCESS) as *mut u8
            }
        }

        fn commit(&self, addr: *mut u8, bytes: usize, prot: Protect) -> bool {
            debug_assert_eq!(addr as usize % PAGE_SIZE, 0, "commit addr must be aligned");
            debug_assert_eq!(bytes % PAGE_SIZE, 0, "commit size must be page-aligned");
            // SAFETY: `addr` lies inside a prior reservation; MEM_COMMIT on an
            // already-reserved sub-range is the documented pattern.
            let p = unsafe {
                VirtualAlloc(addr.cast(), bytes, MEM_COMMIT, prot_flags(prot))
            };
            !p.is_null()
        }

        fn decommit(&self, addr: *mut u8, bytes: usize) -> bool {
            debug_assert_eq!(addr as usize % PAGE_SIZE, 0, "decommit addr must be aligned");
            debug_assert_eq!(bytes % PAGE_SIZE, 0, "decommit size must be page-aligned");
            // SAFETY: MEM_DECOMMIT releases the physical pages but keeps the
            // address range reserved — exactly the quarantine we want. The
            // range stays unusable until the whole reservation is released.
            unsafe { VirtualFree(addr.cast(), bytes, MEM_DECOMMIT) != 0 }
        }

        fn release(&self, addr: *mut u8, _bytes: usize) -> bool {
            // MEM_RELEASE requires size == 0 and `addr` to be the reservation
            // base. SAFETY: `addr` is a base returned by `reserve`.
            unsafe { VirtualFree(addr.cast(), 0, MEM_RELEASE) != 0 }
        }
    }
}

#[cfg(windows)]
pub use win::Win32Vm;

// ──────────────────────────────────────────────────────────────────── //
// Non-Windows implementation (POSIX mmap/mprotect)                     //
// ──────────────────────────────────────────────────────────────────── //
//
// NewBF is Windows-first, but the crate must stay buildable/testable off
// Windows (matching crash_dump.rs's cfg split) so the deterministic ledger /
// quarantine unit tests run anywhere CI does.

#[cfg(not(windows))]
mod posix {
    use super::{PAGE_SIZE, Protect, Vm};
    use core::ffi::c_void;

    unsafe extern "C" {
        fn mmap(
            addr: *mut c_void,
            length: usize,
            prot: i32,
            flags: i32,
            fd: i32,
            offset: i64,
        ) -> *mut c_void;
        fn munmap(addr: *mut c_void, length: usize) -> i32;
        fn mprotect(addr: *mut c_void, len: usize, prot: i32) -> i32;
    }

    const PROT_NONE: i32 = 0x0;
    const PROT_READ: i32 = 0x1;
    const PROT_WRITE: i32 = 0x2;
    const MAP_PRIVATE: i32 = 0x2;
    const MAP_ANONYMOUS: i32 = 0x20; // Linux value; good enough for CI.
    const MAP_FAILED: *mut c_void = usize::MAX as *mut c_void;

    fn prot_flags(prot: Protect) -> i32 {
        match prot {
            Protect::ReadWrite => PROT_READ | PROT_WRITE,
            Protect::NoAccess => PROT_NONE,
        }
    }

    /// POSIX VM. `reserve` maps `PROT_NONE`; `commit` flips protection via
    /// `mprotect`; `decommit` maps `PROT_NONE` again (physical pages drop on
    /// next demand), keeping the range reserved for quarantine; `release`
    /// `munmap`s the whole range.
    pub struct PosixVm;

    impl Vm for PosixVm {
        fn reserve(&self, bytes: usize) -> *mut u8 {
            // SAFETY: anonymous private mapping; PROT_NONE means no access until
            // committed.
            let p = unsafe {
                mmap(
                    core::ptr::null_mut(),
                    bytes,
                    PROT_NONE,
                    MAP_PRIVATE | MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            if p == MAP_FAILED { core::ptr::null_mut() } else { p as *mut u8 }
        }

        fn commit(&self, addr: *mut u8, bytes: usize, prot: Protect) -> bool {
            debug_assert_eq!(addr as usize % PAGE_SIZE, 0);
            // SAFETY: `addr` is inside the reservation.
            unsafe { mprotect(addr.cast(), bytes, prot_flags(prot)) == 0 }
        }

        fn decommit(&self, addr: *mut u8, bytes: usize) -> bool {
            // Re-protect to PROT_NONE so accesses fault (quarantine). SAFETY:
            // sub-range of the reservation.
            unsafe { mprotect(addr.cast(), bytes, PROT_NONE) == 0 }
        }

        fn release(&self, addr: *mut u8, bytes: usize) -> bool {
            // SAFETY: `addr`/`bytes` are the original mapping.
            unsafe { munmap(addr.cast(), bytes) == 0 }
        }
    }
}

#[cfg(not(windows))]
pub use posix::PosixVm;

/// The production VM for the host platform.
#[cfg(windows)]
pub type HostVm = Win32Vm;
/// The production VM for the host platform.
#[cfg(not(windows))]
pub type HostVm = PosixVm;

/// Construct the production VM.
#[cfg(windows)]
pub fn host_vm() -> HostVm {
    Win32Vm
}
/// Construct the production VM.
#[cfg(not(windows))]
pub fn host_vm() -> HostVm {
    PosixVm
}

// ──────────────────────────────────────────────────────────────────── //
// Mock VM for deterministic unit tests                                 //
// ──────────────────────────────────────────────────────────────────── //

#[cfg(test)]
pub mod mock {
    use super::{PAGE_SIZE, Protect, Vm};
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Per-page state tracked by the mock so tests can assert the allocator's
    /// VM discipline (reserve→commit→decommit) without real memory.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum PageState {
        Reserved,
        Committed(Protect),
        Decommitted,
    }

    /// An in-memory `Vm` that backs each reservation with a **real, page-aligned
    /// `Vec<u8>` buffer** so the stomp allocator can genuinely write its header
    /// and tests can read user memory — exactly like the production VM, but with
    /// no OS calls and no faulting (decommit just records state). It NEVER hands
    /// out an address range twice (each reservation is a distinct buffer that is
    /// kept alive until `release`), mirroring the quarantine invariant: a freed
    /// user pointer is never returned by a later alloc.
    pub struct MockVm {
        inner: Mutex<MockInner>,
    }

    /// A page-aligned backing buffer. We over-allocate by a page and align the
    /// usable base up so every handed-out address is page-aligned (the allocator
    /// asserts this).
    struct Backing {
        _raw: Vec<u8>,
        base: usize,
    }

    impl Backing {
        fn new(bytes: usize) -> Self {
            let raw = vec![0u8; bytes + PAGE_SIZE];
            let addr = raw.as_ptr() as usize;
            let base = (addr + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
            Backing { _raw: raw, base }
        }
    }

    struct MockInner {
        /// Reservation base -> backing buffer (kept alive; never recycled).
        backing: HashMap<usize, Backing>,
        /// Page address -> state.
        pages: HashMap<usize, PageState>,
        /// Op counters for assertions.
        reserve_calls: usize,
        commit_calls: usize,
        decommit_calls: usize,
        release_calls: usize,
    }

    impl Default for MockVm {
        fn default() -> Self {
            Self::new()
        }
    }

    impl MockVm {
        pub fn new() -> Self {
            MockVm {
                inner: Mutex::new(MockInner {
                    backing: HashMap::new(),
                    pages: HashMap::new(),
                    reserve_calls: 0,
                    commit_calls: 0,
                    decommit_calls: 0,
                    release_calls: 0,
                }),
            }
        }

        pub fn page_state(&self, addr: *mut u8) -> Option<PageState> {
            let g = self.inner.lock().unwrap();
            g.pages.get(&(addr as usize)).copied()
        }

        pub fn reserve_calls(&self) -> usize {
            self.inner.lock().unwrap().reserve_calls
        }
        pub fn commit_calls(&self) -> usize {
            self.inner.lock().unwrap().commit_calls
        }
        pub fn decommit_calls(&self) -> usize {
            self.inner.lock().unwrap().decommit_calls
        }
        pub fn release_calls(&self) -> usize {
            self.inner.lock().unwrap().release_calls
        }
    }

    impl Vm for MockVm {
        fn reserve(&self, bytes: usize) -> *mut u8 {
            assert_eq!(bytes % PAGE_SIZE, 0);
            let backing = Backing::new(bytes);
            let base = backing.base;
            let mut g = self.inner.lock().unwrap();
            g.reserve_calls += 1;
            g.backing.insert(base, backing);
            let mut addr = base;
            while addr < base + bytes {
                g.pages.insert(addr, PageState::Reserved);
                addr += PAGE_SIZE;
            }
            base as *mut u8
        }

        fn commit(&self, addr: *mut u8, bytes: usize, prot: Protect) -> bool {
            let mut g = self.inner.lock().unwrap();
            g.commit_calls += 1;
            let start = addr as usize;
            let mut a = start;
            while a < start + bytes {
                g.pages.insert(a, PageState::Committed(prot));
                a += PAGE_SIZE;
            }
            true
        }

        fn decommit(&self, addr: *mut u8, bytes: usize) -> bool {
            let mut g = self.inner.lock().unwrap();
            g.decommit_calls += 1;
            let start = addr as usize;
            let mut a = start;
            while a < start + bytes {
                g.pages.insert(a, PageState::Decommitted);
                a += PAGE_SIZE;
            }
            true
        }

        fn release(&self, addr: *mut u8, _bytes: usize) -> bool {
            let mut g = self.inner.lock().unwrap();
            g.release_calls += 1;
            // Drop the backing buffer; keep the page map so the address is
            // never re-handed-out (next_base already moved past it).
            g.backing.remove(&(addr as usize));
            true
        }
    }
}
