//! End-to-end: a Beef program's `extern` imports → sema discovery → resolved
//! against the *real* `newbf-winapi` Win32 ABI oracle (15k functions). This
//! is the chain the FFI lowering rides on: discover demanded APIs, get each
//! one's DLL + signature from the oracle, then materialize import thunks.

use newbf_lexer::FileId;
use newbf_parser::parse_file;
use newbf_sema::{SourceFile, analyze, discover_extern_methods, resolve_apis};

#[test]
fn discovers_and_resolves_real_win32_imports() {
    // Param *types* are irrelevant to discovery (it resolves by name + arity),
    // so use plain `int`/`uint32` to keep the snippet trivially parseable.
    let src = "\
class Win32 {
    public static extern int MessageBoxW(int hWnd, int lpText, int lpCaption, uint32 uType);
    public static extern void Sleep(uint32 dwMilliseconds);
    public static extern int NotARealWin32Function(int x);
}";
    let (unit, pdiags) = parse_file(src, FileId(0));
    assert!(pdiags.is_empty(), "parse diagnostics: {pdiags:?}");
    let files = [SourceFile {
        file: FileId(0),
        src,
        unit: &unit,
        name: "",
    }];
    let program = analyze(&files);

    let imports = discover_extern_methods(&program);
    assert_eq!(imports.len(), 3, "discovered: {imports:?}");

    // Resolve against the REAL oracle.
    let resolved = resolve_apis(imports, |name| {
        newbf_winapi::find_function_any_dll(name).map(|f| (f.dll.clone(), f.params.len()))
    });

    let mbw = resolved
        .iter()
        .find(|r| r.import.symbol == "MessageBoxW")
        .unwrap();
    assert!(mbw.is_resolved(), "MessageBoxW should resolve");
    assert_eq!(mbw.dll.as_deref(), Some("user32.dll"));
    // MessageBoxW(hWnd, lpText, lpCaption, uType) — 4 params; our declaration
    // matches, so the arity cross-check passes.
    assert_eq!(mbw.arity_matches(), Some(true));

    let sleep = resolved
        .iter()
        .find(|r| r.import.symbol == "Sleep")
        .unwrap();
    assert!(sleep.is_resolved(), "Sleep should resolve");
    assert_eq!(sleep.dll.as_deref(), Some("kernel32.dll"));

    let bogus = resolved
        .iter()
        .find(|r| r.import.symbol == "NotARealWin32Function")
        .unwrap();
    assert!(!bogus.is_resolved(), "bogus name must not resolve");
    assert_eq!(bogus.arity_matches(), None);
}
