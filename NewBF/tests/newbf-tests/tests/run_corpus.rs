//! Compile-and-run corpus: every `.bf` in `beef-tests/run-corpus/` is a
//! self-contained `Program.Main` returning `int32`, with a `// expect: N`
//! header. This harness drives the *whole* pipeline — parse → analyze → lower
//! → LLVM → JIT — then calls `Program.Main` and checks its value. Behavioral,
//! not golden-IR: it catches "the meaning changed", which is what found the
//! `int32 x = 0` stack-overrun bug (a store wider than the slot) that the LLVM
//! verifier happily accepted.
//!
//! JIT (not AOT) so there's no per-program link; the AOT path has its own
//! end-to-end coverage in `newbf-llvm`.

use std::path::PathBuf;

use newbf_lexer::FileId;
use newbf_llvm::OrcJit;
use newbf_parser::parse_file;
use newbf_sema::{SourceFile, analyze, lower_program};

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../beef-tests/run-corpus")
}

/// The `// expect: N` value from the program's header.
fn expected(src: &str) -> Option<i32> {
    src.lines().find_map(|l| {
        l.trim()
            .strip_prefix("// expect:")
            .and_then(|n| n.trim().parse().ok())
    })
}

/// Parse → analyze → lower → JIT → call `Program.Main`, returning its `i32`.
fn run(src: &str) -> i32 {
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
    // SAFETY: corpus entries are `static int32 Main()` — a nullary `i32` fn.
    let main: extern "C" fn() -> i32 = unsafe { std::mem::transmute(addr) };
    main()
}

#[test]
fn run_corpus_programs_produce_expected_values() {
    let dir = corpus_dir();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("bf"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no .bf programs in {}", dir.display());

    for path in &paths {
        let src = std::fs::read_to_string(path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy();
        let exp = expected(&src).unwrap_or_else(|| panic!("{name}: missing `// expect: N`"));
        let got = run(&src);
        assert_eq!(
            got, exp,
            "{name}: Program.Main returned {got}, expected {exp}"
        );
        eprintln!("  {name:<16} → {got}  ✓");
    }
    eprintln!(
        "run-corpus: {} programs compiled + ran with correct results",
        paths.len()
    );
}
