//! Whole-corpus def-build gate. Parses then analyzes every `.bf` file in
//! `beef-tests/corlib-slice/` and `beef-tests/feature-suite/src/` and
//! builds the definition graph.
//!
//! The hard gate is **no panics**: the test passing proves sema's build +
//! resolve passes terminate on every real Beef file without crashing. It
//! also reports exhaustiveness (total namespaces/types/members captured)
//! and the sema clean rate (files with zero sema diagnostics) via
//! `eprintln!`. Run with `--nocapture` to see the counts.
//!
//! Sema diagnostics here are in-program contradictions only (duplicate
//! definitions). Unresolved references to corlib (`System`, primitive
//! type names) are *not* errors at this phase — corlib lands later — so a
//! clean parse should almost always yield a clean def-build.
//!
//! NOTE: the corpus lives at `E:\NewBF\beef-tests\…`.

use std::path::PathBuf;

use newbf_lexer::FileId;
use newbf_parser::parse_file;
use newbf_sema::{SourceFile, analyze, lower_program};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../beef-tests")
}

fn collect_bf(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_bf(&p, out);
        } else if p.extension().and_then(|x| x.to_str()) == Some("bf") {
            out.push(p);
        }
    }
}

#[test]
fn sema_does_not_panic_on_real_beef() {
    let mut files = Vec::new();
    collect_bf(&root().join("corlib-slice"), &mut files);
    collect_bf(&root().join("feature-suite/src"), &mut files);

    let mut clean = 0usize;
    let mut errored = 0usize;
    let mut total_types = 0usize;
    let mut total_members = 0usize;
    let mut total_namespaces = 0usize;
    let mut total_diags = 0usize;
    let mut worst: Vec<(usize, String)> = Vec::new();

    for path in &files {
        let src = std::fs::read_to_string(path).unwrap();
        // Analyze each file as its own one-file program. Parse diagnostics
        // are tolerated — sema must still not panic on a partial AST.
        let (unit, _pdiags) = parse_file(&src, FileId(0));
        let program = analyze(&[SourceFile {
            file: FileId(0),
            src: &src,
            unit: &unit,
        }]);

        total_types += program.graph.types.len();
        total_members += program.graph.members.len();
        // -1 for the always-present global namespace.
        total_namespaces += program.graph.namespaces.len().saturating_sub(1);

        if program.diagnostics.is_empty() {
            clean += 1;
        } else {
            errored += 1;
            total_diags += program.diagnostics.len();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            worst.push((program.diagnostics.len(), name));
        }
    }
    worst.sort_by_key(|(n, _)| std::cmp::Reverse(*n));

    eprintln!(
        "sema corpus: {clean} / {} files built cleanly  ({errored} with sema diagnostics, \
         {total_diags} diagnostics total)",
        files.len()
    );
    eprintln!(
        "  captured: {total_namespaces} namespaces, {total_types} types, {total_members} members"
    );
    eprintln!("  noisiest files:");
    for (n, name) in worst.iter().take(8) {
        eprintln!("    {n:>4}  {name}");
    }

    // No-panic gate.
    assert!(!files.is_empty(), "no .bf fixtures found");
    // Exhaustiveness: the build pass must actually capture symbols, not
    // silently no-op.
    assert!(total_types > 0, "no types captured across the corpus");
    assert!(total_members > 0, "no members captured across the corpus");
    // Clean-build ratchet. The parser now parses the whole corpus cleanly,
    // and sema builds a contradiction-free definition graph for **every**
    // file. The floor is 100%.
    assert_eq!(
        clean,
        files.len(),
        "sema clean-build coverage regressed below 100%: {clean} / {}",
        files.len()
    );
}

/// IR lowering (the primitive kernel) must terminate on every real Beef
/// file without panicking — richer constructs are skipped, not crashed on.
/// Reports how many IR functions were produced across the corpus.
#[test]
fn lowering_does_not_panic_on_real_beef() {
    let mut files = Vec::new();
    collect_bf(&root().join("corlib-slice"), &mut files);
    collect_bf(&root().join("feature-suite/src"), &mut files);
    assert!(!files.is_empty(), "no .bf fixtures found");

    let mut total_funcs = 0usize;
    for path in &files {
        let src = std::fs::read_to_string(path).unwrap();
        let (unit, _pdiags) = parse_file(&src, FileId(0));
        let srcs = [SourceFile {
            file: FileId(0),
            src: &src,
            unit: &unit,
        }];
        let program = analyze(&srcs);
        let module = lower_program(&srcs, &program);
        total_funcs += module.funcs.len();
    }
    eprintln!(
        "ir lowering: {total_funcs} functions lowered across {} files",
        files.len()
    );
    assert!(total_funcs > 0, "lowering produced no functions");
}
