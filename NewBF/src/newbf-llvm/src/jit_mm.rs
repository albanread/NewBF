//! Win64 SEH unwind-registration memory manager for the ORC JIT.
//!
//! Ported from NewM2 (`newm2-llvm/jit_mm.rs`, itself from NCL), adapted from
//! MCJIT's `LLVMCreateSimpleMCJITMemoryManager` to ORC's
//! `LLVMOrcCreateRTDyldObjectLinkingLayerWithMCJITMemoryManagerLikeCallbacks`.
//!
//! Why this exists
//! ---------------
//! LLVM emits `.pdata`/`.xdata` sections for every function with the
//! `uwtable` attribute. The Windows SEH unwinder doesn't scan memory for
//! them — it consults a registry. Statically-linked PE images are
//! auto-registered; JIT'd code in `VirtualAlloc`-ed memory is not. Without
//! registration, a Rust `panic!` (or any SEH exception) raised in a runtime
//! helper called from JIT'd code hits the JIT frame, finds no unwind info
//! known to the OS, and aborts the process. With it, the same exception
//! unwinds cleanly back to the `catch_unwind` boundary.
//!
//! ORC vs. MCJIT
//! -------------
//! RTDyldObjectLinkingLayer creates a *new* memory manager per linked object
//! by calling `CreateContext`. We follow the MCJIT-emulation recipe from the
//! LLVM C header: `CreateContext` returns the one shared context pointer (our
//! [`JitMm`]); per-object `destroy` is a no-op; `notify_terminating` frees the
//! [`JitMm`] when the layer is torn down. The allocate/finalize callbacks are
//! identical to MCJIT's — RTDyld passes our context as their `Opaque`.

#![allow(non_snake_case, non_camel_case_types, dead_code)]

use std::ffi::{CStr, c_char, c_void};
use std::sync::Mutex;

// ── Windows API shim ──────────────────────────────────────────────────

#[cfg(windows)]
mod win {
    use super::c_void;

    pub const MEM_COMMIT: u32 = 0x1000;
    pub const MEM_RESERVE: u32 = 0x2000;
    pub const PAGE_NOACCESS: u32 = 0x01;
    pub const PAGE_READWRITE: u32 = 0x04;
    pub const PAGE_EXECUTE_READ: u32 = 0x20;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct RUNTIME_FUNCTION {
        pub BeginAddress: u32,
        pub EndAddress: u32,
        pub UnwindData: u32,
    }

    unsafe extern "system" {
        pub fn VirtualAlloc(
            lpAddress: *mut c_void,
            dwSize: usize,
            flAllocationType: u32,
            flProtect: u32,
        ) -> *mut c_void;
        pub fn VirtualProtect(
            lpAddress: *mut c_void,
            dwSize: usize,
            flNewProtect: u32,
            lpflOldProtect: *mut u32,
        ) -> i32;
        pub fn RtlAddFunctionTable(
            FunctionTable: *const RUNTIME_FUNCTION,
            EntryCount: u32,
            BaseAddress: u64,
        ) -> u8;
    }
}

// ── Per-allocation record ─────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
struct Section {
    name_tag: NameTag,
    ptr: *mut u8,
    size: usize,
    is_code: bool,
}

unsafe impl Send for Section {}
unsafe impl Sync for Section {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NameTag {
    Text,
    Pdata,
    Xdata,
    Other,
}

impl NameTag {
    fn from_name(name: *const c_char) -> NameTag {
        if name.is_null() {
            return NameTag::Other;
        }
        let s = unsafe { CStr::from_ptr(name) };
        match s.to_bytes() {
            b".text" => NameTag::Text,
            b".pdata" => NameTag::Pdata,
            b".xdata" => NameTag::Xdata,
            _ => NameTag::Other,
        }
    }
}

// ── Per-engine memory manager state ───────────────────────────────────

const PAGE: usize = 4096;
/// One reservation per JIT'd module. Big enough for all sections, since
/// `IMAGE_REL_AMD64_ADDR32NB` relocations in `.pdata` reference `.text` and
/// `.xdata` as 32-bit RVAs — all sections must sit within 4 GiB of each
/// other. 4 MiB is plenty for any single module we emit.
const MODULE_RESERVE: usize = 4 * 1024 * 1024;

struct Bump {
    base: *mut u8,
    size: usize,
    used: usize,
}

unsafe impl Send for Bump {}

/// State carried as the `opaque` of the RTDyld MM callbacks — one per JIT.
/// Owns one contiguous virtual reservation; sections bump-allocate inside it
/// so RVAs in `.pdata` reach `.text`/`.xdata` within u32 range.
pub(crate) struct JitMm {
    bump: Mutex<Bump>,
    sections: Mutex<Vec<Section>>,
}

impl JitMm {
    fn new() -> Box<JitMm> {
        let base = reserve_module_region();
        Box::new(JitMm {
            bump: Mutex::new(Bump {
                base,
                size: MODULE_RESERVE,
                used: 0,
            }),
            sections: Mutex::new(Vec::new()),
        })
    }

    fn alloc(&self, size: usize, align: u32) -> *mut u8 {
        if size == 0 {
            return std::ptr::null_mut();
        }
        let mut b = self.bump.lock().unwrap();
        if b.base.is_null() {
            return std::ptr::null_mut();
        }
        // Each section starts on its own page so a VirtualProtect to
        // EXECUTE_READ on finalize never drags adjacent data along.
        let start = (b.used + (PAGE - 1)) & !(PAGE - 1);
        let user_align = (align as usize).max(8);
        let start = (start + user_align - 1) & !(user_align - 1);
        let end = start + size;
        let end_rounded = (end + (PAGE - 1)) & !(PAGE - 1);
        if end_rounded > b.size {
            return std::ptr::null_mut();
        }
        let p = unsafe { b.base.add(start) };
        if !commit_pages(p, end_rounded - start) {
            return std::ptr::null_mut();
        }
        b.used = end_rounded;
        p
    }

    fn track(&self, sec: Section) {
        self.sections.lock().unwrap().push(sec);
    }
}

// ── Reserve + commit primitives ───────────────────────────────────────

#[cfg(windows)]
fn reserve_module_region() -> *mut u8 {
    unsafe {
        win::VirtualAlloc(
            std::ptr::null_mut(),
            MODULE_RESERVE,
            win::MEM_RESERVE,
            win::PAGE_NOACCESS,
        ) as *mut u8
    }
}

#[cfg(not(windows))]
fn reserve_module_region() -> *mut u8 {
    unimplemented!("jit_mm: non-Windows reservation not yet implemented")
}

#[cfg(windows)]
fn commit_pages(p: *mut u8, size: usize) -> bool {
    let r =
        unsafe { win::VirtualAlloc(p as *mut c_void, size, win::MEM_COMMIT, win::PAGE_READWRITE) };
    !r.is_null()
}

#[cfg(not(windows))]
fn commit_pages(_p: *mut u8, _size: usize) -> bool {
    unimplemented!("jit_mm: non-Windows commit not yet implemented")
}

// ── RTDyld MM callbacks ───────────────────────────────────────────────

pub(crate) extern "C" fn allocate_code_section(
    opaque: *mut c_void,
    size: usize,
    alignment: u32,
    _section_id: u32,
    section_name: *const c_char,
) -> *mut u8 {
    let mm = unsafe { &*(opaque as *const JitMm) };
    let p = mm.alloc(size, alignment);
    if p.is_null() {
        return p;
    }
    mm.track(Section {
        name_tag: NameTag::from_name(section_name),
        ptr: p,
        size,
        is_code: true,
    });
    p
}

pub(crate) extern "C" fn allocate_data_section(
    opaque: *mut c_void,
    size: usize,
    alignment: u32,
    _section_id: u32,
    section_name: *const c_char,
    _is_readonly: i32,
) -> *mut u8 {
    let mm = unsafe { &*(opaque as *const JitMm) };
    let p = mm.alloc(size, alignment);
    if p.is_null() {
        return p;
    }
    mm.track(Section {
        name_tag: NameTag::from_name(section_name),
        ptr: p,
        size,
        is_code: false,
    });
    p
}

pub(crate) extern "C" fn finalize_memory(opaque: *mut c_void, err_msg: *mut *mut c_char) -> i32 {
    let mm = unsafe { &*(opaque as *const JitMm) };
    let sections = mm.sections.lock().unwrap().clone();

    // 1. Flip every code section to PAGE_EXECUTE_READ.
    #[cfg(windows)]
    {
        for sec in sections.iter().filter(|s| s.is_code) {
            let mut old: u32 = 0;
            let ok = unsafe {
                win::VirtualProtect(
                    sec.ptr as *mut c_void,
                    sec.size,
                    win::PAGE_EXECUTE_READ,
                    &mut old,
                )
            };
            if ok == 0 {
                let msg = b"VirtualProtect failed for code section\0";
                unsafe { *err_msg = msg.as_ptr() as *mut c_char };
                return 1;
            }
        }
        register_seh_for_module(&sections);
    }

    #[cfg(not(windows))]
    {
        let _ = (sections, err_msg);
    }

    0
}

/// Per-object teardown. With the MCJIT-emulation recipe the one shared
/// context lives across all objects, so this is a no-op; the real free
/// happens in [`notify_terminating`].
pub(crate) extern "C" fn destroy(_opaque: *mut c_void) {}

/// CreateContext: reuse the one allocation context (our [`JitMm`]) for every
/// object linked by the layer.
pub(crate) extern "C" fn create_context(ctx_ctx: *mut c_void) -> *mut c_void {
    ctx_ctx
}

/// NotifyTerminating: the layer is shutting down; free the [`JitMm`]. The
/// SEH tables and code pages it registered are intentionally leaked for the
/// process lifetime (they remain registered with the OS unwinder); only the
/// bookkeeping struct is freed. Module retirement (RtlDeleteFunctionTable +
/// VirtualFree) lands with the hot-swap sprint.
pub(crate) extern "C" fn notify_terminating(ctx_ctx: *mut c_void) {
    if !ctx_ctx.is_null() {
        let _ = unsafe { Box::from_raw(ctx_ctx as *mut JitMm) };
    }
}

/// Allocate a fresh [`JitMm`] and return it as the `CreateContextCtx` pointer
/// handed to the RTDyld-MM-callbacks layer.
pub(crate) fn new_context() -> *mut c_void {
    Box::into_raw(JitMm::new()) as *mut c_void
}

// ── SEH registration ──────────────────────────────────────────────────

#[cfg(windows)]
fn register_seh_for_module(sections: &[Section]) {
    let Some(text) = sections.iter().find(|s| s.name_tag == NameTag::Text) else {
        return;
    };
    let base = text.ptr as u64;
    if text.size > u32::MAX as usize {
        eprintln!(
            "[jit_mm] .text larger than u32 RVA window ({} bytes); SEH registration skipped",
            text.size
        );
        return;
    }

    // Windows binary-searches the table and does NOT validate sort order;
    // unsorted/zero-padded entries corrupt unwinding (wrong RUNTIME_FUNCTION
    // → bad UNWIND_INFO → RSP corruption → 0xC0000409). Pack live entries
    // (Begin < End) to the front, zero the tail, sort, register only live.
    for pdata in sections.iter().filter(|s| s.name_tag == NameTag::Pdata) {
        let raw_count = pdata.size / std::mem::size_of::<win::RUNTIME_FUNCTION>();
        if raw_count == 0 {
            continue;
        }
        let entries = unsafe {
            std::slice::from_raw_parts_mut(pdata.ptr as *mut win::RUNTIME_FUNCTION, raw_count)
        };
        let mut live = 0usize;
        for i in 0..raw_count {
            if entries[i].BeginAddress < entries[i].EndAddress {
                if i != live {
                    entries[live] = entries[i];
                }
                live += 1;
            }
        }
        for slot in &mut entries[live..raw_count] {
            *slot = win::RUNTIME_FUNCTION {
                BeginAddress: 0,
                EndAddress: 0,
                UnwindData: 0,
            };
        }
        if live == 0 {
            continue;
        }
        let live_entries = &mut entries[..live];
        live_entries.sort_by_key(|e| e.BeginAddress);

        // Sanity-check: BaseAddress + first Begin must land inside .text.
        let first = &live_entries[0];
        let computed = base.wrapping_add(first.BeginAddress as u64);
        let text_lo = base;
        let text_hi = base + text.size as u64;
        if computed < text_lo || computed >= text_hi {
            eprintln!(
                "[jit_mm] SEH base-address sanity check failed: base={base:#x} \
                 first.BeginAddress={:#x} computed={computed:#x} text=[{text_lo:#x},{text_hi:#x}). \
                 Skipping registration.",
                first.BeginAddress
            );
            continue;
        }

        let ok = unsafe {
            win::RtlAddFunctionTable(pdata.ptr as *const win::RUNTIME_FUNCTION, live as u32, base)
        };
        if ok == 0 {
            eprintln!(
                "[jit_mm] RtlAddFunctionTable failed for {live} entries at base={base:#x}; \
                 exceptions through JIT frames will abort"
            );
        } else if std::env::var_os("NBF_TRACE_SEH").is_some() {
            eprintln!(
                "[jit_mm] registered {live} live SEH entries (of {raw_count}) at base={base:#x}"
            );
        }
    }
}
