//! Whole-file parser corpus gate. Parses every `.bf` file in
//! `beef-tests/corlib-slice/` and `beef-tests/feature-suite/src/` (read-
//! only fixtures snapshotted from upstream Beef).
//!
//! The hard gate is **no panics**: the test passing proves the parser
//! terminates on every file without crashing. The success rate (files
//! parsed with zero diagnostics) is reported via `eprintln!` and gated
//! with a low threshold — full corpus compatibility is open-ended work
//! that grows over future sprints. Run with `--nocapture` to see counts.
//!
//! NOTE: the corpus lives at `E:\NewBF\beef-tests\…`; this test is keyed
//! to that vendored location.

use std::path::PathBuf;

use newbf_lexer::FileId;
use newbf_parser::parse_file;

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
fn parser_does_not_panic_on_real_beef() {
    let mut files = Vec::new();
    collect_bf(&root().join("corlib-slice"), &mut files);
    collect_bf(&root().join("feature-suite/src"), &mut files);

    let mut clean = 0usize;
    let mut errored = 0usize;
    let mut total_diags = 0usize;
    let mut worst: Vec<(usize, String)> = Vec::new();

    for path in &files {
        let src = std::fs::read_to_string(path).unwrap();
        let (_unit, diags) = parse_file(&src, FileId(0));
        if diags.is_empty() {
            clean += 1;
        } else {
            errored += 1;
            total_diags += diags.len();
            // remember the noisiest few files for the report
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            worst.push((diags.len(), name));
        }
    }
    worst.sort_by_key(|(n, _)| std::cmp::Reverse(*n));

    eprintln!(
        "parser corpus: {clean} / {} files parsed cleanly  ({errored} with diagnostics, \
         {total_diags} diagnostics total)",
        files.len()
    );
    eprintln!("  noisiest files:");
    for (n, name) in worst.iter().take(8) {
        eprintln!("    {n:>4}  {name}");
    }

    // No-panic gate.
    assert!(!files.is_empty(), "no .bf fixtures found");
    // Coverage ratchet: the bar rises as Beef-syntax coverage fills in.
    // Target (Path B, full Beef faithfulness) is ~70%; this floor locks
    // in current progress so coverage can't silently regress.
    let floor = files.len() * 35 / 100;
    assert!(
        clean >= floor,
        "parser clean-parse coverage regressed: {clean} / {} (floor {floor})",
        files.len()
    );
}
