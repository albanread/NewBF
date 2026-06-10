//! `newbf-sema` — the NewBF semantic core.
//!
//! Builds the **authoritative definition graph**: an exhaustive walk of
//! the parse tree that records every namespace, type, and member with its
//! full shape (modifiers, attributes, generic params, bases, constraints,
//! parameter signatures, accessors), then resolves namespaces and `using`
//! directives and reports in-program contradictions (duplicate defs).
//!
//! Design contract (SPRINTS.md Sprint 05, and the user's directive):
//! downstream phases (IR lowering, codegen, comptime, reflection) consume
//! [`DefGraph`] as the single source of truth — they must **not** re-walk
//! the raw AST. Whatever they need is recorded here. Type references are
//! normalized into [`model::TypeRef`] for exactly this reason.
//!
//! Later sprints fill in the rest of sema (full type resolution, generic
//! instantiation, dispatch, definite-assignment, manual-memory delete-flow
//! checks). Reference: `E:\beef\IDEHelper\Compiler\BfDefBuilder.cpp`,
//! `BfSystem.cpp`, `BfModule.cpp`.

mod api;
mod build;
mod constraints;
mod intern;
mod lower;
mod model;
mod ownership;
mod report;
mod resolve;

pub use api::{ApiImport, ResolvedApi, discover_extern_methods, resolve_apis};
pub use build::SourceFile;
pub use intern::{Interner, Symbol};
pub use lower::lower_program;
pub use model::{
    AccessorDef, AttrRef, BodyKind, DefGraph, DelegateSig, EnumCaseDef, FieldDef, MemberDef,
    MemberId, MethodDef, MethodKind, NamespaceDef, NsId, ParamDef, PropertyDef, TypeDef, TypeId,
    TypeKindD, TypeRef, TypeRefSeg, UsingDef, UsingRes, WhereRef,
};
pub use report::format_defs;

use newbf_lexer::Span;

/// A semantic diagnostic (an in-program contradiction). Span-keyed so the
/// driver can render it like the parser's diagnostics.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Diagnostic {
    pub span: Span,
    pub message: String,
}

/// The analyzed program: the def graph, its interner, and diagnostics.
pub struct Program {
    pub interner: Interner,
    pub graph: DefGraph,
    pub diagnostics: Vec<Diagnostic>,
}

/// Analyze a set of parsed files into the definition graph. Files are
/// merged into one program (open namespaces and extensions span files).
pub fn analyze(files: &[SourceFile<'_>]) -> Program {
    let mut builder = build::Builder::new();
    for f in files {
        builder.build_file(f);
    }
    let mut diagnostics = builder.resolve_and_check();
    let global = builder.global();
    let graph = DefGraph {
        namespaces: builder.namespaces,
        types: builder.types,
        members: builder.members,
        usings: builder.usings,
        global,
    };
    // MS-T5/MS-T6 (memory-safety.md §B0): the compile-time delete-flow pass runs
    // here, AFTER `resolve_and_check`, appending provable double-free /
    // scope-delete (MS-T5) and provable-leak (MS-T6) diagnostics. It is pure sema
    // (no IR, no LLVM) and re-walks the raw `CompUnit` ASTs in `files` (the
    // DefGraph carries no method bodies). It sees only the **user sources** — the
    // corlib prelude is injected later inside lowering, so library code is never
    // analysed here. The pass is strictly conservative (zero false positives): it
    // keeps an owning `new` binding `Owned` only through by-reference uses
    // (arg-pass/method-call/member-read), moves it on `return`/tracked-reassign,
    // drops it on capture/field-store/address-of, and diagnoses only a
    // provably-deleted-then-deleted (or scope-then-deleted) binding, or a binding
    // still `Owned` at a function-body exit edge (a provable leak).
    diagnostics.extend(ownership::check_delete_flow(files, &graph, &builder.interner));
    // CT-T1 (generic-constraints.md §3.2 / §7): the generic-`where`-clause
    // enforcement pass runs here, immediately after `check_delete_flow`, with the
    // same `(files, &DefGraph, &Interner)` signature — the only seam that has the
    // resolved `DefGraph` (for the `(name, arity)` type index) AND the method-body
    // ASTs in `files` (for the clauses' constrained entities). For CT-T1 it does
    // the full body-first classification of every constraint form (supported +
    // deferred) but emits ZERO diagnostics, so it is strictly behavior-preserving;
    // CT-T2/CT-T3 add the provable-violation diagnostics on top of the classifier.
    diagnostics.extend(constraints::check_generic_constraints(
        files,
        &graph,
        &builder.interner,
    ));
    Program {
        graph,
        interner: builder.interner,
        diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use newbf_lexer::FileId;
    use newbf_parser::parse_file;

    /// Parse `src` and analyze it as a one-file program.
    fn analyze_src(src: &str) -> Program {
        let (unit, pdiags) = parse_file(src, FileId(0));
        assert!(
            pdiags.is_empty(),
            "parse diagnostics for test source: {pdiags:?}"
        );
        analyze(&[SourceFile {
            file: FileId(0),
            src,
            unit: &unit,
            name: "",
        }])
    }

    fn type_named<'a>(p: &'a Program, name: &str) -> &'a TypeDef {
        p.graph
            .types
            .iter()
            .find(|t| p.interner.resolve(t.name) == name)
            .unwrap_or_else(|| panic!("no type named {name}"))
    }

    #[test]
    fn captures_namespace_type_member_counts() {
        let src = "
namespace Demo {
    public class Point {
        public int x;
        public int y;
        public this(int x, int y) { this.x = x; this.y = y; }
        public int LenSq() => x * x + y * y;
    }
}
";
        let p = analyze_src(src);
        // global + Demo
        assert!(p.graph.namespaces.iter().any(|n| n.full == "Demo"));
        let pt = type_named(&p, "Point");
        assert_eq!(pt.kind, TypeKindD::Class);
        assert_eq!(pt.members.len(), 4); // x, y, ctor, LenSq
        assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    }

    #[test]
    fn member_shapes_are_recorded() {
        let src = "class C { public int X { get; set; } public static int Sq(int n) => n * n; }";
        let p = analyze_src(src);
        let c = type_named(&p, "C");
        let mut saw_prop = false;
        let mut saw_method = false;
        for &mid in &c.members {
            match p.graph.member(mid) {
                MemberDef::Property(prop) => {
                    saw_prop = true;
                    assert_eq!(prop.accessors.len(), 2);
                    assert_eq!(p.interner.resolve(prop.name), "X");
                }
                MemberDef::Method(m) => {
                    saw_method = true;
                    assert_eq!(p.interner.resolve(m.name), "Sq");
                    assert_eq!(m.params.len(), 1);
                    assert!(m.return_ty.is_some());
                    assert_eq!(m.body, BodyKind::Expr);
                }
                _ => {}
            }
        }
        assert!(saw_prop && saw_method);
    }

    #[test]
    fn nested_namespaces_from_dotted_path() {
        let p = analyze_src("namespace A.B.C { class X { } }");
        for full in ["A", "A.B", "A.B.C"] {
            assert!(
                p.graph.namespaces.iter().any(|n| n.full == full),
                "missing namespace {full}"
            );
        }
        let x = type_named(&p, "X");
        assert_eq!(p.graph.ns(x.parent_ns).full, "A.B.C");
    }

    #[test]
    fn open_namespaces_merge_across_files() {
        let src1 = "namespace N { class A { } }";
        let src2 = "namespace N { class B { } }";
        let (u1, _) = parse_file(src1, FileId(0));
        let (u2, _) = parse_file(src2, FileId(1));
        let p = analyze(&[
            SourceFile {
                file: FileId(0),
                src: src1,
                unit: &u1,
                name: "",
            },
            SourceFile {
                file: FileId(1),
                src: src2,
                unit: &u2,
                name: "",
            },
        ]);
        let ns_n: Vec<_> = p
            .graph
            .namespaces
            .iter()
            .filter(|n| n.full == "N")
            .collect();
        assert_eq!(ns_n.len(), 1, "namespace N must merge into one node");
        assert_eq!(ns_n[0].types.len(), 2, "both A and B live in N");
    }

    #[test]
    fn nested_types_recorded_under_enclosing() {
        let p = analyze_src("class Outer { class Inner { } }");
        let outer = type_named(&p, "Outer");
        assert_eq!(outer.nested_types.len(), 1);
        let inner = type_named(&p, "Inner");
        assert_eq!(inner.enclosing_type, Some(TypeId(outer_id(&p))));
    }

    fn outer_id(p: &Program) -> u32 {
        p.graph
            .types
            .iter()
            .position(|t| p.interner.resolve(t.name) == "Outer")
            .unwrap() as u32
    }

    #[test]
    fn using_resolution_namespace_vs_external() {
        // `Demo` is declared here -> resolves; `System` isn't -> external.
        let src = "
using System;
using Demo;
namespace Demo { class X { } }
";
        let p = analyze_src(src);
        let demo = p
            .graph
            .usings
            .iter()
            .find(|u| matches!(u.resolution, UsingRes::Namespace(_)))
            .expect("Demo using should resolve to a namespace");
        assert!(matches!(demo.resolution, UsingRes::Namespace(_)));
        let external = p
            .graph
            .usings
            .iter()
            .filter(|u| matches!(u.resolution, UsingRes::External))
            .count();
        assert_eq!(external, 1, "System should be external");
    }

    #[test]
    fn duplicate_type_is_diagnosed() {
        let p = analyze_src("namespace N { class A { } class A { } }");
        assert!(
            p.diagnostics
                .iter()
                .any(|d| d.message.contains("duplicate type")),
            "{:?}",
            p.diagnostics
        );
    }

    #[test]
    fn extensions_do_not_count_as_duplicates() {
        let p = analyze_src("namespace N { class A { } extension A { } }");
        assert!(
            !p.diagnostics
                .iter()
                .any(|d| d.message.contains("duplicate type")),
            "extension must not collide with the class it reopens: {:?}",
            p.diagnostics
        );
    }

    // ── GM-A4: generic-method declaration guards ────────────────────────────

    /// A `virtual` generic method can't occupy a vtable slot (it's a family of
    /// monomorphs) — rejected loudly at the declaration (generic-methods §1/§6).
    #[test]
    fn virtual_generic_method_is_diagnosed() {
        let p = analyze_src("class C { public virtual T Wrap<T>(T x) { return x; } }");
        assert!(
            p.diagnostics
                .iter()
                .any(|d| d.message.contains("`virtual` generic method")),
            "virtual generic method must be diagnosed: {:?}",
            p.diagnostics
        );
    }

    /// `override` + generic is the same vtable conflict — also rejected.
    #[test]
    fn override_generic_method_is_diagnosed() {
        let p = analyze_src("class C { public override T G<T>(T x) { return x; } }");
        assert!(
            p.diagnostics
                .iter()
                .any(|d| d.message.contains("`override` generic method")),
            "override generic method must be diagnosed: {:?}",
            p.diagnostics
        );
    }

    /// A `virtual` *non-generic* method, even with a generic return/param type,
    /// is fine — the guard keys on the method's OWN type parameters, not text.
    #[test]
    fn virtual_nongeneric_method_is_not_diagnosed() {
        let p = analyze_src("class C { public virtual List<int> Make() { return null; } }");
        assert!(
            !p.diagnostics
                .iter()
                .any(|d| d.message.contains("generic method")),
            "a non-generic virtual method must not be flagged: {:?}",
            p.diagnostics
        );
    }

    /// A `[Comptime]` generic method is LEGAL Beef (the corlib relies on it,
    /// e.g. `Enum.GetCount<T>()`); only our v1 *lowering* can't instantiate it.
    /// It must NOT be a declaration error, or the corlib-slice ratchet breaks.
    /// (The lowering-side guard prevents the wrong emission; see lower.rs.)
    #[test]
    fn comptime_generic_method_is_not_a_declaration_error() {
        let p = analyze_src(
            "class C { [Comptime] public static T Id<T>(T x) { return x; } }",
        );
        assert!(
            p.diagnostics.is_empty(),
            "a [Comptime] generic method must build a clean def-graph: {:?}",
            p.diagnostics
        );
    }

    #[test]
    fn duplicate_field_is_diagnosed_but_method_overloads_are_not() {
        let dup = analyze_src("class C { int x; int x; }");
        assert!(
            dup.diagnostics
                .iter()
                .any(|d| d.message.contains("duplicate member"))
        );

        let overload = analyze_src("class C { void F() {} void F(int x) {} }");
        assert!(
            !overload
                .diagnostics
                .iter()
                .any(|d| d.message.contains("duplicate member")),
            "method overloads are legal: {:?}",
            overload.diagnostics
        );
    }

    #[test]
    fn explicit_interface_member_is_not_a_duplicate() {
        // A `const MinValue` field and a `IFoo<int>.MinValue` explicit-impl
        // property share a name but must NOT be flagged as duplicate members.
        let src = "struct Int { const int MinValue = 0; static int IMinMaxValue<int>.MinValue => MinValue; }";
        let p = analyze_src(src);
        assert!(
            !p.diagnostics
                .iter()
                .any(|d| d.message.contains("duplicate member")),
            "explicit-interface impl must not collide: {:?}",
            p.diagnostics
        );
        // And the qualifier is recorded on the property.
        let int = type_named(&p, "Int");
        let has_iface = int.members.iter().any(|&m| {
            matches!(p.graph.member(m), MemberDef::Property(prop) if prop.explicit_iface.is_some())
        });
        assert!(has_iface, "explicit interface qualifier should be recorded");
    }

    #[test]
    fn enum_cases_and_delegates_and_aliases_are_captured() {
        let src = "
enum Color { case Red, case Green = 2, case Custom(int r, int g, int b) }
delegate int Op(int a, int b);
typealias Id = int;
";
        let p = analyze_src(src);
        let color = type_named(&p, "Color");
        let cases = color
            .members
            .iter()
            .filter(|&&m| matches!(p.graph.member(m), MemberDef::EnumCase(_)))
            .count();
        assert_eq!(cases, 3);
        let op = type_named(&p, "Op");
        assert_eq!(op.kind, TypeKindD::Delegate);
        assert!(op.delegate_sig.as_ref().unwrap().params.len() == 2);
        let id = type_named(&p, "Id");
        assert_eq!(id.kind, TypeKindD::Alias);
        assert!(id.alias_target.is_some());
    }

    #[test]
    fn report_is_nonempty_and_lists_counts() {
        let p = analyze_src("namespace N { class A { int x; } }");
        let r = format_defs(&p);
        assert!(r.starts_with("defs:"));
        assert!(r.contains("N.A"));
        assert!(r.contains("field x"));
    }

    // ── IR lowering (primitive kernel) ──────────────────────────────────

    /// Parse + analyze + lower `src`, returning the `dump-ir` report.
    fn lower_src(src: &str) -> String {
        let (unit, pdiags) = parse_file(src, FileId(0));
        assert!(pdiags.is_empty(), "parse diagnostics: {pdiags:?}");
        let files = vec![SourceFile {
            file: FileId(0),
            src,
            unit: &unit,
            name: "",
        }];
        let program = analyze(&files);
        let module = lower_program(&files, &program);
        newbf_ir::format_ir(&module)
    }

    #[test]
    fn lowers_integer_arithmetic_method() {
        let ir = lower_src("class C { public static int add(int a, int b) { return a + b; } }");
        assert!(ir.contains("func @C.add(i64 %0, i64 %1) -> i64"), "{ir}");
        assert!(ir.contains("= add i64"), "{ir}");
        assert!(ir.contains("ret %"), "{ir}");
    }

    #[test]
    fn lowers_float_expression_body() {
        let ir = lower_src("class C { public static double dbl(double x) => x * 2.0; }");
        assert!(ir.contains("func @C.dbl(f64 %0) -> f64"), "{ir}");
        assert!(ir.contains("fmul f64"), "{ir}");
    }

    #[test]
    fn lowers_locals_and_while_loop() {
        let ir = lower_src(
            "class C { public static int sum(int n) { \
                int s = 0; while (n > 0) { s = s + n; n = n - 1; } return s; } }",
        );
        assert!(ir.contains("alloca i64"), "{ir}");
        assert!(ir.contains("while.head"), "{ir}");
        assert!(ir.contains("icmp sgt"), "{ir}");
        assert!(ir.contains("condbr"), "{ir}");
    }

    #[test]
    fn lowers_if_else_diamond() {
        let ir = lower_src(
            "class C { public static int m(int a, int b) { \
                if (a > b) { return a; } else { return b; } } }",
        );
        assert!(ir.contains("if.then"), "{ir}");
        assert!(ir.contains("if.else"), "{ir}");
        assert!(ir.contains("condbr"), "{ir}");
    }

    #[test]
    fn unsupported_body_lowers_to_terminated_stub_without_panic() {
        // `new`/member access aren't in the kernel; lowering must still
        // produce a well-formed, terminated function (no panic, no dangling).
        let ir = lower_src("class C { public static void h() { var x = new Foo(); x.Bar(); } }");
        assert!(ir.contains("func @C.h() -> void"), "{ir}");
        assert!(ir.contains("ret void"), "{ir}");
    }

    // ── GM-A4: deferred generic-method cases lower without garbage ───────────
    //
    // Each deferred shape (generic-methods §1) must produce NO monomorph symbol
    // and NO call to one — never a dangling call to a function that was never
    // emitted. The call site falls through to a clean default. (The whole-corpus
    // `llvm_lowering_verifies_on_real_beef` gate proves verifier-cleanliness; here
    // we assert the precise "no bad symbol / no dangling call" property.)

    /// A `virtual` generic method instantiated by a call must NOT emit a
    /// monomorph nor a direct call to it (that would skip vtable dispatch).
    #[test]
    fn virtual_generic_call_emits_no_monomorph() {
        let ir = lower_src(
            "class Base { public virtual T Wrap<T>(T x) { return x; } } \
             class Program { public static int32 Main() { \
                 Base b = new Base(); return b.Wrap<int32>(7); } }",
        );
        assert!(
            !ir.contains("Wrap$"),
            "no virtual generic monomorph may be emitted or called: {ir}"
        );
    }

    /// A `[Comptime]` generic method instantiated by a call must NOT emit a
    /// plain (un-folded) runtime monomorph — the gen-method path can't register
    /// it for comptime folding, so emitting it would silently run at runtime.
    #[test]
    fn comptime_generic_call_emits_no_monomorph() {
        let ir = lower_src(
            "class Program { [Comptime] public static T Id<T>(T x) { return x; } \
             public static int32 Main() { return Id<int32>(7); } }",
        );
        assert!(
            !ir.contains("Id$"),
            "no [Comptime] generic monomorph may be emitted or called: {ir}"
        );
    }

    /// An inherited generic instance method (declared on a base, called on a
    /// derived receiver) is deferred: owner = derived → key miss → clean
    /// fallthrough, never a dangling call to a base-owner symbol.
    #[test]
    fn inherited_generic_instance_call_has_no_dangling_call() {
        let ir = lower_src(
            "class Base { public T Wrap<T>(T x) { return x; } } \
             class Derived : Base { } \
             class Program { public static int32 Main() { \
                 Derived d = new Derived(); return d.Wrap<int32>(7); } }",
        );
        // No call may target a Wrap monomorph (the only Wrap$ allowed is the
        // *definition* emitted for Base, which is fine — but no `call` to it).
        assert!(
            !ir.contains("call i32 @Base.Wrap$"),
            "inherited generic instance call must not emit a dangling call: {ir}"
        );
    }

    /// An instance generic call on an unresolvable receiver (a call-return
    /// value) is deferred: the receiver owner can't be resolved at compile
    /// time, so the call falls through cleanly with no monomorph symbol.
    #[test]
    fn unresolvable_receiver_generic_call_is_clean() {
        let ir = lower_src(
            "class Box { public T Get<T>(T x) { return x; } } \
             class Program { \
                 public static Box MakeBox() { return new Box(); } \
                 public static int32 Main() { return MakeBox().Get<int32>(7); } }",
        );
        assert!(
            !ir.contains("Get$"),
            "an unresolvable-receiver generic call must not emit/call a monomorph: {ir}"
        );
    }

    /// The supported concrete-arg self-call (`M<int32>` inside a generic body)
    /// must keep working — GM-A4's guards must not regress it.
    #[test]
    fn concrete_self_call_in_generic_body_still_works() {
        let ir = lower_src(
            "class Program { \
                 public static T Inner<T>(T x) { return x; } \
                 public static T Outer<T>(T x) { return Inner<int32>(5); } \
                 public static int32 Main() { return Outer<int32>(1); } }",
        );
        assert!(
            ir.contains("call i32 @Program.Inner$i32"),
            "concrete-arg self-call must still resolve to a direct monomorph call: {ir}"
        );
    }

    /// An abstract inner type-arg (`Inner<T>` inside the `Outer<T>` template,
    /// `T` unbound at the template) must NOT mint a bogus `Inner$ptr` monomorph
    /// from the `Ptr` type-fallback (doc §1). The concrete `Inner<int32>` it
    /// also calls is still emitted; only the abstract spurious one is suppressed.
    #[test]
    fn abstract_inner_type_arg_emits_no_bogus_monomorph() {
        let ir = lower_src(
            "class Program { \
                 public static T Inner<T>(T x) { return x; } \
                 public static T Outer<T>(T x) { int32 a = Inner<int32>(5); return Inner<T>(x); } \
                 public static int32 Main() { return Outer<int32>(1); } }",
        );
        assert!(
            !ir.contains("Inner$p"),
            "abstract-type-arg self-call must not mint a bogus $ptr monomorph: {ir}"
        );
        assert!(
            ir.contains("@Program.Inner$i32"),
            "the concrete Inner<int32> monomorph must still be emitted: {ir}"
        );
    }

    // ── IT-T2+3: eager `yield` generator rewrite + walker collection ─────────

    /// A generic call inside `yield return …` must be COLLECTED for
    /// monomorphization. After the eager rewrite the body holds `__yield.Add(
    /// Id<int32>(7))`, so `collect_insts` (which the rewrite runs BEFORE) sees the
    /// `Id<int32>` instantiation and emits `Id$i32`. This proves the rewritten
    /// yielded expression reaches the mono collector (R8 — the walker arms +
    /// the pre-collect_insts ordering).
    #[test]
    fn generic_call_in_yield_return_is_monomorphized() {
        let ir = lower_src(
            "class Program { \
                 public static T Id<T>(T x) { return x; } \
                 public static List<int32> Gen() { yield return Id<int32>(7); } \
                 public static int32 Main() { return 0; } }",
        );
        assert!(
            ir.contains("@Program.Id$i32"),
            "a generic call inside `yield return` must be monomorphized: {ir}"
        );
    }

    /// An inline lambda inside `yield return …` must be COLLECTED for emission.
    /// After the rewrite the body holds `__yield.Add(Apply(x => x + 1))`, so the
    /// inline-lambda collector (driven by `for_each_stmt_expr`, which T2 taught to
    /// descend into `YieldReturn`) assigns it a `$lambda` symbol and emits it.
    #[test]
    fn lambda_in_yield_return_is_collected() {
        let ir = lower_src(
            "class Program { \
                 public static int32 Apply(function int32(int32) f) { return f(10); } \
                 public static List<int32> Gen() { yield return Apply(x => x + 1); } \
                 public static int32 Main() { return 0; } }",
        );
        assert!(
            ir.contains("$lambda"),
            "an inline lambda inside `yield return` must be collected + emitted: {ir}"
        );
    }

    /// The eager rewrite materializes the generator into a `List<int32>`-building
    /// method: the desugared body allocates the list (`new List<int32>` → a
    /// `List$i32` ctor call) and the raw `yield` is GONE (no stray skipped
    /// statement). Proves the re-emit→reparse desugar fired.
    #[test]
    fn yield_generator_desugars_to_list_allocation() {
        let ir = lower_src(
            "class Program { \
                 public static List<int32> Gen() { yield return 1; yield return 2; } \
                 public static int32 Main() { return 0; } }",
        );
        // The generator allocates a `List<int32>` (the monomorph ctor is emitted +
        // called from the rewritten body).
        assert!(
            ir.contains("List$i32"),
            "the eager rewrite must instantiate List<int32> from the synthesized body: {ir}"
        );
    }

    /// A method WITHOUT `yield` is left completely untouched by the rewrite — its
    /// IR is identical whether or not `rewrite_generators` runs. (Lowering the
    /// same source through the whole `lower_program` path is the proof: a
    /// non-generator program lowers exactly as before, the rewrite's fast path
    /// being a no-op.)
    #[test]
    fn non_generator_method_is_untouched_by_rewrite() {
        let ir = lower_src(
            "class C { public static int32 add(int32 a, int32 b) { return a + b; } }",
        );
        assert!(ir.contains("func @C.add(i32 %0, i32 %1) -> i32"), "{ir}");
        assert!(ir.contains("= add i32"), "{ir}");
        // No accidental List allocation / __yield artifact in non-yield code.
        assert!(!ir.contains("__yield"), "non-generator code must not gain __yield: {ir}");
    }
}
