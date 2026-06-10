# Generic Constraints — `where`-clause Enforcement + Constrained-`T` Operations — Design

> **Status: v1 LANDED (wave 3).** `where T : class/struct/IFace/BaseClass` violation
> diagnostics (the `Use<int32>` check) + the decl-internal `class ∧ struct`
> contradiction ship with zero false positives. v1 defers operator/const/delete/
> array/pointer/generic-interface/`T:T2`/`new` enforcement (see §scope below).

## 1. Overview

NewBF fully **parses** every Beef `where`-clause form and **captures** each as a
`WhereRef { name: Symbol, constraints: Vec<TypeRef>, span }` on the model's
`TypeDef`/`MethodDef` (`model.rs:297-301`, fields `constraints` at `model.rs:121,224`),
but **nothing reads them except the pretty-printer** (`report.rs:85-92`) — they are never
enforced. Meanwhile the *dispatch* half of "a constrained `T` can use its constraint"
already works **by accident of monomorphization**:
`static int32 Use<T>(T val) where T : IFace { return val.Get(); }` called as
`Use<Holder>(h)` works because monomorphization binds `T → Ref(Holder_id)`, so `val.Get()`
resolves on `Holder`'s real method table via `struct_base` (`lower.rs:9581`) → the
instance-call block (`lower.rs:11235`); the clause is discarded. This is the canonical
passing test `interface_constraint.bf` (`// expect: 100`).

**v1 capability (one paragraph).** Enforce the *resolvable, supported subset* of
`where`-clauses as **conservative constraint-violation diagnostics**, in a single sema
pass that runs in `analyze` **after** `check_delete_flow` (`lib.rs:85`) — the only seam
that simultaneously has the `DefGraph`, the `Interner`, **and** the method-body ASTs
(`files`), all of which the check needs (§3.2). It mirrors the structure of
`check_generic_method_guards` (`resolve.rs:180`, declaration-level) and the re-walk
pattern of `check_delete_flow` (`ownership.rs:112`, body-level). The supported constraint
kinds are **`T : IInterface`** (in-program non-generic interface), **`T : SomeClass`**
(in-program class base bound), **`T : class`**, **`T : struct`**, and **`T : new`**. A
constrained `T` continues to *use* its constraint through the **already-working
monomorphized concrete path** (no new dispatch code for the common case). The
erased/abstract-`T` interface dispatch path is documented as a **future appendix**
(Appendix A) — **not v1**, and (verified) currently **unreachable** because
`targ_is_abstract` (`lower.rs:1899-1913`) makes `record_method_inst` refuse abstract
type-args, so no lowering ever reaches a constrained-`T` call with an abstract receiver.

**Deferred** (recognized and skipped silently so the ratchet holds): operator constraints,
`const T`/`const String` (const-generic), `delete`, array/sized constraints
(`T : StringView[C]`), pointer-suffixed kind constraints (`T : struct*`, `T : int*`),
**generic interface** constraints (`T : IEnumerator<TElement>`, `T : IFaceD<int16>`),
**primitive-name bounds** (`T : float`), **type-parameter-to-type-parameter**
(`T : T2`), and **type-declaration-level `where`** (a `class C<T> where T : IFace`
violated only via a type-position instantiation, which the method-call collector does not
walk). The first four are exercised by `Constraints.bf`; `T : T2` and pointer-suffixed
kinds by `Generics.bf` (§5). The §5 rationale for each deferral is grounded in a concrete
corpus counterexample, not hand-waving.

The two halves, concretely:
- **(a) The static check** — a `where`-clause whose *supported* constraint a concrete
  method-call instantiation **provably** violates → a `Diagnostic` (e.g. `Use<int32>(…)`
  where `int32` is provably not an interface-implementing type). Conservative: only
  **provable** violations of **supported, in-program-resolvable** constraints fire;
  everything else is silent.
- **(b) The dispatch** — a constrained-`T` method call lowers correctly per monomorph. For
  the concrete case (`Use<Holder>`) this **already works**; this design pins it as a
  ratchet (CT-T0). The abstract-`T` path is out of v1 (Appendix A).

**Where the diagnostic actually gates (load-bearing — see §3.5).** Constraint diagnostics
flow through `Program.diagnostics` from `analyze`. The **driver/AOT** path inspects
`program.diagnostics` and fails the build (`newbf-driver/src/main.rs:251-254`). The
**run-corpus JIT** path does **NOT**: `run_corpus.rs` drives parse → `run_emission` →
`fold_comptime` → JIT and only ever inspects `emit.diagnostics`, never
`program.diagnostics`; and `run_emission` **drops round-0 base-program analyze diagnostics
by design** (`emit.rs:317` gates the surfacing on `!generated.is_empty()`, and the entire
current corpus is generator-free round-0). Therefore the negative (violating) programs are
**not** run-corpus value checks; CT-T3's acceptance asserts via a direct-`analyze`
`constraint_diags()` helper (like `double_free_diags`, `delete_flow.rs:31`), exactly as
delete-flow does.

## 2. Representation / ABI / IR changes

**No new `IrType` variant** (stays `Copy`, the HARD INVARIANT). **No new IR instruction.**
**No corlib type, no runtime symbol, no mangling change.** v1 is **entirely a sema-side
analysis** that emits only `Diagnostic`s plus one transient resolution index. It emits
**zero IR**, so it has **zero** SSA/dominance/ABI surface.

### 2.1 The sema⊥llvm contract (what sema emits by-name vs what llvm defines)

This feature **adds nothing to the sema↔llvm IR contract**: the enforcement half emits
only `Diagnostic`s (which never reach llvm — they flow through `Program.diagnostics` from
`analyze`, `lib.rs:54/86-90`), and the dispatch half emits IR shapes llvm **already
lowers today** (the existing concrete-monomorph `call`).

| Sema emits (by name / by shape) | llvm defines / lowers |
|---|---|
| `Diagnostic { span, message }` via `analyze` | nothing (diagnostics never reach the backend) |
| concrete monomorph call `call @Holder.Get(%body)` (existing, unchanged) | the already-emitted `@Holder.Get` symbol |

**No new global, no new extern, no widened struct.** newbf-llvm needs **zero** change.
newbf-sema gains **no** dependency on newbf-llvm (the whole feature lives in newbf-sema +
the model). The `IrType: Copy` and `StructTable`-owns-its-data invariants are untouched.

### 2.2 New owned data (model side, no lifetime added)

The enforcement check resolves a constraint *name* (`IFace`, a base class) to a model
`TypeId`. The model already binds names to `TypeId` for `using` resolution
(`resolve_dotted`/`type_in_ns`, `resolve.rs:41-65`) but only over the *namespace* index; a
constraint name in `where T : IFace` is typically an *unqualified* simple name. v1 builds
**one** read-only index, **inside the new `analyze`-phase pass** (not on `Builder` — see
§3.2 for why), from `graph.types`:

```rust
// Built in analyze, from the finished DefGraph. Keyed by (simple name, arity)
// — NOT by bare Symbol — so a generic `IFaceD<T>` (arity 1) and a non-generic
// `IFaceD` (arity 0) in the SAME file do not collide (Interfaces.bf:204 vs :280).
// A bare `where T : IFaceD` (no `<…>`) resolves to the arity-0 entry; mirrors
// `check_duplicate_types` (resolve.rs:120) and `index_generic_decls`
// (lower.rs:686), both of which already key by (name, arity).
type_by_name_arity: HashMap<(Symbol, u32), TypeId>,
```

First-wins **within an arity** (conservative: an ambiguous simple name resolves to one
`TypeId`; a violation only fires on a PROVABLE mismatch). This index is owned, transient,
built once per `analyze`, discarded when the pass returns. It adds no lifetime and touches
no ABI. The check reads `TypeDef.bases: Vec<TypeRef>` (`model.rs:120`) and `TypeDef.kind:
TypeKindD` (`model.rs:75-83`, the `Class`/`Struct`/`Interface` variants at lines 76/77/78)
— both already present — to decide implements/subtype/kind.

A small companion table maps **primitive type names** to a structural fact (§3.2,
finding-fix): `int32`/`float`/`bool`/… are *value* types that implement **no in-program
interface and derive from no in-program class*. This mirrors `primitive()`
(`lower.rs:12419-12438`) / `is_primitive_name` (`lower.rs:1917`). It lets a primitive arg
against `T : IFace` or `T : class` be **provably violating** rather than merely
"unresolvable" — which is what makes CT-T3's flagship `Use<int32>` check actually fire
(see §3.2 finding-fix).

## 3. The sema + parser + llvm + runtime changes, concretely

### 3.1 Parser / AST — **no change**

Every supported (and every deferred) constraint form already parses and is captured:
- `where_clauses()` (`parser.rs:2921`) loops `where` clauses; the constrained entity is an
  ident or a type expression (`parser.rs:2929-2950`).
- `constraint_atom()` (`parser.rs:2974`) handles `const Type` (`:2978-2981`),
  `operator …` → `Type::Var` (`:2984-2994`), and the keyword constraints
  `class`/`struct`/`new`/`delete`/`var`/`concrete`/`interface`/`enum` (`:2996-3017`), each
  synthesized as a **single-ident `Type::Path`** whose segment text is the keyword (so
  `class` arrives as a path segment named `"class"`), then passed through
  `type_suffixes` (`:3016`) — so `where T : struct*` is a `Type::Pointer` wrapping the
  `"struct"` path, **not** a bare keyword path (this is why pointer-suffixed kinds are
  deferred, §5).
- AST: `WhereClause { span, name: Span, constraints: Vec<Type> }` (`ast.rs:846-850`); on
  `TypeDecl.constraints`, `Member::Method.constraints`, `Member::Constructor.constraints`.
- Build/model: `lower_where` (`build.rs:565-577`) → `WhereRef { name: Symbol (interned),
  constraints: Vec<TypeRef>, span }`; the keyword/operator atoms become `TypeRef::Path`
  (single seg) / `TypeRef::Var` respectively (`model.rs:336-388`, `Var` at `:386`).

**The feature needs zero parser/AST/build work** — the model already carries everything.
Recognition of a keyword constraint compares `interner.resolve(seg.name)` against the
keyword set (the `WhereRef.name`/segment names are `Symbol`s, so it is an interner-resolve
+ string compare, and matching `WhereRef.name` against a decl's `generic_params: Vec<Symbol>`
is a `Symbol`-eq).

### 3.2 The enforcement seam — one pass in `analyze` (the static-check half, (a))

**Why one pass in `analyze`, not split across `resolve_and_check`.** The instantiation
collector must walk method **bodies**, which the model does not carry (`lib.rs` builds the
`DefGraph` with no bodies). `resolve_and_check` runs on the transient `Builder`
(`lib.rs:64`), and `Builder` (`build.rs:27-38`) retains only
`interner/namespaces/types/members/usings/ns_index` + two cursors — **it has no `files`/AST
access**. Body re-walking is only possible later, in `analyze`, which passes `files` to
`check_delete_flow` (`lib.rs:85`, signature `(files, &DefGraph, &Interner)`,
`ownership.rs:112`) **after** the `DefGraph` is constructed (`lib.rs:66-72`). So the whole
constraint pass runs there, immediately after `check_delete_flow`:

```rust
// lib.rs::analyze, after the delete-flow line (currently lib.rs:85):
diagnostics.extend(ownership::check_delete_flow(files, &graph, &builder.interner));
diagnostics.extend(
    constraints::check_generic_constraints(files, &graph, &builder.interner)
);   // NEW (CT-T1/T2/T3): builds type_by_name_arity from graph.types, then
     //   (i) declaration-level checks over graph.types/graph.members (CT-T2),
     //   (ii) re-walks `files` bodies for method-call instantiations (CT-T3).
```

This keeps the new code in newbf-sema (a new `constraints.rs` module beside `ownership.rs`),
mirrors the delete-flow signature exactly, and **avoids** the wide
`lower_program -> (Module, Vec<Diagnostic>)` signature change that option (ii) of an earlier
draft considered — which is an explicit **non-goal** (it would touch `run_corpus.rs`,
`corpus.rs`, the AOT driver, and `newbf-comptime::run_emission`). The model pass has **no**
instantiation information of its own (monomorphization lives only in `lower.rs`
`record_inst`, `:1671/1709-1714`); CT-T3 reconstructs the *shapes* it needs by walking the
ASTs, registering **no** mono (so it cannot trip the `(name,arity)` fixpoint, §6.3).

#### CT-T2 — declaration-level violations (over `graph.types`/`graph.members`)

Decidable **without** instantiation. v1's only declaration-level diagnostic:
**clause-internal kind contradiction** — a single parameter constrained both `class` and
`struct` in the same decl's clauses (mutually exclusive `TypeKindD` kinds). This needs no
resolution and no instantiation.

**Classification ordering is load-bearing (ratchet-critical).** CT-T2 classifies by the
**constraint body FIRST**, never by the constrained name. If *any* constraint atom on a
clause is a deferred form — `TypeRef::Var` (operator), a `const`-prefixed type, a
generic-instantiation (`segments` with non-empty `args`), an array/sized/pointer-suffixed
type, or a name that resolves to nothing / a primitive / a generic interface — the **entire
clause is skipped unconditionally**, before the name is even inspected. This is mandatory
because real corpus clauses constrain a **non-parameter** entity: `where float : operator T
* T` (`Constraints.bf:55`), `where char8 : operator implicit T` (`:55`), `where int16 :
operator T + T` (`:73`), `where double : operator T - T` (`:64`), `where StructA : operator
explicit T` (`:64`), `where bool : …` — here `WhereRef.name` is `float`/`char8`/`int16`/…,
**not** a generic parameter. A naive "where on a non-generic-parameter name → diagnose"
check would fire on every one of these and break `clean == files.len()`. **There is no
"non-generic-parameter name" diagnostic in v1** — in Beef the constrained entity may
legitimately be a non-parameter type.

#### CT-T3 — method-call instantiation violations (re-walk `files`, the high-value check)

CT-T3 walks the same body surface as `check_delete_flow` and collects, **only for
method-call sites** `Name<Args>(…)` and `Recv.Name<Args>(…)` (and the explicit-type-arg
forms in the corpus, e.g. `Method2<…>(…)`, `MethodG<…>()`), the triple
`(generic_decl_simple_name, arity, [concrete_arg_type_names], span)`. It does **NOT** walk
type-position instantiations (`ClassC<Foo> c = …`, `new ClassC<Foo>()`, field/param
types) — those are out of v1 (type-decl `where` enforcement is deferred, §5). For each
collected call it finds the matching generic decl and validates each supported constraint:

| Constraint syntax | Recognized as | v1 check | Resolve via |
|---|---|---|---|
| `T : IFace` (IFace an in-program **non-generic interface**) | interface bound | the concrete arg's type **transitively implements** `IFace` (see transitive-walk below); a **primitive** arg provably does not → diagnose; an in-program type that does not → diagnose | `type_by_name_arity` → `kind == Interface`, arity 0 |
| `T : Base` (Base an in-program **class**) | base bound | the concrete arg's **class base chain** (walked transitively) reaches `Base`; a primitive/struct arg provably does not → diagnose | `type_by_name_arity` → `kind == Class` |
| `T : class` (keyword path `"class"`) | reference-kind | concrete arg's `TypeKindD == Class` (a primitive/value-struct arg provably is not) → diagnose | keyword match on the single-seg path |
| `T : struct` (keyword path `"struct"`) | value-kind | concrete arg's `TypeKindD == Struct` **or arg is a primitive value type** → satisfied; a class arg → diagnose | keyword match (+ primitive table) |
| `T : new` (keyword path `"new"`) | constructible | **struct/primitive args: always satisfied** (implicit parameterless ctor). A **class** arg with explicitly-declared ctors **none parameterless** → diagnose; a class with no explicit ctor → satisfied | scan arg's `MemberDef::Method` with `method_kind == Constructor`, `params.is_empty()` |
| everything else (operator/`const`/`delete`/array/pointer-suffix/generic-iface/`T : T2`/primitive-name bound) | **deferred** | **skip silently** | recognized-and-ignored so the ratchet holds |

**Finding-fix — make `Use<int32>` actually fire (the flagship check).** An earlier draft's
conservatism — "skip the moment the arg is not fully resolvable in-program" — would skip
`Use<int32>` because `int32` is a primitive with **no `TypeDef`** (`type_by_name_arity`
misses it), defeating the one violation the feature most wants to catch. v1 closes this with
the **primitive table** (§2.2): a primitive arg is a *known* value type that implements no
in-program interface and derives from no in-program class, so `int32` against `T : IFace` or
`T : class` is **provably violating**, not "unresolvable." This is the difference between
CT-T3 delivering its marketed value and being honestly-but-uselessly conservative.

**Finding-fix — overloaded generic decls (ratchet-critical).** `Generics.bf:141/146/151/158`
declare **four** `MethodA<T>` overloads (arity 1) with `where T : var` / `struct` / `enum` /
`interface`, resolved by constraint and called at `:480-483`. The `(name, arity)` key
**cannot distinguish overloads**, and constraint-directed overload resolution is not
implemented anywhere in the tree. So `MethodA("")` (T=`String`, a class) validated against
the `where T : struct` overload would be a provable false positive → ratchet break.
**Rule: CT-T3 skips any call whose `(name, arity)` matches more than one generic decl.**
Overloaded generics are out of v1 enforcement.

**Transitive interface/base resolution (with cycle guard — ratchet-critical).**
`TypeDef.bases` is `Vec<TypeRef>` (`model.rs:120`) — **unresolved** path references, not
`TypeId`s — and there is no resolved base graph in the model (the itable/implements
computation lives in the StructTable's `apply_itables`, `lower.rs:1164`, which the model
pass cannot see). So CT-T3 implements its own bounded resolver:

```
fn implements(arg: TypeId, target: TypeId, g, idx) -> Decision:
    visited = HashSet::new()          // cycle guard — MANDATORY
    stack = [arg]
    while let Some(cur) = stack.pop():
        if cur == target: return Satisfied
        if !visited.insert(cur): continue          // already seen
        for base_ref in g.types[cur].bases:
            match resolve_base(base_ref, idx):       // simple-name → (name,arity) → TypeId
                Some(bid): stack.push(bid)
                None:      return Skip                // a base is unresolvable (External /
                                                      // generic-base path) ⇒ skip the WHOLE
                                                      // check — never a false positive
    return Violated   // (only reached when arg is fully in-program-resolvable and
                      //  the target was never found; primitive args short-circuit to
                      //  Violated via the primitive table before this walk)
```

The visited-set is mandatory: `Generics.bf:79` `class Singleton<T> where T : Singleton<T>`
is a **self-referential** bound and base chains can be mutually recursive; a naive walk
would not terminate (and termination is a HARD invariant — the corpus ratchet proves the
passes terminate). The **any-base-unresolvable ⇒ skip** rule is what keeps the satisfied
`Interfaces.bf` cases green: `StructA : IFaceB : IFaceA` (`:34,16`) satisfies `UseIA<T>
where T : IFaceA` (`:53`) only if the walk resolves `IFaceB` → recurses to `IFaceA`; any gap
(a base resolving `External`, a missed hop) must yield **Skip**, never Violated.

**Recognition default = skip.** Any unrecognized or unresolvable constraint is **skipped**
— exactly how `check_generic_method_guards` only rejects what it is sure about.

### 3.3 The conservatism rule (ratchet-load-bearing)

`corpus.rs::sema_does_not_panic_on_real_beef` asserts **`clean == files.len()`** — EVERY
of the **160** corpus files analyzes with **zero diagnostics** (`corpus.rs:106-111`), and
crucially **analyzes each file as its own one-file program** (`corpus.rs:61`, a single
`SourceFile`). The four constraint-dense files are in that set. Therefore:

> **Hard rule: `check_generic_constraints` must emit ZERO diagnostics on `Constraints.bf`,
> `Generics.bf`, `Generics2.bf`, `Interfaces.bf` under the per-file ratchet configuration.**
> Any supported-form check skips the moment its constraint-type, its argument, the enclosing
> generic decl, or any transitive base is not fully resolvable **in this one-file program**.
> Enforced *by* the corpus ratchet (a false positive is a hard test failure) and *additionally*
> pinned by a focused negative test (CT-T1's `constraint_diags`) asserting the constraint-
> diagnostic count is 0 on all four files.

**Configuration dependence (named risk — §6.7).** The per-file ratchet keeps corlib
interfaces *out of scope*: `IDisposable` is defined in `corlib-slice/System.bf` and
`IHashable` in `corlib-slice/IHashable.bf` — **separate files**. So when `Generics.bf` is
analyzed alone, `where T : IDisposable` (`:101/113/119`) and `Constraints.bf`'s `where K :
IHashable` (`:43`) are **unresolvable → skipped**. But a *driver build* co-analyzing
corlib-slice + feature-suite would make those interfaces in-program, turning them into
*supported* bounds — at which point CT-T3 must validate every `DoDispose<X>` / `Method3`
instantiation and could false-positive if an implementer isn't visibly an implementer.
v1's defense: the **transitive-walk's any-unresolvable ⇒ skip** rule plus the multi-file
negative pin (CT-T4) that co-analyzes corlib-slice + `Constraints.bf` and asserts 0
constraint diagnostics. This dependence is documented, not assumed away.

### 3.4 The dispatch half — concrete monomorph (half (b), already working)

For `Use<Holder>(h)`: `record_inst` (`lower.rs:1671`) binds `T → Ref(Holder_id)` in the
`env` (`:1709-1714`); the param `val` lowers via `lower_ty_env`'s env lookup
(`:12369-12371`) to `Ref(Holder_id)`; at `val.Get()`, `struct_base` (`:9581`) returns
`(body, Holder_id)`, the interface-dispatch branch (`:11227`) is skipped (Holder is a
class), and the **instance-call block** (`:11235-11285`) resolves `Get` on `Holder`'s real
`methods` table → a direct `call @Holder.Get`. **No constraint is consulted; no new code is
needed.** This design's job for half (b) is to **pin this as a non-regression** (CT-T0).

### 3.5 What "constraint-aware use" guarantees, and where it gates (the negative-of-dispatch)

Today, if `Holder` did **not** implement `Get`, the call at `val.Get()` falls through to the
undef catch-all `(undef(IrType::I64), IrType::I64)` (`lower.rs:11287`) — **silently**, no
diagnostic. The enforcement half (a) makes the *provably-violating instantiation* a
**`Diagnostic`** instead. So half (a) and half (b) are complementary: (b) makes the
*satisfied* call dispatch correctly; (a) makes the *unsatisfied* call a diagnostic.

**Where this gate is effective (corrected from an earlier draft's overclaim).** The
diagnostic does **not** "close the undef hole" on the run-corpus JIT path. That path
(`run_corpus.rs`) checks only `emit.diagnostics` and never `program.diagnostics`, and
`run_emission` deliberately **drops round-0 base-program analyze diagnostics**
(`emit.rs:317`, surfacing is gated on `!generated.is_empty()`; the whole corpus is
generator-free round-0). The hole is closed **only in the driver/AOT compile**, which
inspects `program.diagnostics` and fails (`newbf-driver/src/main.rs:251-254`). Consequently:
the violating programs are **not** run-corpus `// expect:` value checks — they are checked
via a direct-`analyze` `constraint_diags()` helper (§4, modeled on `double_free_diags`,
`delete_flow.rs:31`), which inspects `program.diagnostics` straight from `analyze`,
bypassing the JIT harness entirely. The undef at `lower.rs:11287` still executes under the
run-corpus JIT for a hypothetical violating program — but no violating program is in
run-corpus, exactly as no double-free fixture is.

### 3.6 llvm + runtime — **no change**

newbf-llvm: zero change (the feature emits no IR). newbf-runtime: zero change (no new
symbol; diagnostics are compile-time, the satisfied dispatch is an ordinary existing
`call`). No interaction with the memory guard (no alloc/delete/scope touched) and no float
constants (the JIT FP-constant-pool limit, per MEMORY, does not apply — the feature emits
no constants at all).

## 4. Worked examples (programs that prove it)

**Positive (dispatch, run-corpus, `e:/NewBF/beef-tests/run-corpus/`, must run +
value-check):**

1. `constraint_iface_use.bf` — `// expect: 100`. The canonical
   `Use<T>(T) where T : IFace { return val.Get(); }` with `Use<Holder>` — identical in
   spirit to the existing `interface_constraint.bf`; pins half (b) stays green. (CT-T0 may
   reuse `interface_constraint.bf` verbatim as the pin.)
2. `constraint_class_bound.bf` — `// expect: 7`. `Use<T>(T) where T : Animal` called with
   `Dog : Animal`; `val.Speak()` dispatches to `Dog.Speak` (base-bound, satisfied) → 7.
3. `constraint_struct_bound.bf` — `// expect: 9`. `Sum<T>(T) where T : struct` over a value
   struct; pins that the `struct` kind constraint accepts a value type and the call resolves
   on the concrete struct's methods.

   *(No `constraint_new.bf` run-corpus value check.* Verified: `new T()` on a bare
   type-parameter does **not** lower today — `new_class_id` (`lower.rs:9615`) resolves the
   operand via `ctor_class_name` → `structs.ty_of(name)` (`:9626-9629`, `ctor_class_name` at
   `:12022`), a struct-table lookup that does **NOT** consult the monomorph `env`, so a bare
   `new T()` yields `None` and never constructs the concrete mono. The `new`-constraint
   *enforcement* (decl + instantiation checks) needs no `new T()` lowering and lands in CT-T3;
   a `new T()` value program is demoted to a **verify-only pin** in `tests/constraints/`
   gated on a future env-aware `new_class_id`.)*

**Negative (enforcement, `newbf-sema/tests/constraints/`, kept OUT of the auto-collected
corpus — expected to diagnose, exactly like `tests/ownership/*.bf`,
`delete_flow.rs:5-8` — asserted via a direct-`analyze` `constraint_diags` helper):**

4. `violate_iface.bf` — `Use<int32>(…)` against `where T : IFace`; the **primitive** arg
   `int32` is provably not interface-implementing → exactly one
   "`int32` does not satisfy constraint `T : IFace`" diagnostic.
   `constraint_diags("violate_iface.bf") == 1`.
5. `violate_class_constraint.bf` — `where T : class` instantiated with a value struct
   (or a primitive); one diagnostic.
6. `violate_struct_constraint.bf` — `where T : struct` instantiated with an in-program
   class; one diagnostic.
7. `satisfied_no_diag.bf` — every supported constraint satisfied; **zero** diagnostics (the
   zero-false-positive negative, mirroring `delete_flow.rs`'s negatives).
8. `violate_decl_contradiction.bf` — a parameter constrained both `class` and `struct`
   across the same decl's clauses; one (CT-T2) diagnostic.

**Ratchet pin (sema corpus + focused negative, no behavioral run-corpus file):**

9. `Constraints.bf`, `Generics.bf`, `Generics2.bf`, `Interfaces.bf` keep analyzing with
   **zero** constraint diagnostics (CT-T1's `constraint_diags` per-file + CT-T4's multi-file
   co-analysis) and keep lowering verifier-clean (`llvm_lowering_verifies_on_real_beef`, the
   160/160 ratchet).

## 5. v1 scope vs explicitly deferred

**v1 (enforced — diagnostics only):**
- `where T : IInterface` — in-program **non-generic** interface bound; arg validated via
  the transitive `TypeDef.bases` walk (§3.2) with a primitive short-circuit.
- `where T : SomeClass` — in-program class base bound; arg validated via the transitive
  class base chain.
- `where T : class` / `where T : struct` — kind bounds (`TypeKindD`; structs/primitives
  satisfy `struct`).
- `where T : new` — parameterless-ctor bound; **structs/primitives always satisfy**, a
  class is diagnosed only when it has explicit ctors none of which are parameterless.
- **Dispatch (half b):** the concrete monomorphized path (already works) is pinned (CT-T0).

**Explicitly deferred (recognized, skipped silently — NO diagnostic), each with the corpus
reason it cannot be enforced in v1:**
- **Operator constraints** (`where float : operator T * T`, `where char8 : operator implicit
  T`) — `Constraints.bf:55,64,73,83-85`. Parsed as `TypeRef::Var` (`parser.rs:2994`);
  semantically deep. **Skip** (and they force the body-first classification of §3.2).
- **Const-generic constraints** (`where C : const int`, `where T : const String`) —
  `Constraints.bf:93`, `Generics.bf:554,562`. Needs const-arg evaluation. **Skip.**
- **`delete` constraints** (`where alloctype(T) : delete`) — `Generics.bf:171,178`.
  Disposability bound. **Skip.**
- **Array/sized constraints** (`where TS : StringView[C]`) — `Constraints.bf:94`. **Skip.**
- **Pointer-suffixed kind constraints** (`where T : struct*`, `where T : int*`) —
  `Generics.bf:194,202`. These are `TypeRef::Pointer` wrapping a keyword/primitive path
  (`type_suffixes`, `parser.rs:3016`), **not** a bare keyword path. **Skip.**
- **Primitive-name bounds** (`where T : float`) — `Constraints.bf:16`. `float` is a
  primitive with no in-program `TypeDef` → `type_by_name_arity` misses → **Skip**. (The
  primitive table from §2.2 is used to *validate args*, not to make a primitive a *bound
  target*.)
- **Generic-interface constraints** (`where TEnumerator : IEnumerator<TElement>`,
  `where T : IFaceD<int16>`) — `Constraints.bf:33`, `Interfaces.bf:265/270/275`. Generic
  interfaces are **not monomorphized at all** (`index_generic_decls` excludes them,
  `lower.rs:674-680`); `IEnumerator<T>` etc. have empty `imethods`. Recognized by a non-empty
  `args` on the constraint path's last segment. **Skip.** (Note: `Interfaces.bf` defines
  **both** `IFaceD<T>` at `:204` and a non-generic `IFaceD` at `:280`; the `(name, arity)`
  index keeps them distinct, so a bare `where T : IFaceD` (`:311`) binds the arity-0 one —
  but `IFaceD` is *also* corlib-resolution-dependent under the per-file ratchet and so skips
  there regardless.)
- **`where T : T2` (type-parameter-to-type-parameter)** — `Generics.bf:268` `MethodG<T,
  TBase> where T : TBase`. **Deferred** specifically because the corpus contains a
  **deliberately-violating** call: `MethodG<ClassF, ClassG>()` (`:521`) sits inside an
  `[IgnoreErrors(true)]` block (`:518-523`) — it is *expected* to fail the constraint
  (`ClassG` is not a base of `ClassF`; `ClassG : ClassF` at `:318`), while `MethodG<ClassG,
  ClassF>()` (`:524`) is valid. Enforcing `T : T2` would diagnose the `:521` call → ratchet
  break, and CT-T3 has no machinery to recognize that a call flows into an error-suppressed
  `[IgnoreErrors]` position. **Skip.**
- **Type-declaration-level `where`** (a `class C<T> where T : IFace`, `Generics2.bf:36`,
  violated only through a *type-position* instantiation `C<Foo> c = …` / `new C<Foo>()`).
  CT-T3's collector walks **method-call** sites only; type-position instantiation is a much
  larger AST surface and out of v1. Type-decl `where` clauses are still *classified* (so a
  clause-internal CT-T2 contradiction on a type decl is caught), but no instantiation-level
  enforcement fires for them. **Deferred.**
- **Abstract-`T` (un-monomorphized) constraint dispatch** — Appendix A; no corpus program
  needs it and it is currently **unreachable** (`targ_is_abstract`, `lower.rs:1899-1913`).
- **Ctor/destructor `where` clauses** beyond type/method ones (captured at `build.rs`) —
  same classifier applies if ever needed; v1 enforces only method-decl + type-decl clauses
  (the latter classify-only).

**Honesty note:** the *enforcement* half (diagnostics) is the entire new work. The *concrete
dispatch* half is **already done** (monomorphization), so v1's dispatch contribution is
purely the non-regression pin (CT-T0). Abstract dispatch (Appendix A) contributes **nothing**
to v1 and is documented only to prevent a future agent re-deriving the vtable.

## 6. Load-bearing risks + mitigations

1. **Ratchet false-positives on the four constraint-dense files (DOMINANT risk).**
   `clean == files.len()` (`corpus.rs:106-111`). A single over-eager diagnostic breaks the
   160/160 ratchet. The supported forms these files contain are **dense and mostly
   *supported-shaped***: `Generics.bf` alone has `where T : struct` (`:146`), `where T :
   interface` (`:158`), `where T : IDisposable` (`:101/113/119`), `where T : class`
   (`:217`), `where T : new, …` (`:171/178/186/194`); `Interfaces.bf` has `where T : IFaceA`
   (`:53`), `where T : IFaceB` (`:63`), `where T : IFaceD` (`:311`); `Generics2.bf` has a
   type-level `where T : IDisposable` (`:36`). The burden — zero diagnostics across all of
   these — is heavy.
   *Mitigation:* (i) the **per-file** ratchet keeps corlib interfaces (`IDisposable`,
   `IHashable`) unresolvable → skipped; (ii) the **any-base-unresolvable ⇒ skip**
   transitive rule (§3.2); (iii) the **overloaded-decl skip** ( `MethodA<T>`×4); (iv) the
   **deferral of `T : T2`** (the `[IgnoreErrors]` `MethodG` call); (v) the **deferral of
   type-decl instantiation enforcement** (`Generics2.bf:36`); (vi) **body-first
   classification** (operator clauses on `float`/`char8`/… skipped before the name is read).
   Pinned by CT-T1's `constraint_diags == 0` on all four files **and** the corpus ratchet.

2. **Pass placement (single seam in `analyze`).** Diagnostics need spans + name resolution
   (`DefGraph`) **and** method bodies (`files`) — only co-available in `analyze` after the
   graph is built, never on the `Builder` in `resolve_and_check`.
   *Mitigation:* the whole pass runs in `analyze` after `check_delete_flow`
   (`lib.rs:85`), signature `(files, &DefGraph, &Interner)` exactly like delete-flow
   (`ownership.rs:112`). No split across `resolve_and_check`; no `lower_program` signature
   change.

3. **Monomorph keying / fixpoint.** `index_generic_decls` keys by `(name, arity)`
   (`lower.rs:686`); enforcement adds **no new instantiation** (it only inspects shapes via
   AST walk; it never calls `record_inst` or registers a mono), so it cannot trip the
   fixpoint.

4. **SSA / dominance / ABI.** The feature emits **no IR** — zero SSA, zero dominance, zero
   ABI surface. `IrType: Copy`, `StructTable`-owns-its-data, sema⊥llvm all untouched.

5. **Termination.** Every transitive base/iface walk carries a `HashSet<TypeId>` visited
   guard (§3.2) and bails on revisit — required by the self-referential
   `Singleton<T> where T : Singleton<T>` (`Generics.bf:79`) and mutually-recursive bases.
   The corpus ratchet's whole point is that the passes terminate; an unguarded walk would
   hang the suite.

6. **The `Object` root premise (corrected).** `Object` **does exist** — `class Object`
   in `corlib-slice/Object.bf:8`. The earlier "there is no universal base" claim was
   factually wrong. The check is safe under the *per-file* ratchet because `corpus.rs:61`
   analyzes each file as a one-file program, so `Object` is **not co-resolvable** with
   `Generics.bf` (which uses `: Object` at `:178`) → that base resolves to nothing → **Skip**.
   *Mitigation:* `class`/`struct`/`new`/`: Base` checks are **structural** against
   `TypeKindD` / explicit-ctor presence / the `TypeDef.bases` chain — they **never**
   special-case `Object`. Whether `Object` is in scope only changes whether a `: Object`
   bound resolves (and thus is checked vs skipped); it never changes the *meaning* of the
   structural checks.

7. **Per-file vs multi-file configuration dependence.** The "0 diagnostics on the four files"
   proof holds for the **per-file** ratchet configuration; a driver build co-analyzing
   corlib-slice + feature-suite makes corlib interfaces (`IDisposable`, `IHashable`)
   in-program, turning `where T : IDisposable`/`where K : IHashable` into *supported* bounds
   whose instantiations CT-T3 would then validate (and could false-positive on, e.g. `int`
   against `K : IHashable` from `Method3`).
   *Mitigation:* the transitive **any-unresolvable ⇒ skip** rule handles most of it, and
   CT-T4 adds a **multi-file negative pin** (co-analyze corlib-slice + `Constraints.bf`,
   assert 0 constraint diagnostics) so this configuration is tested, not assumed.

8. **Simple-name collisions (same-file and cross-file).** Two in-program types can share a
   simple name. Same-file: `IFaceD<T>` (`Interfaces.bf:204`, arity 1) and `IFaceD`
   (`:280`, arity 0).
   *Mitigation:* the index is keyed by **`(Symbol, arity)`** (§2.2), so the two `IFaceD`s
   do **not** collide and a bare `where T : IFaceD` binds the arity-0 entry — exactly like
   `check_duplicate_types` (`resolve.rs:120`) and `index_generic_decls` (`lower.rs:686`).
   Residual cross-file/same-arity ambiguity is first-wins; a violation fires only on a
   **provable** mismatch, else **Skip** (qualified resolution is a precision follow-on).

9. **Memory-safety-under-guard.** The feature touches no alloc/delete/scope path and emits
   no heap operation; the Stomp guard and `ownership.rs` delete-flow are unaffected. Negative
   fixtures `delete` only concrete refs (or none).

10. **Comptime.** Constraints are not comptime (comptime is primitives-only); no interaction.
    The check runs in `analyze`, before any comptime emission round (`run_emission`,
    `run_corpus.rs:50`), so comptime never sees constraint diagnostics — and, per §3.5,
    `run_emission` would drop a round-0 one anyway.

## 7. Task breakdown

Each task is agent-assignable with a one-line seed and a concrete acceptance gate. Gates
that must stay green at **every** boundary: sema corpus 160/160 (`clean == files.len()`),
verify corpus 160/160 (`llvm_lowering_verifies_on_real_beef`), parser corpus, run-corpus
(authoritative value checks). `interface_constraint.bf -> 100` must stay green throughout.

**CT-T0 — Pin the concrete constrained-`T` dispatch as a non-regression.**
*Seed:* add/confirm `constraint_iface_use.bf` (or reuse `interface_constraint.bf`) plus
`constraint_class_bound.bf` and `constraint_struct_bound.bf` in run-corpus, proving the
already-working monomorphized concrete path. **No `new`-based program** (verified `new T()`
on a bare type-param does not lower, §4).
*Deps:* none. *Accept:* the three run-corpus programs pass their `// expect:` values;
`interface_constraint.bf -> 100`; all aggregate gates green. **Behavior-preserving.**

**CT-T1 — Skeleton (skip-all classifier) + the ratchet-safety pin (lands the guard FIRST).**
*Seed:* add the `constraints.rs` module + `check_generic_constraints(files, &graph,
&interner)` wired into `analyze` after `check_delete_flow` (`lib.rs:85`); build
`type_by_name_arity` from `graph.types`; **recognize** every constraint form (supported +
deferred) but emit **no** diagnostic yet (pure classification, body-first per §3.2). Also
land the parameterized `constraint_diags(root, name)` test helper (mirroring
`double_free_diags`, `delete_flow.rs:31`, but **root-parameterized** so it can read both
`tests/constraints/` and the `beef-tests/feature-suite/src/` ratchet files) and assert
`constraint_diags == 0` on `Constraints.bf`/`Generics.bf`/`Generics2.bf`/`Interfaces.bf`.
*Deps:* CT-T0. *Accept:* sema corpus 160/160 (the check is a no-op); a unit test asserts
the classifier labels `Constraints.bf`'s clauses *deferred* (operator/const/array/generic-
iface/primitive-name) and a synthetic `where T : IFace` *supported*; the four
`constraint_diags == 0` pins pass. **Behavior-preserving.** *(Landing the pin here, before
any diagnostic-emitting task, is deliberate: it guards CT-T2 and CT-T3 from the moment they
emit their first diagnostic, with a precise per-file failure signal.)*

**CT-T2 — Declaration-level enforcement (clause-internal kind contradiction only).**
*Seed:* in `check_generic_constraints`, emit a diagnostic for a parameter constrained both
`class` and `struct` across the same decl's clauses — strictly when both are bare keyword
paths; skip everything else. **No "non-generic-parameter name" check** (would fire on
`where float : operator …`, §3.2).
*Deps:* CT-T1. *Accept:* sema corpus 160/160 (CT-T1's four `== 0` pins still hold);
`violate_decl_contradiction.bf` diagnoses exactly once; `satisfied_no_diag.bf` → 0.
**Behavior-changing (diagnostics only).**

**CT-T3 — Method-call instantiation enforcement (the high-value `Use<int32>` check). RISKIEST.**
*Seed:* add the method-call instantiation collector in `check_generic_constraints` (re-walk
`files` bodies like `check_delete_flow`) recording `(decl_simple_name, arity,
[concrete_arg_type_names], span)` for `Name<Args>(…)` / `Recv.Name<Args>(…)` calls; add the
primitive-fact table (§2.2); validate each supported constraint via the transitive
implements/base walk **with a `HashSet<TypeId>` cycle guard**; **skip** any call whose
`(name, arity)` matches >1 generic decl (overloads), any unresolvable arg/constraint/base,
and all deferred forms; emit one diagnostic per provable violation. Type-position
instantiations are NOT walked.
*Deps:* CT-T2. *Accept:* `violate_iface.bf` (primitive `int32` arg), `violate_class_constraint.bf`,
`violate_struct_constraint.bf` each diagnose **exactly once** (via `constraint_diags`,
direct `analyze` — NOT run-corpus); `satisfied_no_diag.bf` → 0; **all four ratchet files →
0 constraint diagnostics** (incl. `Generics.bf`'s `MethodA`×4 overloads, the `[IgnoreErrors]`
`MethodG`, `IntPtrTest`'s `int*`, and `Constraints.bf`'s operator/const/array/generic-iface
clauses); sema corpus 160/160; verify 160/160. **Behavior-changing.**

**CT-T4 — Multi-file ratchet-safety pin (the configuration-dependence guard).**
*Seed:* add a test that co-analyzes `corlib-slice/*.bf` **with** `Constraints.bf` (and
optionally `Generics.bf`) in a single `analyze` call and asserts 0 constraint diagnostics —
covering the case where `IDisposable`/`IHashable` become in-program (§3.3/§6.7), which the
per-file ratchet cannot exercise. Wire the positive run-corpus programs and the negative
`tests/constraints/` fixtures into the suite.
*Deps:* CT-T3. *Accept:* the multi-file co-analysis yields 0 constraint diagnostics; the
three CT-T0 positives and the five negatives (`violate_iface`/`violate_class_constraint`/
`violate_struct_constraint`/`violate_decl_contradiction`/`satisfied_no_diag`) all assert
their exact counts; full sema + verify + run-corpus green. **Behavior-preserving (test-only).**

**CT-T5 — Journal + doc cross-link.**
*Seed:* add a numbered journal section (design + outcome) in the current journal; cross-link
this design doc; commit pairs with the entry (conventional style + Co-Authored-By trailer).
*Deps:* CT-T4. *Accept:* journal entry present; all gates green.

**Minimal-but-correct v1 = CT-T0, T1, T2, T3, T4, T5** — the concrete dispatch pinned, the
supported `where`-clauses (`IInterface`/`SomeClass`/`class`/`struct`/`new`) enforced as
conservative diagnostics behind the driver/AOT gate, the per-file **and** multi-file
ratchets held, all behind the green gates. CT-T3 is the critical-path, highest-risk node.

## Appendix A — Abstract-`T` interface dispatch (future, NOT v1, currently unreachable)

Documented to prevent a future agent re-deriving the mechanism; **explicitly excluded from
the v1 slice and currently without a reachable trigger**.

The genuinely-new case: a generic body that calls a constraint method on a `T` that is
**never** monomorphized to a concrete class. Today an unbound `T` lowers to `IrType::Ptr`
(`lower_ty_env` fallback, `:12384/12437`) **with no method-table header**, and
`record_method_inst` **refuses abstract type-args** (`targ_is_abstract`, `:1899-1913`, the
collection gate). NewBF always monomorphizes in v1, so **no lowering pass ever reaches a
constrained-`T` call with an abstract receiver** — this path has no input today.

If ever wanted, the landed interface-dispatch feature provides the slot mechanism: given
`where T : IFace`, an abstract `T`-typed receiver *could* be treated as `Ref(IFace_id)` and
dispatched through `emit_iface_dispatch(body_ptr, IFace_id, mname, args)` (`lower.rs:11047`)
— the slot is globally fixed for `(IFace, method)` via `iface_slot_base` (`:11063`). **But
this is not a one-line reroute**: `emit_iface_dispatch` calls `load_vtable_base(body_ptr)`
(`:11069`), which requires `body_ptr` to point at an object carrying a `%ClassVData` header
at offset 0 — an abstract-`T` `Ptr` has no such header, so the path would also need (a) a
**collection-gate change** to admit an `IFace`-bound abstract `T` (relax `targ_is_abstract`),
(b) lowering `T`-typed receivers to `Ref(IFace_id)` rather than the `Ptr` fallback, and (c)
a guarantee that the *runtime value* bound to the abstract `T` actually arrives with the
interface vtable laid out (else feeding a header-less pointer to `load_vtable_base` is a
silent miscompile, not a clean dispatch). Because the path has no reachable trigger in v1,
it is **cut from the task plan**, not merely deferred.
