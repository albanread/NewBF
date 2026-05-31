//! The "hello world" gate. A program that calls `Console.WriteLine` must put
//! the exact bytes on stdout — something a return value can't prove. So this
//! redirects the process `STD_OUTPUT_HANDLE` to an anonymous pipe across the
//! JIT run (the same handle the JIT'd `WriteFile` resolves via `GetStdHandle`),
//! then reads it back and asserts the text. WriteFile is unbuffered, so there's
//! no flush to coordinate.
#![cfg(windows)]

use std::ffi::c_void;
use std::ptr;

use newbf_lexer::FileId;
use newbf_llvm::OrcJit;
use newbf_parser::parse_file;
use newbf_sema::{SourceFile, analyze, lower_program};

const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5; // (DWORD)-11

unsafe extern "system" {
    fn GetStdHandle(n_std_handle: u32) -> *mut c_void;
    fn SetStdHandle(n_std_handle: u32, handle: *mut c_void) -> i32;
    fn CreatePipe(
        read: *mut *mut c_void,
        write: *mut *mut c_void,
        attrs: *mut c_void,
        size: u32,
    ) -> i32;
    fn ReadFile(
        file: *mut c_void,
        buffer: *mut u8,
        to_read: u32,
        read: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn CloseHandle(object: *mut c_void) -> i32;
}

/// Parse → analyze → lower → JIT → call `Program.Main` (for its side effects).
fn run_main(src: &str) {
    let (unit, pdiags) = parse_file(src, FileId(0));
    assert!(pdiags.is_empty(), "parse diagnostics: {pdiags:?}");
    let files = [SourceFile {
        file: FileId(0),
        src,
        unit: &unit,
    }];
    let program = analyze(&files);
    let module = lower_program(&files, &program);
    let jit = OrcJit::from_ir(&module).expect("jit builds");
    let addr = jit.lookup("Program.Main").expect("Program.Main resolves");
    // SAFETY: corpus entry is `static int32 Main()` — a nullary `i32` fn.
    let main: extern "C" fn() -> i32 = unsafe { std::mem::transmute(addr) };
    let _ = main();
}

#[test]
fn console_writeline_emits_exact_bytes() {
    const SRC: &str = r#"
class Program {
    public static int32 Main() {
        String s = "Hello, world!";
        Console.WriteLine(s);
        delete s;
        return 0;
    }
}
"#;

    let captured = unsafe {
        let saved = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut rd: *mut c_void = ptr::null_mut();
        let mut wr: *mut c_void = ptr::null_mut();
        assert!(
            CreatePipe(&mut rd, &mut wr, ptr::null_mut(), 0) != 0,
            "CreatePipe failed"
        );
        assert!(
            SetStdHandle(STD_OUTPUT_HANDLE, wr) != 0,
            "SetStdHandle (redirect) failed"
        );

        run_main(SRC);

        // Restore real stdout and drop the write end so the read terminates.
        SetStdHandle(STD_OUTPUT_HANDLE, saved);
        CloseHandle(wr);

        let mut buf = [0u8; 256];
        let mut n: u32 = 0;
        let ok = ReadFile(
            rd,
            buf.as_mut_ptr(),
            buf.len() as u32,
            &mut n,
            ptr::null_mut(),
        );
        CloseHandle(rd);
        assert!(ok != 0, "ReadFile from capture pipe failed");
        String::from_utf8(buf[..n as usize].to_vec()).expect("captured bytes are utf8")
    };

    assert_eq!(captured, "Hello, world!\n", "captured stdout mismatch");
}
