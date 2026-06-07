//! Source-level comptime: prove that a `[Comptime]` function written in real
//! Beef is *evaluated at compile time* by the JIT, not interpreted. This extends
//! the `newbf-comptime` unit test (which hand-builds IR) to the whole frontend —
//! parse → analyze → lower → JIT-eval — on actual `.bf` source.
//!
//! `[Comptime]` is metadata-only today, so the function lowers to an ordinary
//! nullary `i64()` symbol; `eval_const_i64` JIT-compiles the module and calls it
//! during "compilation" (here, the test). The folding of comptime call sites into
//! literals in the surrounding program is the next increment — this nails the
//! evaluation half end-to-end on real source first.

use newbf_comptime::{eval_const_i64, fold_comptime, run_emission};
use newbf_lexer::FileId;
use newbf_llvm::OrcJit;
use newbf_parser::parse_file;
use newbf_sema::{SourceFile, analyze, lower_program};

/// Parse + analyze + lower `src`, then JIT-evaluate the comptime function `name`.
fn eval_comptime(src: &str, name: &str) -> i64 {
    let (unit, pdiags) = parse_file(src, FileId(0));
    assert!(pdiags.is_empty(), "parse diagnostics: {pdiags:?}");
    let files = [SourceFile {
        file: FileId(0),
        src,
        unit: &unit,
    }];
    let program = analyze(&files);
    let module = lower_program(&files, &program);
    eval_const_i64(&module, name).expect("comptime evaluation succeeds")
}

#[test]
fn comptime_loop_evaluates_at_compile_time() {
    // A non-trivial body (a 1..=100 accumulator) so the value can't be a stray
    // literal — the JIT actually runs the loop during compilation.
    let src = r#"
        class Program {
            [Comptime]
            public static int Sum() {
                int s = 0;
                for (int i = 1; i <= 100; i++) { s = s + i; }
                return s;
            }
            public static int32 Main() { return 0; }
        }
    "#;
    assert_eq!(eval_comptime(src, "Program.Sum"), 5050);
}

#[test]
fn comptime_recursion_evaluates_at_compile_time() {
    // Recursion exercises a real call graph inside the JIT'd comptime function.
    let src = r#"
        class Program {
            [Comptime]
            public static int Fib() { return Rec(20); }
            public static int Rec(int n) {
                if (n < 2) { return n; }
                return Rec(n - 1) + Rec(n - 2);
            }
            public static int32 Main() { return 0; }
        }
    "#;
    // fib(20) = 6765.
    assert_eq!(eval_comptime(src, "Program.Fib"), 6765);
}

#[test]
fn comptime_call_folds_into_caller() {
    // `Main` calls a `[Comptime]` function. After folding, the call site is the
    // computed literal and the comptime function is *gone* from the module — yet
    // `Main` still runs and returns the value. That a now-removed symbol's result
    // survives is the proof the call was folded at compile time, not called at
    // run time (an un-folded call would dangle and fail to JIT-resolve).
    let src = r#"
        class Program {
            [Comptime]
            public static int Sum() {
                int s = 0;
                for (int i = 1; i <= 100; i++) { s = s + i; }
                return s;
            }
            public static int32 Main() { return (int32)Sum(); }
        }
    "#;
    let (unit, pdiags) = parse_file(src, FileId(0));
    assert!(pdiags.is_empty(), "parse diagnostics: {pdiags:?}");
    let files = [SourceFile {
        file: FileId(0),
        src,
        unit: &unit,
    }];
    let program = analyze(&files);
    let mut module = lower_program(&files, &program);

    // Pre-fold: sema marked the comptime symbol and the function exists.
    assert!(module.comptime.iter().any(|s| s == "Program.Sum"));
    assert!(module.funcs.iter().any(|f| f.name == "Program.Sum"));

    fold_comptime(&mut module).expect("comptime fold succeeds");

    // Post-fold: the compile-time-only function is dropped.
    assert!(
        !module.funcs.iter().any(|f| f.name == "Program.Sum"),
        "comptime function should be removed after folding"
    );

    // And `Main` still returns the folded value — JIT-resolving a module that no
    // longer contains `Program.Sum` only succeeds because the call was folded.
    let jit = OrcJit::from_ir(&module).expect("jit builds");
    let addr = jit.lookup("Program.Main").expect("Program.Main resolves");
    let main: extern "C" fn() -> i32 = unsafe { std::mem::transmute(addr) };
    assert_eq!(main(), 5050);
}

#[test]
fn comptime_call_with_const_arg_folds() {
    // A `[Comptime]` function called with a *constant* argument folds via the
    // synthesized wrapper: `Factorial(5)` evaluates to 120 at compile time, the
    // call site becomes the literal, and the (recursive) comptime function is
    // dropped — proven the same way: it's absent yet `Main` still runs.
    let src = r#"
        class Program {
            [Comptime]
            public static int Factorial(int n) {
                if (n <= 1) { return 1; }
                return n * Factorial(n - 1);
            }
            public static int32 Main() { return (int32)Factorial(5); }
        }
    "#;
    let (unit, pdiags) = parse_file(src, FileId(0));
    assert!(pdiags.is_empty(), "parse diagnostics: {pdiags:?}");
    let files = [SourceFile {
        file: FileId(0),
        src,
        unit: &unit,
    }];
    let program = analyze(&files);
    let mut module = lower_program(&files, &program);
    assert!(module.funcs.iter().any(|f| f.name == "Program.Factorial"));

    fold_comptime(&mut module).expect("comptime fold succeeds");

    assert!(
        !module.funcs.iter().any(|f| f.name == "Program.Factorial"),
        "comptime function should be removed after folding"
    );
    let jit = OrcJit::from_ir(&module).expect("jit builds");
    let addr = jit.lookup("Program.Main").expect("Program.Main resolves");
    let main: extern "C" fn() -> i32 = unsafe { std::mem::transmute(addr) };
    assert_eq!(main(), 120);
}

// ── CB-T4: comptime member EMISSION (the fixpoint loop) ───────────────────────

/// The §1 marquee source: a `[Comptime, EmitGenerator]` emits a `Sum()` method
/// that reads pre-existing fields. The emitted member resolves + is callable and
/// reads the original fields → 42 (the only path to that value).
const EMIT_MEMBER_SRC: &str = r#"
    class Vec2 {
        public int32 mX;
        public int32 mY;
        public this(int32 x, int32 y) { this.mX = x; this.mY = y; }

        [Comptime, EmitGenerator]
        public static void Generate() {
            Compiler.EmitTypeBody("public int32 Sum() { return this.mX + this.mY; }");
        }
    }
    class Program {
        public static int32 Main() {
            Vec2 v = new Vec2(30, 12);
            int32 r = v.Sum();
            delete v;
            return r;
        }
    }
"#;

/// Drive emission over a one-file program and return the final module.
fn emit_module(src: &str) -> newbf_ir::Module {
    let (unit, pdiags) = parse_file(src, FileId(0));
    assert!(pdiags.is_empty(), "parse diagnostics: {pdiags:?}");
    let files = [SourceFile {
        file: FileId(0),
        src,
        unit: &unit,
    }];
    let (module, _outcome) = run_emission(&files).expect("comptime emission succeeds");
    module
}

/// **The CB-T4 marquee, full frontend.** Emission feeds back into resolution: the
/// generator emits `Sum()` (reading pre-existing `mX`/`mY`), the compiler
/// re-resolves `Vec2` via the spliced `extension`, and `Main` calls the emitted
/// method → 42.
#[test]
fn comptime_emit_member_returns_42() {
    let module = emit_module(EMIT_MEMBER_SRC);
    let jit = OrcJit::from_ir(&module).expect("final module JIT-links clean");
    let addr = jit.lookup("Program.Main").expect("Program.Main resolves");
    let main: extern "C" fn() -> i32 = unsafe { std::mem::transmute(addr) };
    assert_eq!(main(), 42, "emitted member reads pre-existing fields → 42");
}

/// **The strip + link-clean assertion (R6).** After emission the final module
/// must contain the generated symbol but NEITHER the `[EmitGenerator]` generator
/// NOR the `__newbf_ct_emit` extern — because the app/run JIT and the AOT link do
/// NOT register the shim, so a survivor fails `lookup`/link. Assert both the IR
/// shape (the `dump-ir` golden: generated present, generator + shim absent) AND
/// that the module JIT-links **and** AOT-emits cleanly.
#[test]
fn comptime_emit_strips_generator_and_shim_links_clean() {
    let module = emit_module(EMIT_MEMBER_SRC);

    // dump-ir golden: the generated `Sum` is present.
    assert!(
        module.funcs.iter().any(|f| f.name == "Vec2.Sum"),
        "generated member `Vec2.Sum` must be present in the final IR (have: {:?})",
        module.funcs.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    // The generator is absent (stripped).
    assert!(
        !module.funcs.iter().any(|f| f.name.contains("Generate")),
        "the [EmitGenerator] generator must be ABSENT from the final IR"
    );
    // The `__newbf_ct_emit` shim extern is absent (stripped).
    assert!(
        !module.funcs.iter().any(|f| f.name == "__newbf_ct_emit"),
        "the __newbf_ct_emit extern must be ABSENT from the final IR"
    );
    // No leftover emit jobs.
    assert!(module.emit_jobs.is_empty(), "emit_jobs consumed");

    // JIT-links clean (RTDyld eager-links the whole module on lookup — a
    // surviving `__newbf_ct_emit` extern would fail here with "Symbols not
    // found").
    let jit = OrcJit::from_ir(&module).expect("final module JIT-links with no unresolved symbols");
    assert!(
        jit.lookup("Program.Main").is_some(),
        "Program.Main resolves in the JIT-linked module"
    );

    // AOT-emits clean: object emission of a module with a dangling extern would
    // still emit (undefined externs only fail at LINK), so additionally assert
    // the IR holds no `__newbf_ct_emit` declaration (checked above) AND the
    // object emits without error (the codegen path the AOT pipeline runs).
    let obj = newbf_llvm::emit_object_to_memory(&module)
        .expect("final module AOT-emits an object with no codegen error");
    assert!(!obj.is_empty(), "AOT object is non-empty");
}

/// **Dead-emitted-member link regression (R6).** A generator emits a member that
/// is NEVER called; the generator still ran (so the shim existed during
/// emission), but the stripped final module must still JIT-link and run.
#[test]
fn comptime_emit_dead_member_still_links() {
    let src = r#"
        class Widget {
            public int32 mZ;
            public this(int32 z) { this.mZ = z; }
            [Comptime, EmitGenerator]
            public static void Generate() {
                Compiler.EmitTypeBody("public int32 Unused() { return this.mZ + 99; }");
            }
        }
        class Program {
            public static int32 Main() {
                Widget w = new Widget(123);
                delete w;
                return 7;
            }
        }
    "#;
    let module = emit_module(src);
    assert!(
        !module.funcs.iter().any(|f| f.name == "__newbf_ct_emit"),
        "shim stripped even though the emitted member is never called"
    );
    let jit = OrcJit::from_ir(&module).expect("dead-emitted-member module JIT-links clean");
    let addr = jit.lookup("Program.Main").expect("Program.Main resolves");
    let main: extern "C" fn() -> i32 = unsafe { std::mem::transmute(addr) };
    assert_eq!(main(), 7);
}

// ── CB-T5: fixpoint guards + diagnostics (public-API integration) ─────────────

/// **Abort on generated-code analyze diagnostics (CB-T5 acceptance), full
/// frontend through the public `run_emission`.** A `[Comptime, EmitGenerator]`
/// emits MALFORMED member text: two identical fields. Once spliced as
/// `extension Bag { … }` and re-analyzed, this trips analyze's duplicate-member
/// check, so emission must STOP and surface the analyze diagnostic in
/// `EmitOutcome.diagnostics` — NOT lower garbage IR (a silent miscompile) and
/// NOT loop forever.
///
/// NOTE: the comptime-breadth doc frames this as a "missing-field" emission, but
/// this compiler's `analyze` does not resolve method bodies (it checks usings,
/// duplicate types, duplicate fields/cases, generic-method guards), so a missing
/// field surfaces only at lowering (as `undef`), not as an analyze diagnostic.
/// A duplicate field is the equivalent analyze-catchable malformed emission; the
/// abort MECHANISM under test (generated-code analyze diagnostics → surface +
/// stop) is identical regardless of which analyze rule the bad emission trips.
#[test]
fn comptime_emit_malformed_aborts_with_analyze_diagnostic() {
    let src = r#"
        class Bag {
            public int32 mN;
            [Comptime, EmitGenerator]
            public static void Generate() {
                Compiler.EmitTypeBody("public int32 dupF; public int32 dupF;");
            }
        }
        class Program {
            public static int32 Main() { return 0; }
        }
    "#;
    let (unit, pdiags) = parse_file(src, FileId(0));
    assert!(pdiags.is_empty(), "parse diagnostics: {pdiags:?}");
    let files = [SourceFile {
        file: FileId(0),
        src,
        unit: &unit,
    }];
    // Emission returns Ok (no crash, no hang, no Err) but with diagnostics — the
    // driver/harness surfaces those as a failure.
    let (_module, outcome) = run_emission(&files).expect("emission returns Ok (diagnostics, not Err)");
    assert!(
        !outcome.diagnostics.is_empty(),
        "a malformed (analyze-erroring) emission surfaces a diagnostic, not a silent miscompile"
    );
    assert!(
        outcome.diagnostics.iter().any(|d| d.contains("duplicate member")),
        "the underlying analyze diagnostic is surfaced: {:?}",
        outcome.diagnostics
    );
}
