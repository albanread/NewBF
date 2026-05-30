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

mod build;
mod intern;
mod model;
mod report;
mod resolve;

pub use build::SourceFile;
pub use intern::{Interner, Symbol};
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
    let diagnostics = builder.resolve_and_check();
    let global = builder.global();
    Program {
        graph: DefGraph {
            namespaces: builder.namespaces,
            types: builder.types,
            members: builder.members,
            usings: builder.usings,
            global,
        },
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
            },
            SourceFile {
                file: FileId(1),
                src: src2,
                unit: &u2,
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
}
