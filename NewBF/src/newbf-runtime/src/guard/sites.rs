//! MS-T7: the heap-allocation **site table** registry + resolver
//! (memory-safety.md §A7).
//!
//! Each guarded allocation carries a `site_id` (the third `newbf_alloc` arg) —
//! an index into a per-module table the backend emits as the `__newbf_alloc_sites`
//! global. The table is **in the compiled module** (JIT'd or AOT'd Beef code),
//! while the guard's abort / leak report lives **here, in the runtime**. The seam
//! is [`register_alloc_sites`]: a host (the run harness, the driver, the
//! guard_runner) calls it ONCE after the module is JIT'd/loaded, passing the
//! address + count of `__newbf_alloc_sites`. The guard then resolves a `site_id`
//! to `"<function> @ file:line"` for any UAF / double-free / leak report.
//!
//! ## Layout contract (must match the backend emission)
//! `newbf-llvm`'s `emit_alloc_sites` emits `__newbf_alloc_sites` as a constant
//! `[N x %struct.AllocSite]` where `%struct.AllocSite = { ptr function, ptr file,
//! i32 line }`. [`AllocSiteRaw`] is the `#[repr(C)]` mirror of one entry; the
//! `char8*` strings are NUL-terminated. The table lives in the module's constant
//! data, which the host keeps mapped for the run's lifetime (the JIT is leaked /
//! the AOT image stays loaded), so the borrowed pointers stay valid.
//!
//! ## Lock-free
//! Registration is a single store (once at startup); resolution is a load. Two
//! relaxed atomics (the base pointer + the count) suffice — no `Mutex`, so the
//! abort path (already lock-free) can resolve a site without taking a lock.
//! Release builds simply never register a table (the backend omits it), so
//! resolution returns `None` and reports fall back to the bare address.

use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

/// One entry of the emitted `__newbf_alloc_sites` table — the `#[repr(C)]` mirror
/// of `%struct.AllocSite = { ptr function, ptr file, i32 line }`. Field order +
/// types are the binding contract with `newbf-llvm`'s `emit_alloc_sites`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AllocSiteRaw {
    /// NUL-terminated enclosing-function name (`char8*`).
    pub function: *const u8,
    /// NUL-terminated source file name (`char8*`); may point at an empty string.
    pub file: *const u8,
    /// 1-based source line of the allocating expression.
    pub line: i32,
}

// The registered table: a base pointer + entry count. Both relaxed atomics — set
// once by `register_alloc_sites`, read on the (rare) abort / leak path. The
// pointer is `*const AllocSiteRaw` cast to `*mut` for `AtomicPtr` (we only ever
// read through it). Null base / zero count ⇒ no table registered (release, or
// before registration) ⇒ every resolve is `None`.
static TABLE: AtomicPtr<AllocSiteRaw> = AtomicPtr::new(core::ptr::null_mut());
static COUNT: AtomicUsize = AtomicUsize::new(0);

/// Register the module's `__newbf_alloc_sites` table (base pointer + entry
/// count) with the guard, so a fault / leak report can resolve a `site_id` to
/// `"<function> @ file:line"` (memory-safety.md §A7). Idempotent: a later call
/// replaces the table (the run harness re-registers per program). A null `ptr`
/// or `count == 0` clears the table (resolution then returns `None`).
///
/// # Safety
/// `ptr` must point to `count` valid [`AllocSiteRaw`] entries whose `function` /
/// `file` `char8*` fields are NUL-terminated and stay valid for as long as any
/// report may run (the host keeps the JIT'd/AOT'd module mapped). Passing a
/// dangling pointer is undefined behavior on the next resolution.
pub unsafe fn register_alloc_sites(ptr: *const AllocSiteRaw, count: usize) {
    TABLE.store(ptr as *mut AllocSiteRaw, Ordering::Release);
    COUNT.store(count, Ordering::Release);
}

/// Resolve a `site_id` to its `(function, file, line)`, reading the registered
/// table. Returns `None` when no table is registered (release / pre-registration)
/// or `site_id` is out of range. Lock-free.
fn resolve_raw(site_id: u32) -> Option<(String, String, i32)> {
    let base = TABLE.load(Ordering::Acquire);
    let count = COUNT.load(Ordering::Acquire);
    if base.is_null() || (site_id as usize) >= count {
        return None;
    }
    // SAFETY: `site_id < count` and the registrant guaranteed `count` valid
    // entries with NUL-terminated `char8*` strings that outlive this read.
    unsafe {
        let entry = &*base.add(site_id as usize);
        let function = cstr_to_string(entry.function);
        let file = cstr_to_string(entry.file);
        Some((function, file, entry.line))
    }
}

/// Format a `site_id` as `"<function> @ file:line"` (or `"<function> @ :line"` /
/// `"<function>"` when the file/line are absent), e.g.
/// `"Program.Main @ uaf_after_delete.bf:4"`. Returns `None` when the site can't
/// be resolved (so the caller falls back to the bare address).
pub fn format_site(site_id: u32) -> Option<String> {
    let (function, file, line) = resolve_raw(site_id)?;
    Some(if file.is_empty() {
        if line > 0 {
            format!("{function} @ :{line}")
        } else {
            function
        }
    } else {
        format!("{function} @ {file}:{line}")
    })
}

/// Copy a NUL-terminated C string into an owned `String` (lossy UTF-8). An empty
/// or null pointer yields an empty string.
///
/// # Safety
/// `ptr` must be null or point to a NUL-terminated byte sequence.
unsafe fn cstr_to_string(ptr: *const u8) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: by contract `ptr` is NUL-terminated.
    let cstr = unsafe { std::ffi::CStr::from_ptr(ptr as *const std::os::raw::c_char) };
    cstr.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_a_registered_site_to_function_file_line() {
        // Build a one-entry table with static NUL-terminated strings.
        let func = b"Program.Main\0";
        let file = b"uaf_after_delete.bf\0";
        let table = [AllocSiteRaw {
            function: func.as_ptr(),
            file: file.as_ptr(),
            line: 4,
        }];
        // SAFETY: `table` outlives the resolve calls below (it lives to the end
        // of this test); strings are NUL-terminated.
        unsafe { register_alloc_sites(table.as_ptr(), table.len()) };
        assert_eq!(
            format_site(0).as_deref(),
            Some("Program.Main @ uaf_after_delete.bf:4")
        );
        // Out of range → None.
        assert_eq!(format_site(1), None);
        // Clear and confirm resolution stops.
        unsafe { register_alloc_sites(core::ptr::null(), 0) };
        assert_eq!(format_site(0), None);
    }

    #[test]
    fn empty_file_falls_back_to_function_and_line() {
        let func = b"$lambda0\0";
        let file = b"\0";
        let table = [AllocSiteRaw {
            function: func.as_ptr(),
            file: file.as_ptr(),
            line: 9,
        }];
        unsafe { register_alloc_sites(table.as_ptr(), table.len()) };
        assert_eq!(format_site(0).as_deref(), Some("$lambda0 @ :9"));
        unsafe { register_alloc_sites(core::ptr::null(), 0) };
    }
}
