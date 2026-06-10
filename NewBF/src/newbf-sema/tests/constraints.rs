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

/// CT-T4 (generic-constraints.md §3.3/§6.7, R1) — analyze SEVERAL files together
/// in ONE `analyze` call (the multi-file configuration) and return only the
/// **constraint** diagnostics.
///
/// Unlike `constraint_diags_at` (one file as its own one-file program — the
/// per-file ratchet configuration `tests/corpus.rs` uses), this builds a
/// multi-file `SourceFile` list and merges it in a single `analyze` (the def
/// graph then has ALL the types — "open namespaces and extensions span files",
/// `lib.rs:59`). This is the only way to make corlib interfaces like
/// `IDisposable`/`IHashable` IN-PROGRAM and RESOLVABLE, so the GC-T3
/// instantiation check actually RUNS on `Constraints.bf`'s `where K : IHashable`
/// clause instead of skipping it as unresolvable.
///
/// Parse diagnostics are tolerated per file (exactly as `tests/corpus.rs` does):
/// the corlib slice is large and the pass must classify without panicking on a
/// partial AST. Each file gets its own `FileId` (span identity) and its own
/// `src` (so `Span::text(src)` slices the right buffer per file). The returned
/// diagnostics are filtered to the `constraint` channel so an unrelated
/// cross-file diagnostic (a duplicate-def across the merged set, a delete-flow
/// note) cannot perturb the count.
fn constraint_diags_multi(paths: &[PathBuf]) -> Vec<Diagnostic> {
    // Read every file's source first (the `SourceFile`s borrow these buffers).
    let srcs: Vec<String> = paths
        .iter()
        .map(|p| {
            std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
        })
        .collect();
    // Parse each with its own `FileId` (distinct so spans stay attributable).
    let units: Vec<_> = srcs
        .iter()
        .enumerate()
        .map(|(i, src)| {
            let (unit, _pdiags) = parse_file(src, FileId(i as u32));
            unit
        })
        .collect();
    let files: Vec<SourceFile<'_>> = srcs
        .iter()
        .zip(units.iter())
        .enumerate()
        .map(|(i, (src, unit))| SourceFile {
            file: FileId(i as u32),
            src,
            unit,
            name: "",
        })
        .collect();
    let program = analyze(&files);
    program
        .diagnostics
        .into_iter()
        .filter(|d| d.message.to_lowercase().contains("constraint"))
        .collect()
}

/// Every `.bf` file directly under `beef-tests/corlib-slice/`, sorted for a
/// deterministic merge order (so the first-wins `(name, arity)` index is stable
/// run-to-run).
fn corlib_slice_files() -> Vec<PathBuf> {
    let dir = beef_tests_root().join("corlib-slice");
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("bf"))
        .collect();
    out.sort();
    out
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

// ── CT-T2: declaration-level `class` ∧ `struct` contradiction ───────────────
//
// Negative fixtures live under `tests/constraints/` (in the sema crate, NOT the
// auto-collected `beef-tests` corpus) because they are *expected* to diagnose —
// exactly like `tests/ownership/*.bf` for delete-flow. They are checked via a
// direct `analyze` `constraint_diags`, never run-corpus (§3.5).

/// Analyze a `tests/constraints/` fixture as its own one-file program and return
/// only the constraint diagnostics — same filter as `constraint_diags_at`.
fn local_constraint_diags(name: &str) -> Vec<Diagnostic> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/constraints")
        .join(name);
    constraint_diags_at(&path)
}

/// A parameter constrained both `class` and `struct` across one decl's clauses
/// is an unsatisfiable contradiction → EXACTLY ONE constraint diagnostic.
#[test]
fn violate_decl_contradiction_diagnoses_once() {
    let diags = local_constraint_diags("violate_decl_contradiction.bf");
    assert_eq!(
        diags.len(),
        1,
        "violate_decl_contradiction.bf must emit exactly one constraint diagnostic, got: {diags:?}"
    );
}

/// Satisfiable single kind constraints (and `class`/`struct` on *distinct*
/// parameters) emit ZERO constraint diagnostics — the zero-false-positive pin.
/// CT-T3 extends this fixture with satisfied INSTANTIATIONS (transitive
/// implements/base, struct/class kinds, primitive-as-struct) — all still ZERO.
#[test]
fn satisfied_no_diag_emits_zero() {
    let diags = local_constraint_diags("satisfied_no_diag.bf");
    assert!(
        diags.is_empty(),
        "satisfied_no_diag.bf must emit zero constraint diagnostics, got: {diags:?}"
    );
}

// ── CT-T3: method-call instantiation violations ─────────────────────────────
//
// Each positive fixture instantiates a generic method whose supported
// constraint the concrete type-arg PROVABLY violates → EXACTLY ONE diagnostic.
// Checked via the same direct-`analyze` `constraint_diags` helper (never
// run-corpus, §3.5).

/// `Use<int32>(…)` against `where T : IFace` — the **primitive** `int32`
/// provably implements no in-program interface → EXACTLY ONE diagnostic. The
/// flagship `Use<int32>` check.
#[test]
fn violate_iface_diagnoses_once() {
    let diags = local_constraint_diags("violate_iface.bf");
    assert_eq!(
        diags.len(),
        1,
        "violate_iface.bf must emit exactly one constraint diagnostic, got: {diags:?}"
    );
}

/// A value **struct** arg to a `where T : class` constraint is provably not a
/// reference type → EXACTLY ONE diagnostic.
#[test]
fn violate_class_constraint_diagnoses_once() {
    let diags = local_constraint_diags("violate_class_constraint.bf");
    assert_eq!(
        diags.len(),
        1,
        "violate_class_constraint.bf must emit exactly one constraint diagnostic, got: {diags:?}"
    );
}

/// A reference **class** arg to a `where T : struct` constraint is provably not
/// a value type → EXACTLY ONE diagnostic.
#[test]
fn violate_struct_constraint_diagnoses_once() {
    let diags = local_constraint_diags("violate_struct_constraint.bf");
    assert_eq!(
        diags.len(),
        1,
        "violate_struct_constraint.bf must emit exactly one constraint diagnostic, got: {diags:?}"
    );
}

// ── CT-T4: the multi-file ratchet-safety pin (configuration-dependence guard) ─
//
// The four `constraint_diags == 0` pins above analyze each constraint-dense file
// STANDALONE (the per-file ratchet `tests/corpus.rs` uses). Under that
// configuration the corlib interfaces `Constraints.bf` references —
// `IHashable` (`corlib-slice/IHashable.bf`), `IDisposable`
// (`corlib-slice/System.bf`) — are in SEPARATE files, so they are UNRESOLVABLE →
// the GC-T3 instantiation check SKIPS those clauses (the
// any-base-unresolvable ⇒ Skip rule, §3.2).
//
// CT-T4 is the MULTI-FILE pin (generic-constraints.md §3.3/§6.7, R1): it
// co-analyzes the corlib-slice files TOGETHER WITH `Constraints.bf` in ONE
// `analyze` call, so those interfaces become IN-PROGRAM and RESOLVABLE — at which
// point GC-T3's transitive implements/base walk ACTUALLY RUNS on
// `where K : IHashable` (`Constraints.bf:43`) instead of skipping it. The pin
// asserts STILL ZERO constraint diagnostics: this is the configuration-dependence
// guard — even with the iface/base resolvable, the conservative GC-T3 check must
// not false-positive (a `where`-clause whose bound is now resolvable in a
// multi-file config must still be seen as satisfied/skipped, never violated).
//
// This is the load-bearing assertion of the task: the per-file ratchet CANNOT
// exercise it (it deliberately keeps corlib out of scope), so without this pin a
// GC-T3 instantiation-check false positive on a now-resolvable constraint would
// go undetected until a real driver build co-analyzed corlib + the feature suite.

/// ★ CT-T4 ★ — co-analyze the WHOLE corlib slice (defining `IHashable`,
/// `IDisposable`, `IEnumerator`, `Dictionary`, the value/primitive types, …)
/// TOGETHER WITH `Constraints.bf` in ONE `analyze` call. The merged def graph
/// makes `Constraints.bf`'s referenced interfaces RESOLVABLE, so GC-T3's
/// instantiation check runs on them — and must STILL emit ZERO constraint
/// diagnostics. The configuration-dependence guard (R1).
#[test]
fn corlib_slice_plus_constraints_bf_zero_constraint_diags() {
    let mut paths = corlib_slice_files();
    paths.push(beef_tests_root().join("feature-suite/src/Constraints.bf"));
    let diags = constraint_diags_multi(&paths);
    assert!(
        diags.is_empty(),
        "co-analyzing corlib-slice + Constraints.bf (the multi-file config where \
         IHashable/IDisposable become in-program and RESOLVABLE) must STILL emit \
         zero constraint diagnostics — the configuration-dependence guard. got: {diags:?}"
    );
}

/// CT-T4 companion — co-analyze the corlib slice with `Generics.bf` too (its
/// `where T : IDisposable` clauses at `:101/113/119` become resolvable once
/// `System.bf`'s `IDisposable` is in-program). Still ZERO constraint diagnostics:
/// the multi-file guard holds for the other corlib-iface-dependent ratchet file
/// as well, not just `Constraints.bf`.
#[test]
fn corlib_slice_plus_generics_bf_zero_constraint_diags() {
    let mut paths = corlib_slice_files();
    paths.push(beef_tests_root().join("feature-suite/src/Generics.bf"));
    let diags = constraint_diags_multi(&paths);
    assert!(
        diags.is_empty(),
        "co-analyzing corlib-slice + Generics.bf (where IDisposable becomes \
         in-program and RESOLVABLE) must STILL emit zero constraint diagnostics — \
         the configuration-dependence guard. got: {diags:?}"
    );
}

/// CT-T4 — co-analyze the corlib slice with ALL FOUR constraint-dense ratchet
/// files at once (the maximal driver-build-shaped configuration: every corlib
/// type AND every feature-suite constraint clause in one merged program). The
/// strongest form of the guard — every resolvable bound across all four files is
/// validated by GC-T3 simultaneously, and the count must STILL be zero.
#[test]
fn corlib_slice_plus_all_ratchet_files_zero_constraint_diags() {
    let mut paths = corlib_slice_files();
    for name in [
        "Constraints.bf",
        "Generics.bf",
        "Generics2.bf",
        "Interfaces.bf",
    ] {
        paths.push(beef_tests_root().join("feature-suite/src").join(name));
    }
    let diags = constraint_diags_multi(&paths);
    assert!(
        diags.is_empty(),
        "co-analyzing corlib-slice + all four constraint-dense ratchet files (the \
         maximal multi-file config) must STILL emit zero constraint diagnostics — \
         the configuration-dependence guard. got: {diags:?}"
    );
}
