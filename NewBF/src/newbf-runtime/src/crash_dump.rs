//! Signal-safe Win64 crash-dump handler.
//!
//! NewBF is a Windows app that crashes while it's being built (MANIFESTO
//! core decision 16), so a fault should print a useful dump rather than die
//! silently. Modeled on NewOpenDylan's `nod-runtime/crash_dump.rs` — the
//! portfolio's battle-tested handler — but the domain-state section reports
//! the **manual-memory guard** (live allocations, last free site for
//! use-after-free correlation) instead of GC metrics, since NewBF has no GC.
//!
//! ## Signal-safety
//! These handlers can fire mid-allocation or with the heap corrupted, so
//! they take **no lock and allocate nothing**: a fixed [`StackBuf`] formats
//! the text and raw `WriteFile`/`GetStdHandle` (or `write(2)`) push it to
//! stderr, bypassing `std`'s buffered, lock-taking I/O.
//!
//! ## Layering (Windows)
//!   - a Rust **panic hook** (chains the previous hook);
//!   - `SetUnhandledExceptionFilter` — last-chance, for access violations,
//!     `trap`/`ud2` (illegal instruction), int divide-by-zero, etc. Returns
//!     `CONTINUE_SEARCH` so WER / a JIT debugger still gets a turn;
//!   - a first-chance **VEH for stack overflow** plus
//!     `SetThreadStackGuarantee`, because `STATUS_STACK_OVERFLOW` leaves no
//!     stack for the unhandled filter to run on (NOD's GAP-010: otherwise the
//!     process dies silently).
//!
//! JIT'd code's `.pdata` is registered with `RtlAddFunctionTable` (see
//! `newbf-llvm/jit_mm.rs`), so a later symbolicating `StackWalk64` layer can
//! walk through JIT frames; this first cut reports fault + domain state.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

// ──────────────────────────────────────────────────────────────────── //
// Memory-guard shadow state                                            //
// ──────────────────────────────────────────────────────────────────── //
//
// The debug memory guard (stomp allocator, Sprints 09–11) publishes its
// live counts here so a crash dump can show heap state without touching the
// (possibly corrupt) allocator. Until then these read zero and the dump
// prints "(memory guard not installed)".

static GUARD_INSTALLED: AtomicBool = AtomicBool::new(false);
static LIVE_ALLOCS: AtomicU64 = AtomicU64::new(0);
static LIVE_BYTES: AtomicU64 = AtomicU64::new(0);
static TOTAL_ALLOCS: AtomicU64 = AtomicU64::new(0);
static TOTAL_FREES: AtomicU64 = AtomicU64::new(0);
/// Address of the most recent free, for use-after-free correlation hints.
static LAST_FREE_ADDR: AtomicU64 = AtomicU64::new(0);

/// Called once by the memory guard at startup so the dump can distinguish
/// "no allocations yet" from "guard not present".
pub fn note_memory_guard_installed() {
    GUARD_INSTALLED.store(true, Ordering::Relaxed);
}

/// Publish current heap counts (cheap relaxed stores; call from the guard's
/// alloc/free fast path or periodically).
pub fn update_guard_metrics(
    live_allocs: u64,
    live_bytes: u64,
    total_allocs: u64,
    total_frees: u64,
) {
    LIVE_ALLOCS.store(live_allocs, Ordering::Relaxed);
    LIVE_BYTES.store(live_bytes, Ordering::Relaxed);
    TOTAL_ALLOCS.store(total_allocs, Ordering::Relaxed);
    TOTAL_FREES.store(total_frees, Ordering::Relaxed);
}

/// Record the most recent freed address (use-after-free correlation hook).
pub fn note_free(addr: usize) {
    LAST_FREE_ADDR.store(addr as u64, Ordering::Relaxed);
}

// ──────────────────────────────────────────────────────────────────── //
// No-alloc stack formatter                                             //
// ──────────────────────────────────────────────────────────────────── //

/// Fixed-capacity buffer implementing `fmt::Write` with no heap. Excess
/// bytes beyond `N` are dropped.
struct StackBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> StackBuf<N> {
    const fn new() -> Self {
        Self {
            buf: [0u8; N],
            len: 0,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    fn as_str(&self) -> &str {
        // SAFETY: only ever written from `&str` slices → always valid UTF-8.
        unsafe { core::str::from_utf8_unchecked(self.as_bytes()) }
    }
}

impl<const N: usize> core::fmt::Write for StackBuf<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let to_copy = bytes.len().min(N - self.len);
        self.buf[self.len..self.len + to_copy].copy_from_slice(&bytes[..to_copy]);
        self.len += to_copy;
        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────── //
// Dump writer                                                          //
// ──────────────────────────────────────────────────────────────────── //

/// A consistent read of the memory-guard counters, or `None` when the guard
/// is not installed.
#[derive(Clone, Copy)]
struct GuardSnapshot {
    live_allocs: u64,
    live_bytes: u64,
    total_allocs: u64,
    total_frees: u64,
    last_free: u64,
}

fn guard_snapshot() -> Option<GuardSnapshot> {
    if !GUARD_INSTALLED.load(Ordering::Relaxed) {
        return None;
    }
    Some(GuardSnapshot {
        live_allocs: LIVE_ALLOCS.load(Ordering::Relaxed),
        live_bytes: LIVE_BYTES.load(Ordering::Relaxed),
        total_allocs: TOTAL_ALLOCS.load(Ordering::Relaxed),
        total_frees: TOTAL_FREES.load(Ordering::Relaxed),
        last_free: LAST_FREE_ADDR.load(Ordering::Relaxed),
    })
}

/// Format the dump body into `buf`. Pure (no I/O, no globals) so it is
/// deterministically unit-testable; the live path passes [`guard_snapshot`].
fn format_dump<const N: usize>(
    buf: &mut StackBuf<N>,
    exception_info: &str,
    guard: Option<GuardSnapshot>,
) {
    use core::fmt::Write as _;

    let _ = write!(
        buf,
        "\n\
         ============================================================\n\
         === NEWBF CRASH DUMP =======================================\n\
         ============================================================\n"
    );
    if !exception_info.is_empty() {
        let _ = writeln!(buf, "  exception            : {exception_info}");
    }
    let build = if cfg!(debug_assertions) {
        "debug (memory guard active)"
    } else {
        "release (memory guard stripped)"
    };
    let _ = writeln!(buf, "  build                : {build}");
    let _ = write!(
        buf,
        "------------------------------------------------------------\n\
         MEMORY GUARD\n"
    );
    match guard {
        Some(g) => {
            let _ = write!(
                buf,
                "  live allocations     : {}\n\
                   live bytes           : {}\n\
                   total allocations    : {}\n\
                   total frees          : {}\n\
                   last freed address   : {:#018x}\n",
                g.live_allocs, g.live_bytes, g.total_allocs, g.total_frees, g.last_free,
            );
        }
        None => {
            let _ = writeln!(buf, "  (memory guard not installed)");
        }
    }
    let _ = write!(
        buf,
        "============================================================\n\n"
    );
}

/// Write the crash dump to stderr. Signal-safe: no heap, no locks.
fn write_crash_dump(exception_info: &str) {
    let mut buf = StackBuf::<4096>::new();
    format_dump(&mut buf, exception_info, guard_snapshot());
    write_bytes_to_stderr(buf.as_bytes());
}

#[cfg(windows)]
fn write_bytes_to_stderr(bytes: &[u8]) {
    // Stable kernel32 exports; declared directly to avoid a windows-sys dep.
    unsafe extern "system" {
        fn GetStdHandle(nStdHandle: u32) -> *mut core::ffi::c_void;
        fn WriteFile(
            hFile: *mut core::ffi::c_void,
            lpBuffer: *const core::ffi::c_void,
            nNumberOfBytesToWrite: u32,
            lpNumberOfBytesWritten: *mut u32,
            lpOverlapped: *mut core::ffi::c_void,
        ) -> i32;
    }
    const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF4; // (DWORD)-12
    if bytes.is_empty() {
        return;
    }
    unsafe {
        let handle = GetStdHandle(STD_ERROR_HANDLE);
        if handle.is_null() || handle as isize == -1 {
            return;
        }
        let mut written = 0u32;
        WriteFile(
            handle,
            bytes.as_ptr().cast(),
            bytes.len().min(u32::MAX as usize) as u32,
            &mut written,
            core::ptr::null_mut(),
        );
    }
}

#[cfg(not(windows))]
fn write_bytes_to_stderr(bytes: &[u8]) {
    use std::io::Write as _;
    // NewBF is Windows-first; this best-effort path is not signal-safe but
    // keeps the crate buildable on other hosts for tests.
    let _ = std::io::stderr().write_all(bytes);
}

// ──────────────────────────────────────────────────────────────────── //
// Installation                                                         //
// ──────────────────────────────────────────────────────────────────── //

/// Install the crash-dump handlers. Idempotent (installs at most once per
/// process). Call early from runtime init / the JIT bootstrap.
pub fn install_crash_handler() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        install_panic_hook();
        #[cfg(windows)]
        {
            install_seh_filter();
            install_stack_overflow_handler();
        }
    });
}

/// C-ABI entry so JIT'd/AOT'd bootstrap code can install the handler.
#[unsafe(no_mangle)]
pub extern "C" fn newbf_install_crash_handler() {
    install_crash_handler();
}

fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut ctx = StackBuf::<512>::new();
        use core::fmt::Write as _;
        match info.location() {
            Some(loc) => {
                let _ = write!(
                    ctx,
                    "Rust panic at {}:{}:{}",
                    loc.file(),
                    loc.line(),
                    loc.column()
                );
            }
            None => {
                let _ = write!(ctx, "Rust panic (no location)");
            }
        }
        write_crash_dump(ctx.as_str());
        #[cfg(windows)]
        write_backtrace_to_stderr();
        prev(info);
    }));
}

// ── Windows SEH ───────────────────────────────────────────────────── //

#[cfg(windows)]
#[repr(C)]
struct ExceptionRecord {
    exception_code: u32,
    exception_flags: u32,
    exception_record_chain: *mut ExceptionRecord,
    exception_address: *mut core::ffi::c_void,
    // NumberParameters + ExceptionInformation[15] follow; unused here.
}

#[cfg(windows)]
#[repr(C)]
struct ExceptionPointers {
    exception_record: *mut ExceptionRecord,
    context_record: *mut core::ffi::c_void,
}

#[cfg(windows)]
fn install_seh_filter() {
    unsafe extern "system" {
        fn SetUnhandledExceptionFilter(handler: *const core::ffi::c_void)
        -> *mut core::ffi::c_void;
    }
    unsafe {
        SetUnhandledExceptionFilter(
            unhandled_exception_filter as unsafe extern "system" fn(*mut ExceptionPointers) -> i32
                as *const core::ffi::c_void,
        );
    }
}

#[cfg(windows)]
unsafe extern "system" fn unhandled_exception_filter(info: *mut ExceptionPointers) -> i32 {
    use core::fmt::Write as _;
    let mut ctx = StackBuf::<256>::new();
    if !info.is_null() {
        let rec = unsafe { (*info).exception_record };
        if !rec.is_null() {
            let code = unsafe { (*rec).exception_code };
            let addr = unsafe { (*rec).exception_address };
            let _ = write!(
                ctx,
                "{} (code {code:#010x}) at {addr:p}",
                exception_code_name(code)
            );
        }
    }
    write_crash_dump(ctx.as_str());
    write_backtrace_to_stderr();
    // EXCEPTION_CONTINUE_SEARCH: let WER / a JIT debugger still fire.
    0
}

/// Make stack overflows reportable rather than silent: reserve guaranteed
/// post-guard-page stack so a handler can run, then catch first-chance.
#[cfg(windows)]
fn install_stack_overflow_handler() {
    ensure_stack_overflow_reserve_this_thread();
    unsafe extern "system" {
        fn AddVectoredExceptionHandler(
            first: u32,
            handler: *const core::ffi::c_void,
        ) -> *mut core::ffi::c_void;
    }
    unsafe {
        AddVectoredExceptionHandler(
            1, // run first
            vectored_stack_overflow_handler
                as unsafe extern "system" fn(*mut ExceptionPointers) -> i32
                as *const core::ffi::c_void,
        );
    }
}

/// Reserve enough stack on the current thread for a handler to run after a
/// stack-overflow guard-page fault. Idempotent; call from every thread that
/// runs NewBF/JIT'd code.
#[cfg(windows)]
pub fn ensure_stack_overflow_reserve_this_thread() {
    unsafe extern "system" {
        fn SetThreadStackGuarantee(stack_size_in_bytes: *mut u32) -> i32;
    }
    let mut guarantee: u32 = 64 * 1024;
    unsafe {
        SetThreadStackGuarantee(&mut guarantee);
    }
}

#[cfg(windows)]
unsafe extern "system" fn vectored_stack_overflow_handler(info: *mut ExceptionPointers) -> i32 {
    const EXCEPTION_CONTINUE_SEARCH: i32 = 0;
    const STATUS_STACK_OVERFLOW: u32 = 0xC000_00FD;
    if !info.is_null() {
        let rec = unsafe { (*info).exception_record };
        if !rec.is_null() && unsafe { (*rec).exception_code } == STATUS_STACK_OVERFLOW {
            let addr = unsafe { (*rec).exception_address };
            let mut ctx = StackBuf::<256>::new();
            use core::fmt::Write as _;
            let _ = write!(
                ctx,
                "EXCEPTION_STACK_OVERFLOW (code 0xc00000fd) at {addr:p}"
            );
            write_crash_dump(ctx.as_str());
        }
    }
    // Never swallow — let normal dispatch proceed.
    EXCEPTION_CONTINUE_SEARCH
}

#[cfg(windows)]
fn exception_code_name(code: u32) -> &'static str {
    match code {
        0xC000_0005 => "EXCEPTION_ACCESS_VIOLATION",
        0xC000_0006 => "EXCEPTION_IN_PAGE_ERROR",
        0x8000_0003 => "EXCEPTION_BREAKPOINT", // debugtrap / int3
        0x8000_0004 => "EXCEPTION_SINGLE_STEP",
        0xC000_001D => "EXCEPTION_ILLEGAL_INSTRUCTION", // trap / ud2
        0xC000_0025 => "EXCEPTION_NONCONTINUABLE_EXCEPTION",
        0xC000_008C => "EXCEPTION_ARRAY_BOUNDS_EXCEEDED",
        0xC000_008E => "EXCEPTION_FLT_DIVIDE_BY_ZERO",
        0xC000_0090 => "EXCEPTION_FLT_INVALID_OPERATION",
        0xC000_0091 => "EXCEPTION_FLT_OVERFLOW",
        0xC000_0094 => "EXCEPTION_INT_DIVIDE_BY_ZERO",
        0xC000_0095 => "EXCEPTION_INT_OVERFLOW",
        0xC000_0096 => "EXCEPTION_PRIV_INSTRUCTION",
        0xC000_00FD => "EXCEPTION_STACK_OVERFLOW",
        _ => "EXCEPTION_UNKNOWN",
    }
}

// ──────────────────────────────────────────────────────────────────── //
// Symbolicated backtrace (Windows, DbgHelp)                            //
// ──────────────────────────────────────────────────────────────────── //
//
// Frames-with-names turn "died at 0x7ff6…" into "died in foo::bar+0x2c" — the
// difference between an afternoon and a glance. `RtlCaptureStackBackTrace`
// grabs the current stack's return addresses (one call — far simpler than the
// full `StackWalk64`/`STACKFRAME64` dance) and DbgHelp `SymFromAddr` names
// them. DbgHelp is NOT signal-safe (it allocates, locks, and loads PDBs), so
// this runs only from the panic hook and the *last-chance* SEH filter — where
// the process is already terminating — never the first-chance stack-overflow
// VEH, which has almost no stack. Best-effort: any failure just drops a name.

#[cfg(windows)]
mod bt {
    use super::StackBuf;
    use core::ffi::{CStr, c_char, c_void};
    use std::sync::Once;

    const MAX_NAME: usize = 1024;
    const SYMOPT_UNDNAME: u32 = 0x0000_0002;
    const SYMOPT_LOAD_LINES: u32 = 0x0000_0010;

    /// Mirrors `dbghelp.h`'s `SYMBOL_INFO`. `#[repr(C)]` reproduces its
    /// padding; the `Name` flexible array is over-allocated via [`SymBuf`].
    #[repr(C)]
    struct SymbolInfo {
        size_of_struct: u32,
        type_index: u32,
        reserved: [u64; 2],
        index: u32,
        size: u32,
        mod_base: u64,
        flags: u32,
        value: u64,
        address: u64,
        register: u32,
        scope: u32,
        tag: u32,
        name_len: u32,
        max_name_len: u32,
        name: [c_char; 1],
    }

    /// `SYMBOL_INFO` + a trailing name buffer, stack-allocated (no heap in the
    /// handler). `name` abuts `name_tail`, giving `MAX_NAME` bytes for the name.
    #[repr(C)]
    struct SymBuf {
        info: SymbolInfo,
        name_tail: [c_char; MAX_NAME],
    }

    unsafe extern "system" {
        fn RtlCaptureStackBackTrace(
            frames_to_skip: u32,
            frames_to_capture: u32,
            back_trace: *mut *mut c_void,
            back_trace_hash: *mut u32,
        ) -> u16;
        fn GetCurrentProcess() -> *mut c_void;
        fn GetModuleHandleW(name: *const u16) -> *mut c_void;
        fn GetModuleFileNameA(module: *mut c_void, filename: *mut u8, size: u32) -> u32;
    }

    #[link(name = "dbghelp")]
    unsafe extern "system" {
        fn SymInitialize(process: *mut c_void, search_path: *const c_char, invade: i32) -> i32;
        fn SymRefreshModuleList(process: *mut c_void) -> i32;
        fn SymSetOptions(options: u32) -> u32;
        fn SymLoadModuleEx(
            process: *mut c_void,
            file: *mut c_void,
            image_name: *const c_char,
            module_name: *const c_char,
            base: u64,
            size: u32,
            data: *const c_void,
            flags: u32,
        ) -> u64;
        fn SymFromAddr(
            process: *mut c_void,
            address: u64,
            displacement: *mut u64,
            symbol: *mut SymbolInfo,
        ) -> i32;
    }

    fn ensure_sym_init() -> *mut c_void {
        static ONCE: Once = Once::new();
        let process = unsafe { GetCurrentProcess() };
        ONCE.call_once(|| unsafe {
            // Not deferred: load module symbols at init (`invade = TRUE`) so
            // our own PDB resolves. Without a symbol-server `_NT_SYMBOL_PATH`
            // this only touches local PDBs, so it stays fast.
            SymSetOptions(SYMOPT_UNDNAME | SYMOPT_LOAD_LINES);
            SymInitialize(process, core::ptr::null(), 1);
            SymRefreshModuleList(process);
            // `invade`/refresh register the system DLLs but routinely skip the
            // main exe, leaving our own frames unresolved (SymFromAddr → 487
            // ERROR_INVALID_ADDRESS). Register it explicitly: its base is the
            // exe HMODULE; SymLoadModuleEx reads the PDB path from the image.
            let base = GetModuleHandleW(core::ptr::null());
            let mut path = [0u8; 260];
            let n = GetModuleFileNameA(base, path.as_mut_ptr(), path.len() as u32);
            if !base.is_null() && n > 0 {
                SymLoadModuleEx(
                    process,
                    core::ptr::null_mut(),
                    path.as_ptr() as *const c_char,
                    core::ptr::null(),
                    base as usize as u64,
                    0,
                    core::ptr::null(),
                    0,
                );
            }
        });
        process
    }

    /// Append a symbolicated backtrace of the current stack to `buf`. `skip`
    /// drops that many innermost frames in addition to this function's own.
    ///
    /// Frame naming is best-effort: DLL-export frames (system functions) name
    /// reliably; naming our *own* frames additionally needs DbgHelp to load
    /// the image's PDB, which the in-box `dbghelp.dll` doesn't always manage
    /// (returns `ERROR_INVALID_ADDRESS`) — richer coverage wants a newer SDK
    /// `dbghelp.dll` shipped alongside, a follow-on. Unresolved frames print
    /// the raw address, still mappable via the linker `.map` / a debugger.
    pub(super) fn append_backtrace<const N: usize>(buf: &mut StackBuf<N>, skip: u32) {
        use core::fmt::Write as _;
        let process = ensure_sym_init();

        let mut frames: [*mut c_void; 62] = [core::ptr::null_mut(); 62];
        let n = unsafe {
            RtlCaptureStackBackTrace(skip + 1, 62, frames.as_mut_ptr(), core::ptr::null_mut())
        } as usize;
        if n == 0 {
            return;
        }
        let _ = write!(
            buf,
            "------------------------------------------------------------\n\
             BACKTRACE  ({n} frames)\n"
        );
        for (i, &addr) in frames.iter().take(n).enumerate() {
            let pc = addr as usize as u64;
            let mut sb: SymBuf = unsafe { core::mem::zeroed() };
            sb.info.size_of_struct = core::mem::size_of::<SymbolInfo>() as u32;
            sb.info.max_name_len = MAX_NAME as u32;
            let mut disp: u64 = 0;
            let ok = unsafe { SymFromAddr(process, pc, &mut disp, &mut sb.info) };
            if ok != 0 {
                let name = unsafe { CStr::from_ptr(sb.info.name.as_ptr()) }
                    .to_str()
                    .unwrap_or("<non-utf8>");
                let _ = writeln!(buf, "  #{i:<2} {name}+{disp:#x}  [{pc:#018x}]");
            } else {
                let _ = writeln!(buf, "  #{i:<2} [{pc:#018x}]  <no symbol>");
            }
        }
    }
}

/// Append a symbolicated backtrace of the current stack to stderr (its own
/// flush so the main dump survives even if symbolication wedges).
#[cfg(windows)]
fn write_backtrace_to_stderr() {
    let mut buf = StackBuf::<8192>::new();
    bt::append_backtrace(&mut buf, 1); // skip this helper's frame
    write_bytes_to_stderr(buf.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::fmt::Write as _;

    #[test]
    fn stackbuf_truncates_and_roundtrips() {
        let mut b = StackBuf::<8>::new();
        let _ = write!(b, "abcdefghIJKL"); // 12 bytes into 8
        assert_eq!(b.as_str(), "abcdefgh");
    }

    /// The backtrace resolves frames to names. The debug test binary has a
    /// PDB, so SymFromAddr names this crate's frames; their (mangled) symbols
    /// embed the identifiers, so the test fn / crate name appears.
    #[cfg(windows)]
    #[test]
    fn backtrace_resolves_named_frames() {
        let mut b = StackBuf::<8192>::new();
        super::bt::append_backtrace(&mut b, 0);
        let s = b.as_str();
        assert!(s.contains("BACKTRACE"), "no backtrace header:\n{s}");
        // The thread-root frames (ntdll / kernel32 exports) reliably
        // symbolicate, proving the capture → SymFromAddr → name path works end
        // to end. (Naming our own Rust frames also needs DbgHelp to load the
        // exe's PDB — see append_backtrace's note.)
        assert!(
            s.contains("RtlUserThreadStart") || s.contains("BaseThreadInitThunk"),
            "no frame resolved to a name:\n{s}"
        );
    }

    #[test]
    fn dump_reports_no_guard() {
        let mut b = StackBuf::<4096>::new();
        format_dump(
            &mut b,
            "EXCEPTION_ILLEGAL_INSTRUCTION (code 0xc000001d) at 0x140001234",
            None,
        );
        let s = b.as_str();
        assert!(s.contains("NEWBF CRASH DUMP"), "{s}");
        assert!(s.contains("EXCEPTION_ILLEGAL_INSTRUCTION"), "{s}");
        assert!(s.contains("MEMORY GUARD"), "{s}");
        assert!(s.contains("(memory guard not installed)"), "{s}");
    }

    #[test]
    fn dump_reports_guard_metrics() {
        let g = GuardSnapshot {
            live_allocs: 3,
            live_bytes: 4096,
            total_allocs: 10,
            total_frees: 7,
            last_free: 0xDEAD_BEEF,
        };
        let mut b = StackBuf::<4096>::new();
        format_dump(&mut b, "", Some(g));
        let s = b.as_str();
        assert!(s.contains("live allocations     : 3"), "{s}");
        assert!(s.contains("total frees          : 7"), "{s}");
        assert!(s.contains("deadbeef"), "{s}");
    }

    #[test]
    fn guard_shadow_publishes() {
        // The public publish API feeds guard_snapshot(); exercise it without
        // depending on cross-test ordering of the install flag.
        note_memory_guard_installed();
        update_guard_metrics(1, 64, 2, 1);
        note_free(0x1000);
        let snap = guard_snapshot().expect("installed");
        assert_eq!(snap.live_allocs, 1);
        assert_eq!(snap.total_allocs, 2);
    }

    #[cfg(windows)]
    #[test]
    fn exception_names_known() {
        assert_eq!(
            exception_code_name(0xC000_0005),
            "EXCEPTION_ACCESS_VIOLATION"
        );
        assert_eq!(
            exception_code_name(0xC000_001D),
            "EXCEPTION_ILLEGAL_INSTRUCTION"
        );
        assert_eq!(exception_code_name(0x8000_0003), "EXCEPTION_BREAKPOINT");
        assert_eq!(exception_code_name(0x1234), "EXCEPTION_UNKNOWN");
    }
}
