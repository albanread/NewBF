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

/// LLVM lowering must (1) never panic and (2) produce a module that passes
/// LLVM's own verifier, on every real Beef file. The verifier is the
/// correctness backstop: a clean verify means the typed SSA IR we emit is
/// internally consistent (types match at every use, every block is
/// terminated, phis are well-formed).
#[test]
fn llvm_lowering_verifies_on_real_beef() {
    let mut files = Vec::new();
    collect_bf(&root().join("corlib-slice"), &mut files);
    collect_bf(&root().join("feature-suite/src"), &mut files);
    assert!(!files.is_empty(), "no .bf fixtures found");

    let mut clean = 0usize;
    let mut failed: Vec<(String, String)> = Vec::new();

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
        match newbf_llvm::verify_module(&module) {
            Ok(()) => clean += 1,
            Err(e) => {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                // Keep only the first line of the verifier message.
                let first = e.lines().next().unwrap_or("").to_string();
                failed.push((name, first));
            }
        }
    }

    eprintln!(
        "llvm verify: {clean} / {} modules verified clean ({} failed)",
        files.len(),
        failed.len()
    );
    for (name, msg) in failed.iter().take(15) {
        eprintln!("    {name}: {msg}");
    }

    // Clean-verify ratchet: every Beef file must lower to a verifiable LLVM
    // module. The floor is 100%.
    assert_eq!(
        clean,
        files.len(),
        "llvm clean-verify coverage regressed below 100%: {clean} / {}",
        files.len()
    );
}

/// MX-T1 R7 ratchet (mixins.md §7): `feature-suite/src/Mixins.bf` densely uses
/// mixin syntax. After MX-T1 the parser emits the new `Expr::MixinCall` /
/// `Stmt::MixinDecl` / `Member::Mixin` variants instead of masquerading them as
/// calls / local-fns / methods, yet sema must still IGNORE them so the file
/// lowers to a *verifiable* LLVM module exactly as before (mixin EXPANSION is
/// MX-T3). This pins both halves: (1) the parser actually produces the new
/// variants on this file (so the rewire is exercised), and (2) the module still
/// verifies clean (the load-bearing behavior-preservation gate).
#[test]
fn mixins_bf_parses_to_mixin_variants_and_still_verifies() {
    use newbf_parser::{Expr, Member, Stmt};

    let path = root().join("feature-suite/src/Mixins.bf");
    let src = std::fs::read_to_string(&path).expect("Mixins.bf present in the verify corpus");
    let (unit, pdiags) = parse_file(&src, FileId(0));
    assert!(pdiags.is_empty(), "Mixins.bf must parse clean: {pdiags:?}");

    // The parser must have routed the mixin syntax to the new variants.
    let mut saw_member_mixin = false;
    let mut saw_stmt_mixin = false;
    let mut saw_mixin_call = false;

    fn walk_stmt(s: &Stmt, st: &mut bool, sc: &mut bool) {
        if let Stmt::MixinDecl { .. } = s {
            *st = true;
        }
        // A shallow walk is enough: the corpus has mixin calls at statement and
        // nested-statement level, and `MixinDecl` at block level.
        match s {
            Stmt::Block { stmts, .. } => stmts.iter().for_each(|x| walk_stmt(x, st, sc)),
            Stmt::Expr { expr, .. } => walk_expr(expr, sc),
            Stmt::Local { init: Some(e), .. } | Stmt::Return { value: Some(e), .. } => {
                walk_expr(e, sc)
            }
            Stmt::If { then, els, .. } => {
                walk_stmt(then, st, sc);
                if let Some(e) = els {
                    walk_stmt(e, st, sc);
                }
            }
            Stmt::While { body, .. }
            | Stmt::DoWhile { body, .. }
            | Stmt::For { body, .. }
            | Stmt::ForEach { body, .. }
            | Stmt::Defer { body, .. }
            | Stmt::MixinDecl { body, .. }
            | Stmt::LocalFunction { body, .. } => walk_stmt(body, st, sc),
            _ => {}
        }
    }
    fn walk_expr(e: &Expr, sc: &mut bool) {
        match e {
            Expr::MixinCall { args, .. } => {
                *sc = true;
                args.iter().for_each(|a| walk_expr(a, sc));
            }
            Expr::Call { callee, args, .. } => {
                walk_expr(callee, sc);
                args.iter().for_each(|a| walk_expr(a, sc));
            }
            Expr::Binary { lhs, rhs, .. } | Expr::Assign { target: lhs, value: rhs, .. } => {
                walk_expr(lhs, sc);
                walk_expr(rhs, sc);
            }
            Expr::Member { base, .. } => walk_expr(base, sc),
            Expr::Paren { inner, .. } => walk_expr(inner, sc),
            _ => {}
        }
    }

    fn walk_members(members: &[Member], mm: &mut bool, st: &mut bool, sc: &mut bool) {
        for m in members {
            match m {
                Member::Mixin { .. } => *mm = true,
                Member::Method {
                    body: newbf_parser::MethodBody::Block(s),
                    ..
                } => walk_stmt(s, st, sc),
                Member::Nested(td) => walk_members(&td.members, mm, st, sc),
                _ => {}
            }
        }
    }
    fn walk_items(items: &[newbf_parser::Item], mm: &mut bool, st: &mut bool, sc: &mut bool) {
        for it in items {
            match it {
                newbf_parser::Item::Namespace {
                    body: Some(b), ..
                } => walk_items(b, mm, st, sc),
                newbf_parser::Item::Type(td) => walk_members(&td.members, mm, st, sc),
                _ => {}
            }
        }
    }
    walk_items(
        &unit.items,
        &mut saw_member_mixin,
        &mut saw_stmt_mixin,
        &mut saw_mixin_call,
    );

    assert!(
        saw_member_mixin,
        "MX-T1: Mixins.bf must parse member mixins to Member::Mixin"
    );
    assert!(
        saw_stmt_mixin,
        "MX-T1: Mixins.bf must parse the local `mixin AppendAndNullify` to Stmt::MixinDecl"
    );
    assert!(
        saw_mixin_call,
        "MX-T1: Mixins.bf must parse `Name!(args)` to Expr::MixinCall"
    );

    // The R7 load-bearing half: the module must still verify clean.
    let srcs = [SourceFile {
        file: FileId(0),
        src: &src,
        unit: &unit,
    }];
    let program = analyze(&srcs);
    let module = lower_program(&srcs, &program);
    newbf_llvm::verify_module(&module).expect("MX-T1 R7: Mixins.bf must still verify clean");
}

/// Parse → analyze → lower → LLVM-verify a single in-memory program.
fn verify_src(src: &str) -> Result<(), String> {
    let (unit, _pdiags) = parse_file(src, FileId(0));
    let srcs = [SourceFile {
        file: FileId(0),
        src,
        unit: &unit,
    }];
    let program = analyze(&srcs);
    let module = lower_program(&srcs, &program);
    newbf_llvm::verify_module(&module)
}

/// MX-T3 decline pin #1 (mixins.md §3.4 static-caller guard): a mixin body that
/// references `this` while the CALLER is a static method declines
/// (`ReferencesThisStatically`) — `Expr::This` would otherwise yield `undef(Ptr)`.
/// The gate returns `None` and the call falls back to the existing verifiable path
/// (the synthetic `Call`), so the module still verifies clean (no panic, no novel
/// IR). This pins that turning expansion ON does NOT regress the static-`this`
/// shape.
#[test]
fn mx_t3_static_this_mixin_declines_and_verifies() {
    let src = "\
class C {
	int32 field = 5;
	static mixin Touch() {
		this.field += 1;
	}
	public static int32 Main() {
		Touch!();
		return 0;
	}
}
";
    verify_src(src).expect("MX-T3: a static-context `this`-mixin call declines and verifies clean");
}

/// MX-T3 decline pin #2 (mixins.md §3.5): a generic mixin used as an UNTARGETED
/// sub-expression (`Wrap!<int32>(x) + 1`) declines (`Generic`, via the call's
/// `type_args`) and falls back to the existing verifiable path — the untargeted
/// expression-mixin position lowers without panic and the module verifies clean.
/// (A non-generic untargeted sub-expression instead EXPANDS via the single-pass
/// inferred-type path, §3.5 — also verifiable; this pin uses the generic form to
/// exercise the decline-and-fallback at an untargeted position specifically.)
#[test]
fn mx_t3_untargeted_subexpr_mixin_declines_and_verifies() {
    let src = "\
class C {
	static mixin Wrap<T>(T x) => x;
	public static int32 Main() {
		int32 y = Wrap!<int32>(20) + 1;
		return y;
	}
}
";
    verify_src(src)
        .expect("MX-T3: an untargeted (generic) sub-expression mixin declines and verifies clean");
}

/// MX-T5 `.Err`-branch verify pin (mixins.md §3.7 / §8): a program that reaches
/// the PRELUDE `Result<T, E>`'s `.Err → default` arm in IR (it `Unwrap`s an `.Err`
/// value) must lower verifier-clean. v1 has NO `Internal.FatalError`, so the error
/// arm returns `default` (zeroed `T`) and emits NO unresolved symbol — a clean
/// verify confirms both: the error arm + the `default` it needs build, and nothing
/// dangles. This program declares NO `Result` of its own, so it exercises the
/// canonical prelude type the whole corpus now shares.
#[test]
fn mx_t5_result_err_arm_lowers_clean() {
    let src = "\
class Program {
	public static int32 Main() {
		Result<int32, bool> err = Result<int32, bool>.Err(true);
		// Both the method and the property exercise the `.Err → default` arm.
		int32 a = err.Unwrap();
		int32 b = err.Value;
		return a + b;
	}
}
";
    verify_src(src).expect("MX-T5: the prelude Result's `.Err → default` arm lowers verifier-clean");
}

/// MX-T5 single-param `.Err` verify pin: the prelude's convenience `Result<T>`
/// (payloadless `.Err`) — a DISTINCT arity from `Result<T, E>` — also lowers its
/// `.Err → default` arm clean. Pins that the (name, arity)-keyed generic-decl
/// resolution picks `Result<T>` for the 1-arg use (not the 2-arg decl).
#[test]
fn mx_t5_result_single_param_err_arm_lowers_clean() {
    let src = "\
class Program {
	public static int32 Main() {
		Result<int32> err = Result<int32>.Err;
		int32 a = err.Unwrap();
		int32 b = err.Value;
		return a + b;
	}
}
";
    verify_src(src)
        .expect("MX-T5: the prelude Result<T> (single param) `.Err` arm lowers verifier-clean");
}
