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

use newbf_comptime::{eval_const_i64, fold_comptime};
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
