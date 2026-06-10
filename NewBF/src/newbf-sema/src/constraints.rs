//! CT-T1 (generic-constraints.md §3, §7) — the generic-`where`-clause
//! enforcement pass **skeleton**: a pure-sema classifier that *recognizes*
//! every constraint form (supported **and** deferred) but, in this task, emits
//! **no diagnostic at all**. It is the ratchet-safety foundation (R1): it lands
//! the pass, its `(name, arity)` type index, the full body-first classifier, and
//! the four per-file `constraint_diags == 0` pins **before** any
//! diagnostic-emitting task (CT-T2/CT-T3) builds on top of it.
//!
//! ## Why this is behavior-preserving
//!
//! [`check_generic_constraints`] returns an **empty** `Vec<Diagnostic>` for now
//! (it does the full classification work but pushes nothing). So wiring it into
//! [`crate::analyze`] right after `check_delete_flow` changes nothing observable:
//! every corpus file still analyzes with zero diagnostics, every module still
//! lowers verifier-clean. CT-T2/CT-T3 add the actual diagnostics on top of the
//! classifier this task lands.
//!
//! ## Structure (mirrors [`crate::ownership::check_delete_flow`])
//!
//! Like delete-flow, this is a pure-sema pass that runs in `analyze` **after**
//! `resolve_and_check` (the `DefGraph` is fully built and the method-body ASTs in
//! `files` are available). It re-walks the raw `CompUnit` ASTs in `files` — the
//! `DefGraph` carries no bodies, and the `where`-clauses we classify live on the
//! AST `TypeDecl`/`Member::Method`/`Member::Constructor` nodes. The signature is
//! identical to `check_delete_flow`: `(files, &DefGraph, &Interner)`.
//!
//! ## The `(name, arity)` type index (generic-constraints.md §2.2)
//!
//! One read-only index `(simple name, arity) -> TypeId` is built from
//! `graph.types`, keyed by **`(name, arity)`** — NOT by bare `Symbol` — so a
//! generic `IFaceD<T>` (arity 1) and a non-generic `IFaceD` (arity 0) in the same
//! file do not collide (`Interfaces.bf:204` vs `:280`). This mirrors
//! `index_generic_decls` (`lower.rs`) and `check_duplicate_types`, both of which
//! key by `(name, arity)`. First-wins within an arity (conservative). The index
//! is transient — built once per `analyze`, discarded when the pass returns.
//!
//! ## Body-first classification (generic-constraints.md §3.2 — ratchet-critical)
//!
//! [`classify_constraint`] reads the **constraint atom's structure FIRST**,
//! before ever looking at the constrained `where T` name. This is mandatory: real
//! corpus clauses constrain a **non-parameter** entity, e.g.
//! `where float : operator T * T` (`Constraints.bf:55`), so a name-first check
//! would mis-fire on every operator clause. Reading the body first means an
//! operator clause is classified [`ConstraintKind::OperatorBound`] (deferred) by
//! its `Type::Var` shape, never mistaken for a type bound.
//!
//! ## Termination (generic-constraints.md §3.2, R12)
//!
//! CT-T1 does **no** transitive base/interface walk — pure single-atom shape
//! classification. The transitive `implements`/base walk (with its mandatory
//! `HashSet<TypeId>` cycle guard for self-referential bounds like
//! `Singleton<T> where T : Singleton<T>`, `Generics.bf:79`) lands in CT-T3, which
//! actually validates instantiations. This module hands CT-T3 a complete,
//! per-atom classification so it never has to re-derive the clause shapes.

use std::collections::HashMap;

use newbf_lexer::Span;
use newbf_parser::{GenericParam, Item, Member, Type, TypeDecl, WhereClause};

use crate::Diagnostic;
use crate::build::SourceFile;
use crate::intern::Interner;
use crate::model::{DefGraph, TypeId, TypeKindD};

/// The classified form of **one** constraint atom (`generic-constraints.md`
/// §3.2 / §5). The classifier recognizes every shape the four constraint-dense
/// corpus files contain (supported **and** deferred); CT-T2/CT-T3 then attach
/// diagnostics to the *supported* labels and skip the *deferred* ones.
///
/// `#[allow(dead_code)]`: several variants and their carried data are not
/// consumed until CT-T2/CT-T3 (which emit the diagnostics). They are produced by
/// the classifier now so the diagnostic tasks build on a complete classification
/// rather than re-deriving the clause shapes.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ConstraintKind {
    // ── supported forms (CT-T2/CT-T3 will diagnose provable violations) ──────
    /// `where T : class` — the keyword path `"class"`. Reference-kind bound.
    Class,
    /// `where T : struct` — the keyword path `"struct"`. Value-kind bound.
    Struct,
    /// `where T : new` — the keyword path `"new"`. Constructible bound.
    New,
    /// `where T : SomeIFace` — a single-segment, no-args path whose name binds to
    /// an in-program **non-generic interface** (arity 0). Interface bound. Carries
    /// the bound's simple name (the key CT-T3 resolves against the type index).
    Interface(String),
    /// `where T : SomeClass` — a single-segment, no-args path binding to an
    /// in-program **class** (arity 0). Base-class bound. Carries the simple name.
    BaseClass(String),

    // ── deferred forms (recognized, skipped silently — NO diagnostic ever) ───
    /// `where T : delete` — the keyword path `"delete"`. Disposability bound.
    Delete,
    /// `where T : var` — the keyword path `"var"`.
    Var,
    /// `where T : concrete` — the keyword path `"concrete"`.
    Concrete,
    /// `where T : interface` — the keyword path `"interface"` (kind keyword, NOT
    /// a named interface bound; distinct from [`ConstraintKind::Interface`]).
    InterfaceKind,
    /// `where T : enum` — the keyword path `"enum"`.
    EnumKind,
    /// `where T : T2` — the constrained name's bound is **another generic
    /// parameter** of the enclosing decl (`Generics.bf:268`,
    /// `Generics2.bf:197`). Type-parameter-to-type-parameter. Carries the bound
    /// parameter's name.
    TypeParam(String),
    /// `where T : IEnumerator<TElement>` / `where T : IFaceD<int16>` — a path
    /// whose last segment carries generic args. Generic-interface / generic-base
    /// bound (`Constraints.bf:33`, `Interfaces.bf:265`). Generic interfaces are
    /// not monomorphized, so this is deferred. Carries the last segment's name.
    GenericBound(String),
    /// `where T : float` / (post-`const`-strip) `where C : const int` — a
    /// single-segment path whose name is a **primitive** type. Primitive-name
    /// bound (`Constraints.bf:16`). Carries the primitive name.
    PrimitiveName(String),
    /// `where T : SomeName` where `SomeName` resolves to **nothing** in this
    /// (one-file) program — e.g. a corlib interface (`IDisposable`, `IHashable`)
    /// under the per-file ratchet. Unresolvable ⇒ skip. Carries the name.
    Unresolved(String),
    /// `where … : operator …` — parsed as [`Type::Var`] (`parser.rs`). Operator
    /// constraint (`Constraints.bf:55`). Semantically deep ⇒ deferred.
    OperatorBound,
    /// `where Del : delegate bool(T)` — a `delegate`/`function` type
    /// (`Generics2.bf:71`, `:210`). Delegate-shape bound ⇒ deferred.
    DelegateBound,
    /// `where T : struct*` / `where T : int*` — a `Type::Pointer` wrapping a
    /// keyword/primitive path (`Generics.bf:194/202`). Pointer-suffixed ⇒
    /// deferred.
    Pointer,
    /// `where TS : StringView[C]` — a sized/array type (`Constraints.bf:94`).
    /// Array/sized bound ⇒ deferred.
    ArrayOrSized,
    /// `where comptype(typeof(T2)) : List<T>` constraint atoms that are
    /// `comptype`/`decltype`/… computed types, tuples, or any other shape not
    /// covered above. Catch-all ⇒ deferred.
    Other,
}

#[allow(dead_code)]
impl ConstraintKind {
    /// Whether CT-T2/CT-T3 *may* attach a diagnostic to this kind. Deferred kinds
    /// are recognized-and-skipped so the ratchet holds; supported kinds are the
    /// only ones a later task validates. (Unused until CT-T2/CT-T3.)
    pub(crate) fn is_supported(&self) -> bool {
        matches!(
            self,
            ConstraintKind::Class
                | ConstraintKind::Struct
                | ConstraintKind::New
                | ConstraintKind::Interface(_)
                | ConstraintKind::BaseClass(_)
        )
    }
}

/// Entry point: walk every `where`-clause on every type/method/ctor in the user
/// `files`, classify each constraint atom, and return the constraint
/// diagnostics. For CT-T1 this is a **no-op** — the classifier runs in full but
/// pushes **zero** diagnostics (behavior-preserving). CT-T2/CT-T3 emit on top.
pub(crate) fn check_generic_constraints(
    files: &[SourceFile<'_>],
    graph: &DefGraph,
    interner: &Interner,
) -> Vec<Diagnostic> {
    let index = TypeIndex::build(graph, interner);
    let mut diags = Vec::new();
    let mut cx = Cx {
        index: &index,
        diags: &mut diags,
    };
    for f in files {
        cx.walk_items(&f.unit.items, f.src);
    }
    diags
}

/// The `(simple name, arity) -> TypeId` index plus the coarse `TypeKindD` of
/// each entry (generic-constraints.md §2.2). First-wins within an `(name, arity)`
/// cell, so the `kind` and the `TypeId` always describe the same chosen entry.
///
/// The `TypeId` is what CT-T3 will use to start its transitive base walk; the
/// `kind` lets the classifier split a resolved bare bound into Interface vs
/// BaseClass without re-borrowing the graph.
struct TypeIndex {
    by_name_arity: HashMap<(String, u32), TypeId>,
    kind_by_name_arity: HashMap<(String, u32), TypeKindD>,
}

impl TypeIndex {
    fn build(graph: &DefGraph, interner: &Interner) -> Self {
        let mut by_name_arity = HashMap::new();
        let mut kind_by_name_arity = HashMap::new();
        for (i, t) in graph.types.iter().enumerate() {
            let name = interner.resolve(t.name).to_string();
            // First-wins within an arity (conservative): an ambiguous simple name
            // resolves to one TypeId; a violation only fires on a PROVABLE
            // mismatch (CT-T3), else Skip. Both maps insert under the same guard
            // so they stay consistent (same chosen entry).
            let key = (name, t.arity);
            if !by_name_arity.contains_key(&key) {
                by_name_arity.insert(key.clone(), TypeId(i as u32));
                kind_by_name_arity.insert(key, t.kind);
            }
        }
        Self {
            by_name_arity,
            kind_by_name_arity,
        }
    }

    /// Look up a bare simple name as an arity-0 entry (a `where T : IFace` with
    /// no `<…>` binds the non-generic entry — mirrors `index_generic_decls` /
    /// `check_duplicate_types`). Returns `None` when unresolvable in this program.
    fn lookup_arity0(&self, name: &str) -> Option<TypeId> {
        self.by_name_arity.get(&(name.to_string(), 0)).copied()
    }
}

/// Per-parameter accumulator for CT-T2's clause-internal kind-contradiction
/// check. Tracks, within ONE declaration, whether a given constrained parameter
/// name has been seen with the bare `class` and/or `struct` keyword, plus the
/// span to report the contradiction at (first kind-keyword clause wins).
#[derive(Default)]
struct ContradictionState {
    has_class: bool,
    has_struct: bool,
    span: Option<Span>,
}

struct Cx<'a> {
    index: &'a TypeIndex,
    /// CT-T2 emits the clause-internal `class`∧`struct` contradiction through
    /// this channel (mirroring `ownership::Cx`).
    diags: &'a mut Vec<Diagnostic>,
}

impl Cx<'_> {
    fn walk_items(&mut self, items: &[Item], src: &str) {
        for it in items {
            match it {
                Item::Namespace { body: Some(b), .. } => self.walk_items(b, src),
                Item::Type(td) => self.walk_type(td, src),
                _ => {}
            }
        }
    }

    fn walk_type(&mut self, td: &TypeDecl, src: &str) {
        // Type-decl-level `where` clauses are classified (so CT-T2's
        // clause-internal contradiction can later see them), but no
        // instantiation-level enforcement fires for them in v1 (§5).
        self.classify_clauses(&td.constraints, &td.generic_params, src);
        for m in &td.members {
            match m {
                Member::Method {
                    constraints,
                    generic_params,
                    ..
                } => self.classify_clauses(constraints, generic_params, src),
                Member::Constructor {
                    constraints,
                    generic_params,
                    ..
                } => self.classify_clauses(constraints, generic_params, src),
                Member::Nested(n) => self.walk_type(n, src),
                _ => {}
            }
        }
    }

    /// Classify every atom of every clause in `clauses`, then run CT-T2's
    /// declaration-level **clause-internal kind contradiction** check.
    /// `generic_params` is the set of generic parameters in scope at the
    /// constrained entity (used only to recognize the `where T : T2`
    /// type-parameter-to-type-parameter form).
    ///
    /// CT-T2 (generic-constraints.md §3.2) is a purely-local, decl-internal
    /// check: group the clauses of THIS one declaration by the constrained
    /// parameter NAME (`WhereClause.name`), collect each parameter's classified
    /// kinds, and emit ONE diagnostic if a parameter carries BOTH
    /// [`ConstraintKind::Class`] AND [`ConstraintKind::Struct`] (an unsatisfiable
    /// contradiction — no type is both a reference class and a value struct).
    ///
    /// **Conservative — bare keyword paths only.** Only the bare `class`/`struct`
    /// keyword kinds participate; every other kind (including the `interface`
    /// keyword kind, named bounds, operator/const/array/generic/primitive forms)
    /// is ignored for this check. There is deliberately **no** "the constrained
    /// name isn't a generic parameter" check: in Beef the constrained entity may
    /// legitimately be a non-parameter type (e.g. `where float : operator T * T`,
    /// where `WhereClause.name` is `float`), so such a check would fire on every
    /// operator clause and break the ratchet (§3.2).
    fn classify_clauses(
        &mut self,
        clauses: &[WhereClause],
        generic_params: &[GenericParam],
        src: &str,
    ) {
        // Group this decl's clauses by the constrained parameter NAME and record,
        // per name, whether `class` and/or `struct` was seen plus the span to
        // report against. First-seen span wins (deterministic, points at the
        // earliest contradicting clause). Insertion order preserved so the
        // diagnostic order is stable across runs.
        let mut order: Vec<&str> = Vec::new();
        let mut seen: HashMap<&str, ContradictionState> = HashMap::new();
        for clause in clauses {
            let pname = clause.name.text(src);
            for atom in &clause.constraints {
                // Body-first: read the atom's shape, then (only for a bare path)
                // disambiguate via the generic-param set + the type index.
                let kind = classify_constraint(atom, generic_params, self.index, src);
                // CT-T2: only the bare `class`/`struct` keyword kinds matter here.
                let (is_class, is_struct) = match kind {
                    ConstraintKind::Class => (true, false),
                    ConstraintKind::Struct => (false, true),
                    _ => continue,
                };
                let st = seen.entry(pname).or_insert_with(|| {
                    order.push(pname);
                    ContradictionState::default()
                });
                st.has_class |= is_class;
                st.has_struct |= is_struct;
                // Report at the parameter-name span of the clause that first
                // makes this name carry a kind keyword — a stable, on-the-decl
                // location for the contradiction.
                if st.span.is_none() {
                    st.span = Some(clause.name);
                }
            }
        }
        for pname in order {
            let st = &seen[pname];
            if st.has_class && st.has_struct {
                self.diags.push(Diagnostic {
                    span: st.span.expect("a kind-keyword clause set the span"),
                    message: format!(
                        "generic parameter `{pname}` cannot be constrained as both \
                         `class` and `struct` (unsatisfiable constraint contradiction)"
                    ),
                });
            }
        }
    }
}

/// Classify one constraint atom by its **body shape first** (§3.2). The
/// constrained `where T` name is never consulted here — only the atom's
/// structure, plus `generic_params` (to recognize `T : T2`) and the type index
/// (to split a bare path into Interface / BaseClass / PrimitiveName /
/// Unresolved). Pure and total: every shape lands on exactly one
/// [`ConstraintKind`].
fn classify_constraint(
    atom: &Type,
    generic_params: &[GenericParam],
    index: &TypeIndex,
    src: &str,
) -> ConstraintKind {
    match atom {
        // `operator …` is parsed as `Type::Var` (parser.rs constraint_atom).
        Type::Var(_) => ConstraintKind::OperatorBound,
        // `where T : struct*` / `int*` — pointer-suffixed keyword/primitive.
        Type::Pointer { .. } => ConstraintKind::Pointer,
        // `where TS : StringView[C]` / array-shaped bounds.
        Type::Array { .. } | Type::Sized { .. } => ConstraintKind::ArrayOrSized,
        // `where Del : delegate bool(T)` / `function …`.
        Type::Function { .. } => ConstraintKind::DelegateBound,
        // Tuple / computed (`comptype(…)`) / nullable / anonymous / const-arg /
        // recovery — all deferred catch-alls.
        Type::Tuple { .. }
        | Type::Computed { .. }
        | Type::Nullable { .. }
        | Type::Anonymous(_)
        | Type::ConstArg { .. }
        | Type::Error(_) => ConstraintKind::Other,
        Type::Path { segments, .. } => {
            // A multi-segment path (`A.B`) or one whose LAST segment carries
            // generic args is a generic / qualified bound. Generic interfaces
            // are not monomorphized → deferred. (Note: `const Type` has had its
            // `const` stripped by the parser, so `const int` arrives as the bare
            // path `int` and is classified as PrimitiveName below — both are
            // deferred, §5.)
            let Some(last) = segments.last() else {
                return ConstraintKind::Other;
            };
            if segments.len() > 1 || !last.args.is_empty() {
                return ConstraintKind::GenericBound(last.name.text(src).to_string());
            }
            // Single-segment, no-args path. Read the segment text once.
            let name = last.name.text(src);
            // Keyword constraints arrive as a single-ident path whose segment
            // text IS the keyword (parser.rs synthesizes them this way).
            match name {
                "class" => return ConstraintKind::Class,
                "struct" => return ConstraintKind::Struct,
                "new" => return ConstraintKind::New,
                "delete" => return ConstraintKind::Delete,
                "var" => return ConstraintKind::Var,
                "concrete" => return ConstraintKind::Concrete,
                "interface" => return ConstraintKind::InterfaceKind,
                "enum" => return ConstraintKind::EnumKind,
                _ => {}
            }
            // `where T : T2` — the bound is another generic parameter in scope.
            if generic_params.iter().any(|gp| gp.name.text(src) == name) {
                return ConstraintKind::TypeParam(name.to_string());
            }
            // A primitive-name bound (`where T : float`) has no in-program
            // TypeDef → deferred.
            if is_primitive_name(name) {
                return ConstraintKind::PrimitiveName(name.to_string());
            }
            // A named bound: resolve it as an arity-0 entry. Class → BaseClass,
            // interface → Interface; anything else (struct/enum/delegate/alias)
            // or unresolvable → deferred/skip.
            match index.lookup_arity0(name) {
                Some(_) => classify_named_bound(name, index),
                None => ConstraintKind::Unresolved(name.to_string()),
            }
        }
    }
}

/// A bare named bound that resolved to an in-program arity-0 type: split by its
/// declared `TypeKindD`. Only `Interface`/`Class` become *supported* bounds
/// (which CT-T3 validates); any other declared kind (struct/enum/delegate/alias)
/// is conservatively `Other` (deferred). The kind is read from the index's
/// `kind_by_name_arity` map (recorded at build time), so the classifier never
/// re-borrows the graph.
fn classify_named_bound(name: &str, index: &TypeIndex) -> ConstraintKind {
    match index.kind_by_name_arity.get(&(name.to_string(), 0)) {
        Some(TypeKindD::Interface) => ConstraintKind::Interface(name.to_string()),
        Some(TypeKindD::Class) => ConstraintKind::BaseClass(name.to_string()),
        // struct/enum/delegate/alias bound — not a supported v1 form.
        _ => ConstraintKind::Other,
    }
}

/// The primitive type names (generic-constraints.md §2.2 companion table). A
/// primitive bound has no in-program `TypeDef`, so `T : float` is a
/// primitive-name bound (deferred), and a primitive **arg** against `T : class`
/// is provably violating (CT-T3 uses this fact). Mirrors `is_primitive_name`
/// (`lower.rs`).
fn is_primitive_name(name: &str) -> bool {
    matches!(
        name,
        "void"
            | "bool"
            | "int"
            | "int64"
            | "intptr"
            | "int8"
            | "int16"
            | "int32"
            | "uint"
            | "uint64"
            | "uintptr"
            | "uint8"
            | "char8"
            | "uint16"
            | "char16"
            | "uint32"
            | "char32"
            | "float"
            | "double"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use newbf_lexer::FileId;
    use newbf_parser::parse_file;

    use crate::build::SourceFile;

    /// Parse `src`, analyze it, build the type index, then classify the FIRST
    /// constraint atom of the FIRST `where` clause on the method named `method`
    /// (declared in the first top-level type). Returns the kind label.
    fn classify_first(src: &str, method: &str) -> ConstraintKind {
        let (unit, pdiags) = parse_file(src, FileId(0));
        assert!(pdiags.is_empty(), "must parse clean: {pdiags:?}");
        let files = [SourceFile {
            file: FileId(0),
            src,
            unit: &unit,
            name: "",
        }];
        let program = crate::analyze(&files);
        let index = TypeIndex::build(&program.graph, &program.interner);

        for it in &unit.items {
            let Item::Type(td) = it else { continue };
            for m in &td.members {
                let Member::Method {
                    name,
                    constraints,
                    generic_params,
                    ..
                } = m
                else {
                    continue;
                };
                if name.text(src) == method && !constraints.is_empty() {
                    return classify_constraint(
                        &constraints[0].constraints[0],
                        generic_params,
                        &index,
                        src,
                    );
                }
            }
        }
        panic!("no method named {method} with a where clause");
    }

    #[test]
    fn classifier_labels_each_kind() {
        let src = "\
interface IFace { }
class Base { }
class Holder {
    public static void MStruct<T>(T v) where T : struct { }
    public static void MClass<T>(T v) where T : class { }
    public static void MNew<T>(T v) where T : new { }
    public static void MIface<T>(T v) where T : IFace { }
    public static void MBase<T>(T v) where T : Base { }
    public static void MOp<T>(T v) where float : operator T * T { }
    public static void MConst<C>() where C : const int { }
    public static void MTypeParam<T, T2>(T v) where T : T2 { }
    public static void MPrim<T>(T v) where T : float { }
    public static void MGeneric<T>(T v) where T : IFace2<T> { }
    public static void MUnres<T>(T v) where T : ISomethingExternal { }
}
";
        assert_eq!(classify_first(src, "MStruct"), ConstraintKind::Struct);
        assert_eq!(classify_first(src, "MClass"), ConstraintKind::Class);
        assert_eq!(classify_first(src, "MNew"), ConstraintKind::New);
        assert!(matches!(
            classify_first(src, "MIface"),
            ConstraintKind::Interface(_)
        ));
        assert!(matches!(
            classify_first(src, "MBase"),
            ConstraintKind::BaseClass(_)
        ));
        assert_eq!(classify_first(src, "MOp"), ConstraintKind::OperatorBound);
        // `const int` arrives as the bare path `int` (parser strips `const`),
        // so it classifies as a primitive-name bound (deferred, like the operator
        // and generic forms).
        assert!(matches!(
            classify_first(src, "MConst"),
            ConstraintKind::PrimitiveName(_)
        ));
        assert!(matches!(
            classify_first(src, "MTypeParam"),
            ConstraintKind::TypeParam(_)
        ));
        assert!(matches!(
            classify_first(src, "MPrim"),
            ConstraintKind::PrimitiveName(_)
        ));
        assert!(matches!(
            classify_first(src, "MGeneric"),
            ConstraintKind::GenericBound(_)
        ));
        assert!(matches!(
            classify_first(src, "MUnres"),
            ConstraintKind::Unresolved(_)
        ));

        // The supported/deferred split is correctly labeled.
        assert!(ConstraintKind::Struct.is_supported());
        assert!(ConstraintKind::Class.is_supported());
        assert!(ConstraintKind::New.is_supported());
        assert!(!ConstraintKind::OperatorBound.is_supported());
        assert!(!ConstraintKind::Pointer.is_supported());
    }
}
