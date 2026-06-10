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
        name: "",
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
        name: "",
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
        name: "",
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

// ── CB-T6: widened-int folds + fold-width fix + inner-fold-first ──────────────

/// **The CB-T6 fold-width proof, full frontend.** A `[Comptime] int32 F(int32 x)
/// => x*x` called as `F(7)` folds to the `i32` constant 49 — and the folded
/// module is **verify-clean** (the literal carries the call's own `i32` width, not
/// a hardcoded `i64`, so every SSA use stays width-consistent). `F` is dropped
/// (compile-time only) yet `Main` still returns 49: the proof it was folded, not
/// run.
#[test]
fn comptime_i32_arg_folds_width_correct_and_verify_clean() {
    let src = r#"
        class Program {
            [Comptime]
            public static int32 F(int32 x) { return x * x; }
            public static int32 Main() { return F(7); }
        }
    "#;
    let (unit, pdiags) = parse_file(src, FileId(0));
    assert!(pdiags.is_empty(), "parse diagnostics: {pdiags:?}");
    let files = [SourceFile {
        file: FileId(0),
        src,
        unit: &unit,
        name: "",
    }];
    let program = analyze(&files);
    let mut module = lower_program(&files, &program);
    assert!(module.funcs.iter().any(|f| f.name == "Program.F"));

    fold_comptime(&mut module).expect("comptime fold succeeds");

    // The comptime function is dropped (compile-time only).
    assert!(
        !module.funcs.iter().any(|f| f.name == "Program.F"),
        "the i32 comptime function should be removed after folding"
    );
    // The folded module is LLVM-verify-clean — the width fix's load-bearing
    // assertion: a hardcoded-i64 literal in an i32 slot would fail verify here.
    newbf_llvm::verify_module(&module)
        .expect("CB-T6: the i32-folded module must verify clean (width-correct literal)");

    // And `Main` still returns 49 (an unfolded call to the dropped `F` would
    // dangle and fail to JIT-resolve).
    let jit = OrcJit::from_ir(&module).expect("jit builds");
    let addr = jit.lookup("Program.Main").expect("Program.Main resolves");
    let main: extern "C" fn() -> i32 = unsafe { std::mem::transmute(addr) };
    assert_eq!(main(), 49);
}

/// **Inner-fold-first / fixpoint, full frontend.** `Outer(Inner(3))` where both
/// are `[Comptime]` folds bottom-up across fixpoint passes: `Inner(3)` → 4, then
/// `Outer(4)` → 40. Both comptime functions are dropped and `Main` returns the
/// single collapsed literal (3+1)*10 = 40.
#[test]
fn comptime_nested_calls_fold_inner_first() {
    let src = r#"
        class Program {
            [Comptime]
            public static int32 Inner(int32 x) { return x + 1; }
            [Comptime]
            public static int32 Outer(int32 y) { return y * 10; }
            public static int32 Main() { return Outer(Inner(3)); }
        }
    "#;
    let (unit, pdiags) = parse_file(src, FileId(0));
    assert!(pdiags.is_empty(), "parse diagnostics: {pdiags:?}");
    let files = [SourceFile {
        file: FileId(0),
        src,
        unit: &unit,
        name: "",
    }];
    let program = analyze(&files);
    let mut module = lower_program(&files, &program);

    fold_comptime(&mut module).expect("comptime fold succeeds");

    // Both nested comptime functions are dropped (fully collapsed inner-first).
    assert!(
        !module.funcs.iter().any(|f| f.name == "Program.Inner"),
        "Inner should be folded away"
    );
    assert!(
        !module.funcs.iter().any(|f| f.name == "Program.Outer"),
        "Outer should be folded away (its arg became a constant after Inner folded)"
    );
    newbf_llvm::verify_module(&module).expect("CB-T6: the nested-folded module must verify clean");

    let jit = OrcJit::from_ir(&module).expect("jit builds");
    let addr = jit.lookup("Program.Main").expect("Program.Main resolves");
    let main: extern "C" fn() -> i32 = unsafe { std::mem::transmute(addr) };
    assert_eq!(main(), 40);
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
        name: "",
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

// ── CR-T3: reflection-driven codegen (the comptime-reflection marquee) ────────

/// The CR-T3 marquee source (the run-corpus `comptime_reflect_field_count.bf`):
/// the generator reads `typeof(Pair).GetFieldCount()` AT COMPILE TIME and emits a
/// member whose body returns that count. The emitted `FieldCount()`'s return value
/// is itself a compile-time reflection read — reflection drives code generation.
const REFLECT_FIELD_COUNT_SRC: &str = r#"
    [Reflect(.Fields)]
    class Pair {
        public int32 mA;
        public int32 mB;

        [Comptime, EmitGenerator]
        public static void Generate() {
            int n = typeof(Pair).GetFieldCount();      // 2 (widened to int = i64)
            String s = new String("public int32 FieldCount() { return ");
            s.Append(n);                               // "...return 2" (Append(int), decimal)
            s.Append("; }");
            Compiler.EmitTypeBody(s);                  // runtime String, NOT a literal
            delete s;                                  // exactly once → no double-free
        }
    }
    class Program {
        public static int32 Main() {
            Pair p = new Pair();
            int32 r = p.FieldCount();                  // the EMITTED member returns 2
            delete p;
            return r;
        }
    }
"#;

/// **CR-T3 — the reflection-driven-codegen marquee (full frontend).** The
/// generator reflects `typeof(Pair).GetFieldCount()` in the emission sandbox and
/// emits `FieldCount() { return 2; }`; `Main` calls it → 2. The value is
/// computable only if the sandbox saw the two reflected fields AND the runtime-
/// `String` `Compiler.EmitTypeBody(...)` path (CR-T0) carried the computed text out
/// and re-resolved it as `extension Pair { … }`. Running through `run_emission` /
/// `OrcJit` exercises the generator under the same `GuardMode::Stomp` the
/// run-corpus harness uses — a double-free in the generator (R10) would fault here.
#[test]
fn comptime_reflect_field_count_returns_2() {
    newbf_llvm::set_guard_mode(newbf_llvm::GuardMode::Stomp);
    let module = emit_module(REFLECT_FIELD_COUNT_SRC);
    let jit = OrcJit::from_ir(&module).expect("final module JIT-links clean");
    let addr = jit.lookup("Program.Main").expect("Program.Main resolves");
    let main: extern "C" fn() -> i32 = unsafe { std::mem::transmute(addr) };
    assert_eq!(
        main(),
        2,
        "the emitted FieldCount() returns the compile-time-reflected field count"
    );
    newbf_llvm::guard_reset();
}

/// **CR-T3 — the strip + JIT/AOT link-clean assertion for reflection-driven
/// emission (R10/R7).** After emission the final module must contain the generated
/// `Pair.FieldCount` but NEITHER the `[EmitGenerator]` generator NOR the
/// `__newbf_ct_emit` shim (the app/run JIT and the AOT link do NOT register the
/// shim, so a survivor would fail link). The corlib reflection methods the
/// generator pulled in (`Type.GetFieldCount`, …) SURVIVE — they are ordinary corlib
/// code, not `module.comptime`. Asserts the IR shape AND that the module both
/// JIT-links and AOT-emits cleanly.
#[test]
fn comptime_reflect_field_count_strips_generator_and_shim_links_clean() {
    let module = emit_module(REFLECT_FIELD_COUNT_SRC);

    // The generated member is present.
    assert!(
        module.funcs.iter().any(|f| f.name == "Pair.FieldCount"),
        "generated member `Pair.FieldCount` must be present (have: {:?})",
        module.funcs.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    // The corlib reflection API the generator reflected through survives the strip
    // (it is ordinary corlib code, not module.comptime).
    assert!(
        module
            .funcs
            .iter()
            .any(|f| f.name.contains("Type.GetFieldCount")),
        "corlib `Type.GetFieldCount` must survive the strip"
    );
    // The generator + shim are stripped.
    assert!(
        !module.funcs.iter().any(|f| f.name.contains("Generate")),
        "the [EmitGenerator] generator must be ABSENT from the final IR"
    );
    assert!(
        !module.funcs.iter().any(|f| f.name == "__newbf_ct_emit"),
        "the __newbf_ct_emit shim must be ABSENT from the final IR"
    );
    assert!(module.emit_jobs.is_empty(), "emit_jobs consumed");

    // JIT-links clean (a surviving `__newbf_ct_emit` extern would fail lookup).
    let jit = OrcJit::from_ir(&module).expect("final module JIT-links with no unresolved symbols");
    assert!(
        jit.lookup("Program.Main").is_some(),
        "Program.Main resolves in the JIT-linked module"
    );
    // AOT-emits clean.
    let obj = newbf_llvm::emit_object_to_memory(&module)
        .expect("final module AOT-emits an object with no codegen error");
    assert!(!obj.is_empty(), "AOT object is non-empty");
}

/// **CR-T3 — the strip differential at comptime (R10).** An UNMARKED type reflects
/// `GetFieldCount() == 0` (the `%struct.Type` global still exists, only the
/// FieldInfo array is policy-gated), so the generator emits `Code() { return 7; }`
/// (0 + 7). Mirrors the run-corpus `comptime_reflect_count_zero.bf`; proves the
/// generator observes the policy-gated metadata at comptime (a marked type emits a
/// different constant).
#[test]
fn comptime_reflect_count_zero_returns_7() {
    let src = r#"
        class Plain {
            public int32 mA;
            public int32 mB;
            [Comptime, EmitGenerator]
            public static void Generate() {
                int n = typeof(Plain).GetFieldCount();   // 0 (stripped; Type global present)
                String s = new String("public int32 Code() { return ");
                s.Append(n + 7);                         // (i64) 0 + 7 = 7 → Append(int)
                s.Append("; }");
                Compiler.EmitTypeBody(s);
                delete s;                                // exactly once
            }
        }
        class Program {
            public static int32 Main() {
                Plain p = new Plain();
                int32 r = p.Code();
                delete p;
                return r;
            }
        }
    "#;
    newbf_llvm::set_guard_mode(newbf_llvm::GuardMode::Stomp);
    let module = emit_module(src);
    let jit = OrcJit::from_ir(&module).expect("final module JIT-links clean");
    let addr = jit.lookup("Program.Main").expect("Program.Main resolves");
    let main: extern "C" fn() -> i32 = unsafe { std::mem::transmute(addr) };
    assert_eq!(
        main(),
        7,
        "an unmarked type reflects field count 0 at comptime → emitted Code() returns 0 + 7 = 7"
    );
    newbf_llvm::guard_reset();
}

// ── CR-T4: name-driven reflection-driven codegen (the name marquee) ───────────

/// The CR-T4 marquee source (the run-corpus `comptime_reflect_field_name.bf`): the
/// generator reads the FIRST FIELD'S NAME (`typeof(Tagged).GetField(0).GetName()` →
/// `char8*`) AT COMPILE TIME and EMITS a predicate whose behavior depends on that
/// name. Both the generator code AND the emitted runtime text BIND a `FieldInfo`
/// local before `.GetName()` (R5 — the value-struct method-chain trap: a chained
/// `GetField(0).GetName()` lowers the rvalue receiver to `undef`). The emitted text
/// contains a NESTED Beef string literal (`"mX"`) the runtime `Internal.StrEq`
/// compares the re-derived name against; `Append(char8*)` (CR-T2) splices the
/// reflected name between the quotes. The value 1 is computable only if the sandbox
/// saw the reflected name "mX" and the runtime-`String` `Compiler.EmitTypeBody(...)`
/// path (CR-T0) carried the computed text out and re-resolved it.
const REFLECT_FIELD_NAME_SRC: &str = r#"
    [Reflect(.Fields)]
    class Tagged {
        public int32 mX;

        [Comptime, EmitGenerator]
        public static void Generate() {
            String s = new String(
                "public bool FirstFieldIsMX() { FieldInfo f = typeof(Tagged).GetField(0); return Internal.StrEq(f.GetName(), \"");
            FieldInfo gf = typeof(Tagged).GetField(0);   // R5: bind the local
            s.Append(gf.GetName());                      // Append(char8*) — the reflected NAME ("mX")
            s.Append("\"); }");
            Compiler.EmitTypeBody(s);                    // runtime String, NOT a literal
            delete s;                                    // exactly once → no double-free
        }
    }
    class Program {
        public static int32 Main() {
            Tagged t = new Tagged();
            bool ok = t.FirstFieldIsMX();                // the EMITTED predicate: first field IS "mX"
            delete t;
            return ok ? 1 : 0;
        }
    }
"#;

/// **CR-T4 — the name-driven reflection-driven-codegen marquee (full frontend).**
/// The generator reflects the first field's NAME in the emission sandbox and emits
/// `FirstFieldIsMX()` whose runtime body binds a `FieldInfo` local, re-derives the
/// name, and StrEqs it against the literal "mX" the generator spliced in; `Main`
/// calls it → 1. Reflection field NAMES drive codegen: the emitted member's behavior
/// depends on a compile-time-reflected field name. Running through `run_emission` /
/// `OrcJit` exercises the generator under the same `GuardMode::Stomp` the run-corpus
/// harness uses — a double-free in the generator (R10) would fault here.
#[test]
fn comptime_reflect_field_name_returns_1() {
    newbf_llvm::set_guard_mode(newbf_llvm::GuardMode::Stomp);
    let module = emit_module(REFLECT_FIELD_NAME_SRC);
    let jit = OrcJit::from_ir(&module).expect("final module JIT-links clean");
    let addr = jit.lookup("Program.Main").expect("Program.Main resolves");
    let main: extern "C" fn() -> i32 = unsafe { std::mem::transmute(addr) };
    assert_eq!(
        main(),
        1,
        "the emitted FirstFieldIsMX() re-derives the compile-time-reflected field name → 1"
    );
    newbf_llvm::guard_reset();
}

/// **CR-T4 — the strip + JIT/AOT link-clean assertion for name-driven emission
/// (R10/R7).** After emission the final module must contain the generated
/// `Tagged.FirstFieldIsMX` but NEITHER the `[EmitGenerator]` generator NOR the
/// `__newbf_ct_emit` shim. The corlib reflection methods the generator pulled in
/// (`Type.GetField`, `FieldInfo.GetName`, `String.Append`) SURVIVE — they are
/// ordinary corlib code, not `module.comptime`. Asserts the IR shape AND that the
/// module both JIT-links and AOT-emits cleanly.
#[test]
fn comptime_reflect_field_name_strips_generator_and_shim_links_clean() {
    let module = emit_module(REFLECT_FIELD_NAME_SRC);

    // The generated member is present.
    assert!(
        module
            .funcs
            .iter()
            .any(|f| f.name == "Tagged.FirstFieldIsMX"),
        "generated member `Tagged.FirstFieldIsMX` must be present (have: {:?})",
        module.funcs.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    // The corlib reflection API the generator reflected through survives the strip
    // (it is ordinary corlib code, not module.comptime).
    assert!(
        module.funcs.iter().any(|f| f.name.contains("Type.GetField")),
        "corlib `Type.GetField` must survive the strip"
    );
    assert!(
        module
            .funcs
            .iter()
            .any(|f| f.name.contains("FieldInfo.GetName")),
        "corlib `FieldInfo.GetName` must survive the strip"
    );
    // The generator + shim are stripped.
    assert!(
        !module.funcs.iter().any(|f| f.name.contains("Generate")),
        "the [EmitGenerator] generator must be ABSENT from the final IR"
    );
    assert!(
        !module.funcs.iter().any(|f| f.name == "__newbf_ct_emit"),
        "the __newbf_ct_emit shim must be ABSENT from the final IR"
    );
    assert!(module.emit_jobs.is_empty(), "emit_jobs consumed");

    // JIT-links clean (a surviving `__newbf_ct_emit` extern would fail lookup).
    let jit = OrcJit::from_ir(&module).expect("final module JIT-links with no unresolved symbols");
    assert!(
        jit.lookup("Program.Main").is_some(),
        "Program.Main resolves in the JIT-linked module"
    );
    // AOT-emits clean.
    let obj = newbf_llvm::emit_object_to_memory(&module)
        .expect("final module AOT-emits an object with no codegen error");
    assert!(!obj.is_empty(), "AOT object is non-empty");
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
        name: "",
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
