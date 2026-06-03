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

use newbf_comptime::eval_const_i64;
use newbf_lexer::FileId;
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
