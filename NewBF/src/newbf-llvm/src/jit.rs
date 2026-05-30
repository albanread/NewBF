//! ORCv2 LLJIT execution with a Win64-SEH-aware RTDyld object linking layer.
//!
//! inkwell builds the module; llvm-sys drives ORC. The JIT runs over
//! `RTDyldObjectLinkingLayer` (not JITLink) so it can reuse NewM2's
//! `RtlAddFunctionTable`-registering memory manager ([`crate::jit_mm`]) — the
//! gating requirement is that exceptions unwind through JIT'd frames.
//!
//! ## The corrected binding
//! llvm-sys 221's binding for
//! `LLVMOrcCreateRTDyldObjectLinkingLayerWithMCJITMemoryManagerLikeCallbacks`
//! is wrong: it omits the `CreateContextCtx` parameter and types the
//! `CreateContext` callback as returning `()` instead of `void*` (the real
//! LLVM 22 C API — verified against the installed `OrcEE.h` — returns the
//! context). Calling the llvm-sys binding would mis-pass arguments and crash,
//! so we declare a corrected `extern` below. (LLVM 22 also dropped
//! `LLVMOrcThreadSafeContextGetContext`, so the module is handed over by
//! transferring ownership of the inkwell context via
//! `…ThreadSafeContextFromLLVMContext`.)

use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::Once;

use inkwell::context::Context;
use inkwell::targets::{InitializationConfig, Target};
use llvm_sys::core::LLVMGetModuleContext;
use llvm_sys::error::{LLVMDisposeErrorMessage, LLVMErrorRef, LLVMGetErrorMessage};
use llvm_sys::execution_engine::{
    LLVMMemoryManagerAllocateCodeSectionCallback, LLVMMemoryManagerAllocateDataSectionCallback,
    LLVMMemoryManagerDestroyCallback, LLVMMemoryManagerFinalizeMemoryCallback,
};
use llvm_sys::orc2::lljit::{
    LLVMOrcCreateLLJIT, LLVMOrcCreateLLJITBuilder, LLVMOrcDisposeLLJIT,
    LLVMOrcLLJITAddLLVMIRModule, LLVMOrcLLJITBuilderSetObjectLinkingLayerCreator,
    LLVMOrcLLJITGetGlobalPrefix, LLVMOrcLLJITGetMainJITDylib, LLVMOrcLLJITLookup, LLVMOrcLLJITRef,
};
use llvm_sys::orc2::{
    LLVMOrcCreateDynamicLibrarySearchGeneratorForProcess,
    LLVMOrcCreateNewThreadSafeContextFromLLVMContext, LLVMOrcCreateNewThreadSafeModule,
    LLVMOrcDefinitionGeneratorRef, LLVMOrcDisposeThreadSafeContext, LLVMOrcExecutionSessionRef,
    LLVMOrcExecutorAddress, LLVMOrcJITDylibAddGenerator, LLVMOrcObjectLayerRef,
};
use newbf_ir::Module as IrModule;

use crate::jit_mm;
use crate::lower::emit_module;

// The corrected binding (see module docs). The alloc/finalize/destroy
// callback types from `execution_engine` already match the C header.
unsafe extern "C" {
    fn LLVMOrcCreateRTDyldObjectLinkingLayerWithMCJITMemoryManagerLikeCallbacks(
        ES: LLVMOrcExecutionSessionRef,
        CreateContextCtx: *mut c_void,
        CreateContext: extern "C" fn(*mut c_void) -> *mut c_void,
        NotifyTerminating: extern "C" fn(*mut c_void),
        AllocateCodeSection: LLVMMemoryManagerAllocateCodeSectionCallback,
        AllocateDataSection: LLVMMemoryManagerAllocateDataSectionCallback,
        FinalizeMemory: LLVMMemoryManagerFinalizeMemoryCallback,
        Destroy: LLVMMemoryManagerDestroyCallback,
    ) -> LLVMOrcObjectLayerRef;
}

/// Register the host target + asm printer once (required before LLJIT
/// detects the host machine).
fn init_target() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        Target::initialize_native(&InitializationConfig::default())
            .expect("LLVM native target init failed");
    });
}

/// LLJIT's object-linking-layer factory: build an RTDyld layer backed by our
/// SEH-registering memory manager. `ctx` is the `JitMm` pointer we threaded
/// through `SetObjectLinkingLayerCreator`, reused as the `CreateContextCtx`.
extern "C" fn obj_layer_creator(
    ctx: *mut c_void,
    es: LLVMOrcExecutionSessionRef,
    _triple: *const c_char,
) -> LLVMOrcObjectLayerRef {
    unsafe {
        LLVMOrcCreateRTDyldObjectLinkingLayerWithMCJITMemoryManagerLikeCallbacks(
            es,
            ctx,
            jit_mm::create_context,
            jit_mm::notify_terminating,
            jit_mm::allocate_code_section,
            jit_mm::allocate_data_section,
            jit_mm::finalize_memory,
            Some(jit_mm::destroy),
        )
    }
}

fn take_error(err: LLVMErrorRef) -> String {
    // LLVMGetErrorMessage consumes the error and yields an owned C string.
    unsafe {
        let cmsg = LLVMGetErrorMessage(err);
        let s = CStr::from_ptr(cmsg).to_string_lossy().into_owned();
        LLVMDisposeErrorMessage(cmsg);
        s
    }
}

/// A live ORC LLJIT instance holding one JIT'd module. Symbols stay callable
/// until this is dropped.
pub struct OrcJit {
    jit: LLVMOrcLLJITRef,
}

impl OrcJit {
    /// Lower `ir` to LLVM, JIT it under ORC + RTDyld with SEH registration,
    /// and return a handle whose symbols can be looked up and called.
    pub fn from_ir(ir: &IrModule) -> Result<OrcJit, String> {
        init_target();

        // Build the module with inkwell, then transfer ownership of both the
        // module and its context to ORC (LLVM 22 dropped the API to read a
        // context back out of a fresh ThreadSafeContext, so we donate ours).
        let ctx = Context::create();
        let module = emit_module(&ctx, ir);
        let mod_raw = module.as_mut_ptr();
        let ctx_raw = unsafe { LLVMGetModuleContext(mod_raw) };
        std::mem::forget(module);
        std::mem::forget(ctx);

        // Fresh SEH memory-manager context, threaded to the layer factory.
        let mm_ctx = jit_mm::new_context();

        let builder = unsafe { LLVMOrcCreateLLJITBuilder() };
        unsafe {
            LLVMOrcLLJITBuilderSetObjectLinkingLayerCreator(builder, obj_layer_creator, mm_ctx);
        }

        let mut jit: LLVMOrcLLJITRef = std::ptr::null_mut();
        let err = unsafe { LLVMOrcCreateLLJIT(&mut jit, builder) };
        if !err.is_null() {
            // CreateLLJIT consumes the builder even on failure. The layer
            // factory never ran, so free the orphaned MM context.
            jit_mm::notify_terminating(mm_ctx);
            return Err(format!("LLJIT creation failed: {}", take_error(err)));
        }

        // Resolve external symbols from the host process — Win32 exports from
        // loaded DLLs (kernel32 et al.), runtime helpers, comptime callbacks.
        // This is what lets JIT'd code call the Windows API directly: ORC's
        // idiomatic equivalent of MCJIT's LLVMAddGlobalMapping, and simpler —
        // no per-symbol binding, the generator resolves on demand at lookup.
        let jd = unsafe { LLVMOrcLLJITGetMainJITDylib(jit) };
        unsafe {
            let prefix = LLVMOrcLLJITGetGlobalPrefix(jit);
            let mut generator: LLVMOrcDefinitionGeneratorRef = std::ptr::null_mut();
            let gerr = LLVMOrcCreateDynamicLibrarySearchGeneratorForProcess(
                &mut generator,
                prefix,
                None,
                std::ptr::null_mut(),
            );
            if gerr.is_null() {
                LLVMOrcJITDylibAddGenerator(jd, generator);
            } else {
                // Non-fatal: without it, only module-internal symbols resolve.
                let _ = take_error(gerr);
            }
        }

        // Wrap the donated context + module and hand the module to the JIT.
        let tsc = unsafe { LLVMOrcCreateNewThreadSafeContextFromLLVMContext(ctx_raw) };
        let tsm = unsafe { LLVMOrcCreateNewThreadSafeModule(mod_raw, tsc) };
        // The ThreadSafeModule keeps its own reference to the context's data.
        unsafe { LLVMOrcDisposeThreadSafeContext(tsc) };

        let err = unsafe { LLVMOrcLLJITAddLLVMIRModule(jit, jd, tsm) };
        if !err.is_null() {
            let msg = take_error(err);
            unsafe { LLVMOrcDisposeLLJIT(jit) };
            return Err(format!("adding module failed: {msg}"));
        }

        Ok(OrcJit { jit })
    }

    /// Look up a JIT'd symbol's address (0 / `None` if unresolved). ORC
    /// applies the target's name mangling internally, so pass the plain name.
    pub fn lookup(&self, name: &str) -> Option<u64> {
        let cname = CString::new(name).ok()?;
        let mut addr: LLVMOrcExecutorAddress = 0;
        let err = unsafe { LLVMOrcLLJITLookup(self.jit, &mut addr, cname.as_ptr()) };
        if !err.is_null() {
            // Consume + discard the not-found error.
            let _ = take_error(err);
            return None;
        }
        (addr != 0).then_some(addr)
    }
}

impl Drop for OrcJit {
    fn drop(&mut self) {
        if !self.jit.is_null() {
            // Disposing the LLJIT tears down the object layer, which calls
            // our NotifyTerminating to free the MM context.
            unsafe {
                let _ = LLVMOrcDisposeLLJIT(self.jit);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::OrcJit;
    use newbf_ir::{BinOp, FunctionBuilder, IrType, Module as IrModule, Param};

    /// Milestone 1: ORC + RTDyld + the SEH memory manager actually execute a
    /// JIT'd function and return the right value.
    #[test]
    fn jit_executes_add() {
        // i64 add(i64 a, i64 b) => a + b;
        let mut f = FunctionBuilder::new(
            "add",
            vec![
                Param {
                    name: Some("a".into()),
                    ty: IrType::I64,
                },
                Param {
                    name: Some("b".into()),
                    ty: IrType::I64,
                },
            ],
            IrType::I64,
        );
        let (a, b) = (f.param(0), f.param(1));
        let s = f.bin(BinOp::Add, a, b, IrType::I64);
        f.ret(Some(s));
        let mut m = IrModule::new("jit_add");
        m.add_function(f.finish());

        let jit = OrcJit::from_ir(&m).expect("jit builds");
        let addr = jit.lookup("add").expect("add resolves");
        let add: extern "C" fn(i64, i64) -> i64 = unsafe { std::mem::transmute(addr) };
        assert_eq!(add(3, 4), 7);
        assert_eq!(add(-10, 32), 22);
    }

    /// The compiler autogenerates a real Win32 call. JIT a function that calls
    /// `GetCurrentProcessId` (kernel32 — always loaded), declared with the
    /// oracle's signature and resolved via the ORC process search generator,
    /// and confirm it returns this very process's id. This is the JIT half of
    /// "call the Windows API directly" — no thunk table, no AddGlobalMapping.
    #[cfg(windows)]
    #[test]
    fn jit_calls_a_real_win32_function() {
        // extern u32 GetCurrentProcessId();  u32 pid() => GetCurrentProcessId();
        let mut m = IrModule::new("jit_win32");
        m.declare_extern("GetCurrentProcessId", vec![], IrType::U32);
        let mut f = FunctionBuilder::new("pid", vec![], IrType::U32);
        let r = f.call("GetCurrentProcessId", vec![], IrType::U32);
        f.ret(Some(r));
        m.add_function(f.finish());

        let jit = OrcJit::from_ir(&m).expect("jit builds");
        let addr = jit.lookup("pid").expect("pid resolves");
        let pid: extern "C" fn() -> u32 = unsafe { std::mem::transmute(addr) };
        assert_eq!(pid(), std::process::id());
    }

    /// Milestone 2 (SEH foundation): a `debugtrap` lowered into JIT'd code
    /// raises `EXCEPTION_BREAKPOINT`, which a Vectored Exception Handler
    /// catches — with the faulting `Rip` inside the JIT'd function — and then
    /// resumes past. This proves Windows SEH delivery works for code running
    /// in JIT'd `VirtualAlloc` pages (the precondition for symbolicated stack
    /// dumps). `debugtrap` (int3, 1 byte) is resumable, so the handler steps
    /// over it and the function returns normally — no process abort.
    #[cfg(windows)]
    #[test]
    fn debugtrap_in_jit_code_delivers_seh() {
        use std::ffi::c_void;
        use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

        const EXCEPTION_BREAKPOINT: u32 = 0x8000_0003;
        const EXCEPTION_CONTINUE_EXECUTION: i32 = -1;
        const EXCEPTION_CONTINUE_SEARCH: i32 = 0;
        // x64 CONTEXT.Rip offset (winnt.h).
        const RIP_OFFSET: usize = 0xF8;

        #[repr(C)]
        struct ExceptionRecord {
            exception_code: u32,
            exception_flags: u32,
            exception_record: *mut ExceptionRecord,
            exception_address: *mut c_void,
            // remaining fields unused
        }
        #[repr(C)]
        struct ExceptionPointers {
            exception_record: *mut ExceptionRecord,
            context_record: *mut c_void, // CONTEXT*, accessed by offset
        }

        unsafe extern "system" {
            fn AddVectoredExceptionHandler(
                First: u32,
                Handler: unsafe extern "system" fn(*mut ExceptionPointers) -> i32,
            ) -> *mut c_void;
            fn RemoveVectoredExceptionHandler(Handle: *mut c_void) -> u32;
        }

        static FIRED: AtomicBool = AtomicBool::new(false);
        static CODE: AtomicU32 = AtomicU32::new(0);
        static RIP: AtomicU64 = AtomicU64::new(0);

        unsafe extern "system" fn veh(info: *mut ExceptionPointers) -> i32 {
            unsafe {
                let rec = &*(*info).exception_record;
                if rec.exception_code != EXCEPTION_BREAKPOINT {
                    return EXCEPTION_CONTINUE_SEARCH;
                }
                let rip_ptr = ((*info).context_record as *mut u8).add(RIP_OFFSET) as *mut u64;
                CODE.store(rec.exception_code, Ordering::SeqCst);
                RIP.store(*rip_ptr, Ordering::SeqCst);
                FIRED.store(true, Ordering::SeqCst);
                // Step over the 1-byte int3 so execution resumes cleanly.
                *rip_ptr += 1;
                EXCEPTION_CONTINUE_EXECUTION
            }
        }

        // void boom() { debugtrap; return; }
        let mut f = FunctionBuilder::new("boom", vec![], IrType::Void);
        f.trap(true);
        f.ret(None);
        let mut m = IrModule::new("jit_trap");
        m.add_function(f.finish());

        let jit = OrcJit::from_ir(&m).expect("jit builds");
        let addr = jit.lookup("boom").expect("boom resolves");

        let handle = unsafe { AddVectoredExceptionHandler(1, veh) };
        assert!(!handle.is_null(), "VEH install failed");

        let boom: extern "C" fn() = unsafe { std::mem::transmute(addr) };
        boom(); // executes int3 → VEH catches + resumes → ret

        unsafe { RemoveVectoredExceptionHandler(handle) };

        assert!(FIRED.load(Ordering::SeqCst), "VEH did not fire");
        assert_eq!(CODE.load(Ordering::SeqCst), EXCEPTION_BREAKPOINT);
        let rip = RIP.load(Ordering::SeqCst);
        assert!(
            rip >= addr && rip < addr + 256,
            "faulting Rip {rip:#x} not inside JIT'd boom [{addr:#x}, {:#x})",
            addr + 256
        );
    }
}
