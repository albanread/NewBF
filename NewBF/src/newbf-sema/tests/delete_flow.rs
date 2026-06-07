//! MS-T5 — compile-time delete-flow (Track B) acceptance tests.
//!
//! These exercise `newbf_sema::analyze`'s new `check_delete_flow` pass (run after
//! `resolve_and_check`, appending to `Program.diagnostics`). They load real `.bf`
//! fixtures under `tests/ownership/` — kept OUT of the auto-collected
//! `beef-tests/` corpus precisely because they are *expected* to diagnose — and
//! assert the exact double-free / scope-delete diagnostic count, plus the
//! zero-diagnostic negatives (single delete, reassign-between, conditional).
//!
//! The whole-corpus zero-false-positive ratchet itself lives in
//! `tests/corpus.rs::sema_does_not_panic_on_real_beef` (it asserts EVERY
//! `beef-tests` file analyses with zero diagnostics — which now includes this
//! pass), so a false positive there is a hard failure.

use std::path::PathBuf;

use newbf_lexer::FileId;
use newbf_parser::parse_file;
use newbf_sema::{Diagnostic, SourceFile, analyze};

fn fixture(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/ownership")
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Analyze a fixture and return only the delete-flow ("provable double-free")
/// diagnostics — filtered so an unrelated future diagnostic can't perturb the
/// count.
fn double_free_diags(name: &str) -> Vec<Diagnostic> {
    let src = fixture(name);
    let (unit, pdiags) = parse_file(&src, FileId(0));
    assert!(pdiags.is_empty(), "{name} must parse clean: {pdiags:?}");
    let program = analyze(&[SourceFile {
        file: FileId(0),
        src: &src,
        unit: &unit,
    }]);
    program
        .diagnostics
        .into_iter()
        .filter(|d| d.message.contains("double-free"))
        .collect()
}

/// Analyze a fixture and return only the leak ("provable leak") diagnostics.
fn leak_diags(name: &str) -> Vec<Diagnostic> {
    let src = fixture(name);
    let (unit, pdiags) = parse_file(&src, FileId(0));
    assert!(pdiags.is_empty(), "{name} must parse clean: {pdiags:?}");
    let program = analyze(&[SourceFile {
        file: FileId(0),
        src: &src,
        unit: &unit,
    }]);
    program
        .diagnostics
        .into_iter()
        .filter(|d| d.message.contains("provable leak"))
        .collect()
}

// ── positives ────────────────────────────────────────────────────────────────

#[test]
fn provable_double_free_is_diagnosed_exactly_once() {
    let diags = double_free_diags("provable_double_free.bf");
    assert_eq!(
        diags.len(),
        1,
        "expected exactly one provable double-free diagnostic, got: {diags:?}"
    );
    assert!(
        diags[0].message.contains("'p' is deleted again"),
        "message should name the re-deleted binding: {:?}",
        diags[0].message
    );
}

#[test]
fn scope_bound_delete_is_diagnosed_exactly_once() {
    let diags = double_free_diags("scope_delete.bf");
    assert_eq!(
        diags.len(),
        1,
        "expected exactly one scope-delete diagnostic, got: {diags:?}"
    );
    assert!(
        diags[0].message.contains("scope-allocated"),
        "message should describe the scope double-free: {:?}",
        diags[0].message
    );
}

// ── MS-T6: provable leak (positive) ──────────────────────────────────────────

#[test]
fn provable_leak_is_diagnosed_exactly_once() {
    let diags = leak_diags("provable_leak.bf");
    assert_eq!(
        diags.len(),
        1,
        "expected exactly one provable-leak diagnostic, got: {diags:?}"
    );
    assert!(
        diags[0].message.contains("'p'"),
        "message should name the leaked binding: {:?}",
        diags[0].message
    );
}

// ── MS-T6: leak negatives (every resolved disposition must stay silent) ───────

#[test]
fn leak_negatives_are_all_silent() {
    let diags = leak_diags("leak_negatives.bf");
    assert!(
        diags.is_empty(),
        "deleted / scoped / returned / aliased / field-stored / address-taken / \
         captured `new`s must NOT be flagged as leaks, got: {diags:?}"
    );
}

// ── negatives (must stay silent) ─────────────────────────────────────────────

#[test]
fn single_balanced_delete_is_silent() {
    assert!(
        double_free_diags("single_delete_ok.bf").is_empty(),
        "a single balanced delete must not be diagnosed"
    );
}

#[test]
fn reassignment_between_deletes_is_silent() {
    assert!(
        double_free_diags("reassigned_delete_ok.bf").is_empty(),
        "a reassignment between two deletes resets the lattice — no double-free"
    );
}

#[test]
fn conditional_delete_join_is_silent() {
    assert!(
        double_free_diags("conditional_delete_ok.bf").is_empty(),
        "a delete that is Deleted on only some paths must not be flagged (conservative join)"
    );
}

// ── in-memory micro-cases (exercise the lattice corners directly) ─────────────

fn double_free_diags_src(src: &str) -> Vec<Diagnostic> {
    let (unit, pdiags) = parse_file(src, FileId(0));
    assert!(pdiags.is_empty(), "must parse clean: {pdiags:?}");
    let program = analyze(&[SourceFile {
        file: FileId(0),
        src,
        unit: &unit,
    }]);
    program
        .diagnostics
        .into_iter()
        .filter(|d| d.message.contains("double-free"))
        .collect()
}

/// A binding passed as an argument between two deletes is conservatively
/// untracked (Beef passes by reference, but the analysis can't follow the
/// callee), so the second delete is NOT flagged — no false positive.
#[test]
fn arg_pass_untracks_then_delete_is_silent() {
    let src = "\
class Node { public int32 value; }
class Program {
    static void Use(Node n) { }
    public static int32 Main() {
        let p = new Node();
        delete p;
        Use(p);
        delete p;
        return 0;
    }
}
";
    assert!(
        double_free_diags_src(src).is_empty(),
        "a use between deletes untracks the binding — must be silent"
    );
}

/// A `new` of a value `struct` is not an owning-class allocation, so deleting
/// such a binding twice is never tracked (struct types are not heap owners here).
#[test]
fn struct_new_is_not_tracked() {
    let src = "\
struct Vec2 { public int32 x; }
class Program {
    public static int32 Main() {
        let v = new Vec2();
        delete v;
        delete v;
        return 0;
    }
}
";
    assert!(
        double_free_diags_src(src).is_empty(),
        "a `new` of a value struct must not be tracked as an owning class"
    );
}

/// String interpolation and array literals are compiler-synthesized allocations
/// (§B1) — never tracked, so they can never trip the rule.
#[test]
fn synthesized_allocations_are_not_tracked() {
    let src = "\
class Program {
    public static int32 Main() {
        let arr = new int32[4];
        delete arr;
        let s = $\"x={arr[0]}\";
        delete s;
        return 0;
    }
}
";
    assert!(
        double_free_diags_src(src).is_empty(),
        "array/String sugar allocations must not be tracked as owning classes"
    );
}

/// The straight-line double-free is still caught when the binding is a typed
/// `Node x = new Node();` (not just `let`), proving the local-type map handles
/// both declaration forms.
#[test]
fn typed_local_double_free_is_caught() {
    let src = "\
class Node { public int32 value; }
class Program {
    public static int32 Main() {
        Node x = new Node();
        delete x;
        delete x;
        return 0;
    }
}
";
    assert_eq!(
        double_free_diags_src(src).len(),
        1,
        "a typed-local double-free must be caught exactly once"
    );
}
