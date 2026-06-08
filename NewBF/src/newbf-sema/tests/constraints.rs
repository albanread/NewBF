//! CT-T1 (generic-constraints.md §7) — the ratchet-safety pins for the
//! generic-`where`-clause enforcement pass.
//!
//! These assert that `newbf_sema::analyze`'s new `check_generic_constraints`
//! pass emits **zero** constraint diagnostics on the four constraint-dense corpus
//! files (`Constraints.bf`, `Generics.bf`, `Generics2.bf`, `Interfaces.bf`).
//!
//! For CT-T1 the pass is a no-op (it classifies every clause but emits no
//! diagnostic), so these pins pass trivially. They are landed **first**, before
//! any diagnostic-emitting task (CT-T2/CT-T3), so those tasks have a precise
//! per-file zero-diagnostic baseline: the moment CT-T2/CT-T3 over-classify and
//! turn a dense corpus clause into a diagnostic, the matching pin here fails with
//! a per-file failure signal — long before the broader whole-corpus ratchet in
//! `tests/corpus.rs::sema_does_not_panic_on_real_beef` (which would also fail,
//! but without pinpointing which file).
//!
//! The `constraint_diags` helper is **root-parameterized** (mirroring
//! `double_free_diags`, `delete_flow.rs:31`, but able to read both
//! `beef-tests/feature-suite/src/` ratchet files and a `tests/constraints/`
//! directory for CT-T2/CT-T3's future negative fixtures). It analyzes the given
//! source as its own **one-file program** — exactly the per-file ratchet
//! configuration `tests/corpus.rs` uses — and returns only the constraint
//! diagnostics, filtered by a `constraint` substring so an unrelated diagnostic
//! cannot perturb the count.

use std::path::PathBuf;

use newbf_lexer::FileId;
use newbf_parser::parse_file;
use newbf_sema::{Diagnostic, SourceFile, analyze};

/// The repo's `beef-tests` root (same resolution `tests/corpus.rs` uses).
fn beef_tests_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../beef-tests")
}

/// Analyze the file at `path` as its own one-file program and return only the
/// **constraint** diagnostics — filtered so an unrelated future diagnostic
/// (delete-flow, duplicate-def) cannot perturb the count. The filter keys on a
/// `constraint` substring; CT-T1 emits none, and CT-T2/CT-T3's messages will all
/// describe a "constraint" violation, so this is the stable channel for the pass.
fn constraint_diags_at(path: &std::path::Path) -> Vec<Diagnostic> {
    let src = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    // Parse diagnostics are tolerated (the corpus ratchet does the same): the
    // constraint pass must classify without panicking even on a partial AST. But
    // the four ratchet files all parse clean today, so assert it to catch a
    // regression early.
    let (unit, pdiags) = parse_file(&src, FileId(0));
    assert!(
        pdiags.is_empty(),
        "{} must parse clean: {pdiags:?}",
        path.display()
    );
    let program = analyze(&[SourceFile {
        file: FileId(0),
        src: &src,
        unit: &unit,
        name: "",
    }]);
    program
        .diagnostics
        .into_iter()
        .filter(|d| d.message.to_lowercase().contains("constraint"))
        .collect()
}

/// Root-parameterized wrapper: count the constraint diagnostics for a named file
/// resolved under `dir` (relative to `beef-tests`). Lets CT-T2/CT-T3 reuse the
/// same helper against `feature-suite/src` and a future `tests/constraints/`.
fn constraint_diags(dir: &str, name: &str) -> Vec<Diagnostic> {
    let path = beef_tests_root().join(dir).join(name);
    constraint_diags_at(&path)
}

/// The four constraint-dense ratchet files, analyzed each as a one-file program,
/// must each produce **zero** constraint diagnostics. CT-T1 emits none (the pass
/// is a no-op classifier), so this is trivially true now — but the pins are
/// landed FIRST so CT-T2/CT-T3 cannot regress any individual file.
const RATCHET_DIR: &str = "feature-suite/src";

#[test]
fn constraints_bf_has_zero_constraint_diags() {
    let diags = constraint_diags(RATCHET_DIR, "Constraints.bf");
    assert!(
        diags.is_empty(),
        "Constraints.bf must emit zero constraint diagnostics, got: {diags:?}"
    );
}

#[test]
fn generics_bf_has_zero_constraint_diags() {
    let diags = constraint_diags(RATCHET_DIR, "Generics.bf");
    assert!(
        diags.is_empty(),
        "Generics.bf must emit zero constraint diagnostics, got: {diags:?}"
    );
}

#[test]
fn generics2_bf_has_zero_constraint_diags() {
    let diags = constraint_diags(RATCHET_DIR, "Generics2.bf");
    assert!(
        diags.is_empty(),
        "Generics2.bf must emit zero constraint diagnostics, got: {diags:?}"
    );
}

#[test]
fn interfaces_bf_has_zero_constraint_diags() {
    let diags = constraint_diags(RATCHET_DIR, "Interfaces.bf");
    assert!(
        diags.is_empty(),
        "Interfaces.bf must emit zero constraint diagnostics, got: {diags:?}"
    );
}
