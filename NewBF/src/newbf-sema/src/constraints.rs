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

use std::collections::{HashMap, HashSet};

use newbf_lexer::Span;
use newbf_parser::{
    Accessor, Expr, GenericParam, InterpPart, Item, Member, MethodBody, Stmt, SwitchArm, Type,
    TypeDecl, WhereClause,
};

use crate::Diagnostic;
use crate::build::SourceFile;
use crate::intern::Interner;
use crate::model::{DefGraph, TypeId, TypeKindD, TypeRef};

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
/// diagnostics.
///
/// Two phases run here:
///  * **CT-T2** (declaration-level): the [`Cx`] item walk classifies every
///    clause and emits the clause-internal `class` ∧ `struct` contradiction.
///  * **CT-T3** (instantiation-level, the high-value `Use<int32>` check): an
///    index of generic method/function decls (skipping any `(name, arity)` that
///    matches >1 decl — overloads) is built, then the bodies are re-walked to
///    collect explicit-type-arg call instantiations `Name<Args>(…)` /
///    `Recv.Name<Args>(…)`, each validated against its decl's supported
///    constraints via the transitive implements/base walk (with a
///    `HashSet<TypeId>` cycle guard, R12). Only a PROVABLE violation diagnoses;
///    every uncertainty (overload, unresolvable arg/constraint/base, deferred
///    kind) is **skipped**.
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
    // CT-T2: declaration-level classification + contradiction.
    for f in files {
        cx.walk_items(&f.unit.items, f.src);
    }
    // CT-T3: build the generic-decl index (overload-aware), then re-walk bodies
    // for call instantiations and validate each against its decl's constraints.
    let decls = GenericDeclIndex::build(files, &index);
    let mut icx = InstCx {
        index: &index,
        decls: &decls,
        graph,
        interner,
        diags: &mut diags,
    };
    for f in files {
        icx.walk_items(&f.unit.items, f.src);
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
    /// Every type's `TypeKindD` keyed by its own `TypeId` (so CT-T3 can read the
    /// kind of a *resolved* arg directly, independent of first-wins name choice).
    kind_by_id: HashMap<TypeId, TypeKindD>,
    /// `(simple name, arity)` keys that match **more than one** in-program type
    /// (a simple-name collision, §6 item 8). Under the per-file ratchet these are
    /// rare, but a MULTI-FILE configuration (GC-T4: corlib-slice co-analyzed with
    /// the feature suite, or several feature files together) makes them common:
    /// e.g. `struct StructA` appears in `Constraints.bf`/`Generics.bf`/
    /// `Interfaces.bf` with DIFFERENT bases. First-wins would pick ONE of them and
    /// could `Violated`-mismatch a call that actually instantiates a DIFFERENT
    /// same-named type (e.g. `Alloc2<StructA>()` in `Generics.bf` where
    /// `StructA : IDisposable`, but first-wins binds `Constraints.bf`'s base-less
    /// `StructA`). An ambiguous name is therefore treated as **unresolvable** for
    /// validation (the `lookup`s below return `None`), so the GC-T3 check SKIPS it
    /// — a conservative extension of the any-base-unresolvable ⇒ skip rule (§3.2)
    /// that closes the configuration-dependence false positive R1/§6.7 warns of.
    ambiguous: HashSet<(String, u32)>,
}

impl TypeIndex {
    fn build(graph: &DefGraph, interner: &Interner) -> Self {
        let mut by_name_arity = HashMap::new();
        let mut kind_by_name_arity = HashMap::new();
        let mut kind_by_id = HashMap::new();
        let mut ambiguous = HashSet::new();
        for (i, t) in graph.types.iter().enumerate() {
            let id = TypeId(i as u32);
            kind_by_id.insert(id, t.kind);
            let name = interner.resolve(t.name).to_string();
            // First-wins within an arity for the chosen TypeId/kind, but ALSO
            // record any second-or-later occurrence as ambiguous: a name matching
            // >1 type cannot be resolved to a single TypeId without picking
            // arbitrarily, so the validation treats it as unresolvable (skip).
            let key = (name, t.arity);
            if !by_name_arity.contains_key(&key) {
                by_name_arity.insert(key.clone(), id);
                kind_by_name_arity.insert(key, t.kind);
            } else {
                ambiguous.insert(key);
            }
        }
        Self {
            by_name_arity,
            kind_by_name_arity,
            kind_by_id,
            ambiguous,
        }
    }

    /// The `TypeKindD` of a resolved type by its `TypeId`.
    fn kind_by_name_arity_of(&self, id: TypeId) -> Option<TypeKindD> {
        self.kind_by_id.get(&id).copied()
    }

    /// Look up a bare simple name as an arity-0 entry (a `where T : IFace` with
    /// no `<…>` binds the non-generic entry — mirrors `index_generic_decls` /
    /// `check_duplicate_types`). Returns `None` when unresolvable in this program
    /// — including when the name is **ambiguous** (matches >1 arity-0 type), the
    /// conservative skip for a simple-name collision (§6 item 8, GC-T4).
    fn lookup_arity0(&self, name: &str) -> Option<TypeId> {
        self.lookup(name, 0)
    }

    /// Look up a simple name at a specific arity. Used by the transitive base
    /// walk to resolve a base reference (`Singleton<ClassC>` → arity 1; `IFaceB`
    /// → arity 0). Returns `None` when unresolvable in this program — including
    /// when the name is **ambiguous** (matches >1 type at this arity): the
    /// any-base-unresolvable ⇒ skip rule (§3.2) then fires and the whole check is
    /// skipped, never a false positive against an arbitrarily-chosen collision
    /// winner (GC-T4 / R1).
    fn lookup(&self, name: &str, arity: u32) -> Option<TypeId> {
        let key = (name.to_string(), arity);
        if self.ambiguous.contains(&key) {
            return None;
        }
        self.by_name_arity.get(&key).copied()
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

// ── CT-T3: method-call instantiation enforcement ─────────────────────────────
//
// The high-value `Use<int32>` check. It (1) indexes every generic method/ctor
// decl that carries a `where`-clause by `(name, arity)` — recording per-param
// constraint kinds and **flagging any `(name, arity)` that matches >1 decl as
// overloaded** (skip, can't disambiguate); (2) re-walks the method bodies
// collecting explicit-type-arg call instantiations `Name<Args>(…)` /
// `Recv.Name<Args>(…)`; (3) validates each call against its (uniquely-resolved,
// non-overloaded) decl's supported constraints via the transitive base/iface
// walk with a `HashSet<TypeId>` cycle guard (R12). Only a PROVABLE violation
// diagnoses; every uncertainty is skipped.

/// The per-param constraints of one generic decl: the parameter NAMES in
/// declaration order (so the i-th type-arg of a call maps to the i-th param)
/// plus, per param name, the classified constraint kinds.
struct GenericDecl {
    /// Generic parameter names in declaration order (arg position i ↔ param i).
    param_names: Vec<String>,
    /// Constraint kinds per parameter name (only the kinds that classified).
    constraints: HashMap<String, Vec<ConstraintKind>>,
}

/// The `(decl_name, arity) -> GenericDecl` index for generic method/ctor decls
/// that carry a `where`-clause. An `(name, arity)` seen on more than one decl is
/// recorded as **overloaded** (its entry is removed and the key is poisoned) —
/// CT-T3 cannot tell which overload a call resolves to (constraint-directed
/// overload resolution is unimplemented), so it skips every such call
/// (ratchet-critical: `MethodA<T>`×4 in `Generics.bf`).
struct GenericDeclIndex {
    by_name_arity: HashMap<(String, u32), GenericDecl>,
    /// `(name, arity)` keys that matched >1 decl → skip every matching call.
    overloaded: HashSet<(String, u32)>,
}

impl GenericDeclIndex {
    fn build(files: &[SourceFile<'_>], index: &TypeIndex) -> Self {
        let mut me = GenericDeclIndex {
            by_name_arity: HashMap::new(),
            overloaded: HashSet::new(),
        };
        for f in files {
            me.collect_items(&f.unit.items, f.src, index);
        }
        me
    }

    fn collect_items(&mut self, items: &[Item], src: &str, index: &TypeIndex) {
        for it in items {
            match it {
                Item::Namespace { body: Some(b), .. } => self.collect_items(b, src, index),
                Item::Type(td) => self.collect_type(td, src, index),
                _ => {}
            }
        }
    }

    fn collect_type(&mut self, td: &TypeDecl, src: &str, index: &TypeIndex) {
        for m in &td.members {
            match m {
                Member::Method {
                    name,
                    generic_params,
                    constraints,
                    ..
                } => self.record(name.text(src), generic_params, constraints, src, index),
                Member::Constructor {
                    generic_params,
                    constraints,
                    ..
                } => self.record("this", generic_params, constraints, src, index),
                Member::Nested(n) => self.collect_type(n, src, index),
                _ => {}
            }
        }
    }

    /// Record one generic decl. A non-generic decl (no generic params) carries
    /// no instantiation to validate → skipped. A second decl with the same
    /// `(name, arity)` poisons the key (overloaded → skip).
    fn record(
        &mut self,
        name: &str,
        generic_params: &[GenericParam],
        constraints: &[WhereClause],
        src: &str,
        index: &TypeIndex,
    ) {
        if generic_params.is_empty() {
            return;
        }
        let arity = generic_params.len() as u32;
        let key = (name.to_string(), arity);
        if self.overloaded.contains(&key) {
            return;
        }
        if self.by_name_arity.remove(&key).is_some() {
            // A second decl with this (name, arity): overloaded → poison the key.
            self.overloaded.insert(key);
            return;
        }
        let param_names: Vec<String> = generic_params
            .iter()
            .map(|gp| gp.name.text(src).to_string())
            .collect();
        // Classify every clause atom (body-first), grouped by the constrained
        // parameter NAME. `generic_params` is the scope for `T : T2` recognition.
        let mut cmap: HashMap<String, Vec<ConstraintKind>> = HashMap::new();
        for clause in constraints {
            let pname = clause.name.text(src).to_string();
            for atom in &clause.constraints {
                let kind = classify_constraint(atom, generic_params, index, src);
                cmap.entry(pname.clone()).or_default().push(kind);
            }
        }
        self.by_name_arity.insert(
            key,
            GenericDecl {
                param_names,
                constraints: cmap,
            },
        );
    }

    /// Look up a non-overloaded generic decl for a call's `(name, arity)`.
    /// Returns `None` for an unknown or overloaded key (→ skip the call).
    fn lookup(&self, name: &str, arity: u32) -> Option<&GenericDecl> {
        let key = (name.to_string(), arity);
        if self.overloaded.contains(&key) {
            return None;
        }
        self.by_name_arity.get(&key)
    }
}

/// A collected call instantiation: the decl's simple name, its arity, the
/// concrete type-arg names (or `None` for an arg whose shape is not a bare
/// single-segment no-generic-args path — e.g. a pointer/array/tuple/generic
/// arg, which makes that position unresolvable → skip its validation), and the
/// call span.
struct CallInst<'s> {
    name: &'s str,
    arity: u32,
    /// One entry per type-arg: `Some(simple_name)` for a bare single-segment
    /// no-args path, `None` otherwise (unresolvable arg position → skip).
    arg_names: Vec<Option<&'s str>>,
    span: Span,
}

/// The CT-T3 instantiation walker: re-walks method bodies, recording call
/// instantiations and validating each. Holds the resolved indexes + the graph
/// for the transitive base walk.
struct InstCx<'a> {
    index: &'a TypeIndex,
    decls: &'a GenericDeclIndex,
    graph: &'a DefGraph,
    interner: &'a Interner,
    diags: &'a mut Vec<Diagnostic>,
}

impl InstCx<'_> {
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
        for m in &td.members {
            match m {
                Member::Method { body, .. }
                | Member::Constructor { body, .. }
                | Member::Destructor { body, .. }
                | Member::Mixin { body, .. } => self.walk_body(body, src),
                Member::Property { accessors, .. } => {
                    for a in accessors {
                        self.walk_accessor(a, src);
                    }
                }
                Member::Nested(n) => self.walk_type(n, src),
                _ => {}
            }
        }
    }

    fn walk_accessor(&mut self, a: &Accessor, src: &str) {
        self.walk_body(&a.body, src);
    }

    fn walk_body(&mut self, body: &MethodBody, src: &str) {
        match body {
            MethodBody::Block(s) => self.walk_stmt(s, src),
            MethodBody::Expr(e) => self.walk_expr(e, src),
            MethodBody::None => {}
        }
    }

    fn walk_stmt(&mut self, s: &Stmt, src: &str) {
        match s {
            Stmt::Block { stmts, .. } | Stmt::Locals { decls: stmts, .. } => {
                for st in stmts {
                    self.walk_stmt(st, src);
                }
            }
            Stmt::Expr { expr, .. } => self.walk_expr(expr, src),
            Stmt::Local { init: Some(i), .. } => self.walk_expr(i, src),
            Stmt::Local { init: None, .. } | Stmt::Empty(_) => {}
            Stmt::If {
                cond, then, els, ..
            } => {
                self.walk_expr(cond, src);
                self.walk_stmt(then, src);
                if let Some(e) = els {
                    self.walk_stmt(e, src);
                }
            }
            Stmt::While { cond, body, .. } | Stmt::DoWhile { cond, body, .. } => {
                self.walk_expr(cond, src);
                self.walk_stmt(body, src);
            }
            Stmt::For {
                init,
                init_extra,
                cond,
                update,
                update_extra,
                body,
                ..
            } => {
                if let Some(i) = init {
                    self.walk_stmt(i, src);
                }
                for e in init_extra {
                    self.walk_stmt(e, src);
                }
                if let Some(c) = cond {
                    self.walk_expr(c, src);
                }
                for u in update_extra {
                    self.walk_expr(u, src);
                }
                if let Some(u) = update {
                    self.walk_expr(u, src);
                }
                self.walk_stmt(body, src);
            }
            Stmt::ForEach { iter, body, .. } => {
                self.walk_expr(iter, src);
                self.walk_stmt(body, src);
            }
            Stmt::Return { value: Some(v), .. } => self.walk_expr(v, src),
            Stmt::YieldReturn { value, .. } => self.walk_expr(value, src),
            Stmt::Return { value: None, .. }
            | Stmt::YieldBreak { .. }
            | Stmt::Break { .. }
            | Stmt::Continue { .. } => {}
            Stmt::Defer { body, .. } => self.walk_stmt(body, src),
            Stmt::Switch {
                scrutinee, arms, ..
            } => {
                self.walk_expr(scrutinee, src);
                for arm in arms {
                    self.walk_switch_arm(arm, src);
                }
            }
            Stmt::LocalFunction { body, .. } => self.walk_stmt(body, src),
            _ => {}
        }
    }

    fn walk_switch_arm(&mut self, arm: &SwitchArm, src: &str) {
        if let Some(p) = &arm.pattern {
            self.walk_expr(p, src);
        }
        for e in &arm.extra {
            self.walk_expr(e, src);
        }
        if let Some(g) = &arm.guard {
            self.walk_expr(g, src);
        }
        for st in &arm.body {
            self.walk_stmt(st, src);
        }
    }

    /// Walk an expression, recording a generic call instantiation when one is
    /// found and recursing into every sub-expression.
    fn walk_expr(&mut self, e: &Expr, src: &str) {
        // Recognise a call whose callee is a generic-instantiated name:
        //   `Name<Args>(…)`         → callee == Generic { base: Ident, args }
        //   `Recv.Name<Args>(…)`    → callee == Generic { base: Member, args }
        if let Expr::Call { callee, .. } = e
            && let Expr::Generic { base, args, .. } = strip_paren(callee)
            && let Some(name) = callee_simple_name(base, src)
        {
            let inst = CallInst {
                name,
                arity: args.len() as u32,
                arg_names: args.iter().map(|a| type_simple_name(a, src)).collect(),
                span: e.span(),
            };
            self.validate(&inst, src);
        }
        self.walk_children(e, src);
    }

    /// Recurse into every sub-expression of `e` (so nested calls are collected).
    fn walk_children(&mut self, e: &Expr, src: &str) {
        match e {
            Expr::Paren { inner, .. }
            | Expr::Unary { operand: inner, .. }
            | Expr::PostInc { operand: inner, .. }
            | Expr::PostDec { operand: inner, .. }
            | Expr::Prefix { operand: inner, .. }
            | Expr::Cast { operand: inner, .. }
            | Expr::Member { base: inner, .. }
            | Expr::Generic { base: inner, .. } => self.walk_expr(inner, src),
            Expr::Binary { lhs, rhs, .. } => {
                self.walk_expr(lhs, src);
                self.walk_expr(rhs, src);
            }
            Expr::Assign { target, value, .. } => {
                self.walk_expr(target, src);
                self.walk_expr(value, src);
            }
            Expr::Ternary {
                cond, then, els, ..
            } => {
                self.walk_expr(cond, src);
                self.walk_expr(then, src);
                self.walk_expr(els, src);
            }
            Expr::Call { callee, args, .. } | Expr::MixinCall { callee, args, .. } => {
                self.walk_expr(callee, src);
                for a in args {
                    self.walk_expr(a, src);
                }
            }
            Expr::Index { base, args, .. } => {
                self.walk_expr(base, src);
                for a in args {
                    self.walk_expr(a, src);
                }
            }
            Expr::Tuple { elems, .. } => {
                for el in elems {
                    self.walk_expr(el, src);
                }
            }
            Expr::Initializer { base, entries, .. } => {
                self.walk_expr(base, src);
                for en in entries {
                    self.walk_expr(en, src);
                }
            }
            Expr::Named { value, .. } => self.walk_expr(value, src),
            Expr::Interp { parts, .. } => {
                for p in parts {
                    if let InterpPart::Hole(h) = p {
                        self.walk_expr(h, src);
                    }
                }
            }
            Expr::Lambda { body, .. } => self.walk_stmt(body, src),
            // Leaves (idents/literals/this/base/dot-ident/sizeof/typeof/error):
            // no sub-expression to recurse into.
            _ => {}
        }
    }

    /// Validate one collected call against its decl's supported constraints.
    /// Skips on ANY uncertainty (overload, missing decl, unresolvable arg /
    /// constraint / base, deferred kind). Emits at most ONE diagnostic per
    /// provable violation (the FIRST proven mismatch on the call).
    fn validate(&mut self, inst: &CallInst<'_>, _src: &str) {
        let Some(decl) = self.decls.lookup(inst.name, inst.arity) else {
            // Unknown or overloaded `(name, arity)` → skip.
            return;
        };
        // Map each type-arg position to its parameter name, then to that param's
        // constraints. Validate position by position; emit only the FIRST proven
        // violation so a call yields at most one diagnostic.
        for (i, arg_name) in inst.arg_names.iter().enumerate() {
            let Some(pname) = decl.param_names.get(i) else {
                continue;
            };
            let Some(kinds) = decl.constraints.get(pname) else {
                continue;
            };
            // An arg position that is not a bare resolvable simple name → skip
            // (unresolvable arg, the conservative rule).
            let Some(arg_name) = arg_name else {
                continue;
            };
            for kind in kinds {
                if let Some(message) = self.check_one(kind, arg_name, pname) {
                    self.diags.push(Diagnostic {
                        span: inst.span,
                        message,
                    });
                    return;
                }
            }
        }
    }

    /// Validate one supported constraint `kind` against the concrete type-arg
    /// `arg_name`. Returns `Some(message)` on a PROVABLE violation, `None`
    /// otherwise (satisfied, OR uncertain → skip). Deferred kinds always return
    /// `None`.
    fn check_one(&self, kind: &ConstraintKind, arg_name: &str, pname: &str) -> Option<String> {
        // Resolve the concrete arg's kind: a primitive is a known value type
        // (implements no in-program interface, derives from no in-program class);
        // otherwise look it up as an arity-0 in-program type.
        let arg_is_primitive = is_primitive_name(arg_name);
        let arg_id = self.index.lookup_arity0(arg_name);
        let arg_kind = arg_id.and_then(|id| self.index.kind_by_name_arity_of(id));
        match kind {
            // `where T : class` — the arg must be a reference type (Class). A
            // primitive (value) or an in-program struct/enum is a provable
            // violation. An unresolvable, non-primitive arg → skip.
            ConstraintKind::Class => {
                if arg_is_primitive {
                    return Some(violation_msg(pname, arg_name, "class", "a value type"));
                }
                match arg_kind {
                    Some(TypeKindD::Class) => None,
                    Some(TypeKindD::Struct) => {
                        Some(violation_msg(pname, arg_name, "class", "a value struct"))
                    }
                    Some(TypeKindD::Enum) => {
                        Some(violation_msg(pname, arg_name, "class", "an enum"))
                    }
                    // Interface/delegate/alias/extension or unresolvable → skip.
                    _ => None,
                }
            }
            // `where T : struct` — the arg must be a value type (Struct OR a
            // primitive). An in-program class is a provable violation.
            ConstraintKind::Struct => {
                if arg_is_primitive {
                    return None; // primitives are value types — satisfied.
                }
                match arg_kind {
                    Some(TypeKindD::Class) => {
                        Some(violation_msg(pname, arg_name, "struct", "a reference class"))
                    }
                    // Struct → satisfied; everything else (enum/iface/…/unres) → skip.
                    _ => None,
                }
            }
            // `where T : IFace` — the arg's type must transitively implement
            // `IFace`. A primitive provably does not. An in-program type that
            // provably does not (and whose whole base chain resolved) → violation.
            ConstraintKind::Interface(iface) => {
                let Some(target) = self.index.lookup_arity0(iface) else {
                    return None; // the constraint iface didn't resolve → skip.
                };
                if arg_is_primitive {
                    return Some(violation_msg(
                        pname,
                        arg_name,
                        iface,
                        "a primitive that implements no in-program interface",
                    ));
                }
                let Some(start) = arg_id else {
                    return None; // unresolvable non-primitive arg → skip.
                };
                match self.transitive_reaches(start, target) {
                    Decision::Satisfied => None,
                    Decision::Violated => Some(violation_msg(
                        pname,
                        arg_name,
                        iface,
                        "it does not implement that interface",
                    )),
                    Decision::Skip => None,
                }
            }
            // `where T : Base` — the arg's class base chain must reach `Base`.
            ConstraintKind::BaseClass(base) => {
                let Some(target) = self.index.lookup_arity0(base) else {
                    return None; // the constraint base didn't resolve → skip.
                };
                if arg_is_primitive {
                    return Some(violation_msg(
                        pname,
                        arg_name,
                        base,
                        "a primitive does not derive from that class",
                    ));
                }
                let Some(start) = arg_id else {
                    return None; // unresolvable non-primitive arg → skip.
                };
                match self.transitive_reaches(start, target) {
                    Decision::Satisfied => None,
                    Decision::Violated => Some(violation_msg(
                        pname,
                        arg_name,
                        base,
                        "it does not derive from that class",
                    )),
                    Decision::Skip => None,
                }
            }
            // `where T : new` — DEFERRED in v1 (the doc's rule is hard to prove
            // conservatively for the corpus; structs/primitives always satisfy
            // and a class's ctor visibility is not reliably resolvable here).
            // Recognised-and-skipped — never a false positive.
            ConstraintKind::New => None,
            // Every other kind is a deferred form → skip.
            _ => None,
        }
    }

    /// Transitive reachability of `target` from `start` through the
    /// `TypeDef.bases` chain (covers both class-base and interface-implements,
    /// since both are recorded in `bases`). Carries a `HashSet<TypeId>` visited
    /// guard (R12 — self-referential/mutually-recursive bounds like
    /// `Singleton<T> where T : Singleton<T>` would otherwise hang). Returns
    /// `Skip` the moment **any** base is unresolvable (the any-base-unresolvable
    /// ⇒ skip rule, §3.2 — never a false positive), `Satisfied` when `target` is
    /// reached, `Violated` only when the whole chain resolved and `target` was
    /// never found.
    fn transitive_reaches(&self, start: TypeId, target: TypeId) -> Decision {
        let mut visited: HashSet<TypeId> = HashSet::new();
        let mut stack = vec![start];
        while let Some(cur) = stack.pop() {
            if cur == target {
                return Decision::Satisfied;
            }
            if !visited.insert(cur) {
                continue;
            }
            for base_ref in &self.graph.ty(cur).bases {
                match self.resolve_base(base_ref) {
                    Some(bid) => stack.push(bid),
                    // A base that does not resolve in this program (External, a
                    // generic-arg base whose name+arity misses, a non-path base)
                    // ⇒ skip the WHOLE check — never a false positive.
                    None => return Decision::Skip,
                }
            }
        }
        Decision::Violated
    }

    /// Resolve a base `TypeRef` to a `TypeId` via the `(simple-name, arity)`
    /// index. A base path's last segment supplies the name and arity (so a
    /// generic base `Singleton<ClassC>` resolves to the arity-1 `Singleton`).
    /// Returns `None` for any non-path base or an unresolvable name+arity.
    fn resolve_base(&self, base: &TypeRef) -> Option<TypeId> {
        let TypeRef::Path { segments, .. } = base else {
            return None;
        };
        let last = segments.last()?;
        let name = self.interner.resolve(last.name);
        let arity = last.args.len() as u32;
        self.index.lookup(name, arity)
    }
}

/// The outcome of a transitive base/iface reachability query.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Decision {
    /// `target` was reached — the constraint is satisfied.
    Satisfied,
    /// The whole base chain resolved in-program and `target` was never reached
    /// — a PROVABLE violation.
    Violated,
    /// A base was unresolvable (External / generic / non-path) — skip the whole
    /// check (never a false positive).
    Skip,
}

/// Build the diagnostic message for a provable instantiation-level violation.
fn violation_msg(pname: &str, arg_name: &str, bound: &str, why: &str) -> String {
    format!(
        "type argument `{arg_name}` for generic parameter `{pname}` does not satisfy \
         constraint `{pname} : {bound}` ({why}) — constraint violation"
    )
}

/// The simple (last-segment) name a generic-call callee base names:
/// `Name<…>(…)` → `Name`, `Recv.Name<…>(…)` → `Name`. Returns `None` for any
/// other base shape (so an exotic callee is conservatively not collected).
fn callee_simple_name<'s>(base: &Expr, src: &'s str) -> Option<&'s str> {
    match strip_paren(base) {
        Expr::Ident(s) => Some(s.text(src)),
        Expr::Member { name, .. } => Some(name.text(src)),
        _ => None,
    }
}

/// The simple name a type-arg names IF it is a bare single-segment no-generic-
/// args path (`int32`, `Holder`, `ClassA`). Returns `None` for any other shape
/// (pointer/array/tuple/generic/qualified/computed/var/error) so that arg
/// position is treated as unresolvable → its validation is skipped.
fn type_simple_name<'s>(t: &Type, src: &'s str) -> Option<&'s str> {
    let Type::Path { segments, .. } = t else {
        return None;
    };
    if segments.len() != 1 {
        return None;
    }
    let seg = &segments[0];
    if !seg.args.is_empty() {
        return None;
    }
    Some(seg.name.text(src))
}

/// Peel `( … )` wrappers off an expression (CT-T3's local copy — the ownership
/// pass has its own).
fn strip_paren(e: &Expr) -> &Expr {
    match e {
        Expr::Paren { inner, .. } => strip_paren(inner),
        _ => e,
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
