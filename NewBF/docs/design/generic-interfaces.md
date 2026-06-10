# Generic-Interface Monomorphization + Dynamic Dispatch (the wave-4 foundation)

> **Status: DESIGN (wave 4, feature #1 — the foundation).** This feature lifts the
> deliberate generic-interface exclusion that landed with the non-generic itable
> feature (journal §112, `docs/design/itables.md` §6/§10) so that a class
> implementing `IFaceD<int16>` gets a real per-`(class, IFaceD$i16)` itable, an
> `IFaceD<int16>`-typed value dispatches dynamically through it, and a
> generic-interface constraint (`T : IFaceD<int16>`) can be classified and
> enforced. It is the foundation the **deferred** halves of the other wave-4
> generic cases (iterators-lazy's interface-typed `IEnumerable<T>`,
> generic-constraints' `T : IEnumerator<TElement>`) will build on. Every
> load-bearing claim is anchored to a re-verified `file:line` in
> `e:/NewBF/NewBF/src/newbf-sema/src/`.

This document is modeled on `docs/design/itables.md` (the non-generic dispatch it
extends) and `docs/design/generic-constraints.md` (the constraint pass it
unblocks); their rigor and task structure are mirrored here.

> **Revision note (hardening pass).** Three adversarial reviews (correctness /
> integration / planning) converged on the same load-bearing defects, all now
> fixed below: (1) the mono interface→interface base link routes into the WRONG
> map — `collect_iface_bases` builds its own `iface_links` from
> `collect_iface_links_type` (gated `generic_params.is_empty()`, `lower.rs:1615`),
> a SEPARATE object from `fill_iface_members`'s local `bases` map, so the
> inherit example silently has no `IA$i16` slot (now Seam C′/§3.4); (2) the
> existing verify-corpus classes that implement generic interfaces
> (`EnumeratorTest : IEnumerator<int32>` Loops.bf:17, `IndexTest`/
> `IndexTestExplicit : IIndexable<float>` Indexers.bf:96/112, `ClassD`/`ClassE`
> Interfaces.bf:229/247) will hit `resolve_itable_impl`'s `debug_assert!(false)`
> (`lower.rs:1337`) the moment their interfaces monomorphize — a LOUD ratchet
> panic, not a silent miscompile (now T-PRE/§3.9 + R1/R7 reframed); (3) `Interfaces.bf`
> is a **verify-only** fixture — its `Test.Assert(v == 123)` is NEVER JIT-run, so
> the only behavioral proof is the new `generic_iface_*.bf` run-corpus programs
> (R7 reworded). Plus: the generic `extension IFaceD<T>` (`GetVal2`,
> Interfaces.bf:221) is now explicitly DEFERRED (§5); the `is`/`as` and
> distinct-return-type examples needed code changes / a rewrite (§3.6/§4);
> constraint enforcement (Seam G) is **independent** of Seams A–F and arg-blind by
> arity (§3.7); and several `file:line` anchors were corrected
> (`targ_is_abstract` 1973 not 1866, the seam routing).

---

## 1. Overview + the v1 capability

**v1 capability (one paragraph).** Monomorphize a **generic interface** the same
way a generic struct/class is monomorphized today: `IFaceD<int16>` becomes a real
registered type `IFaceD$i16` with a `StructId`, `StructKind::Interface`, and an
env-filled `imethods` slot table (its `T`-bearing method signatures resolved with
`T → int16`). A concrete class implementing `class ClassD : IFaceD<int16>` then
routes that mono interface id into `iface_bases[ClassD]`, and the **already-working,
generic-agnostic** `apply_itables` (`lower.rs:1238`) composes the mono interface's
methods into `ClassD`'s vtable at a globally-fixed `iface_slot_base`, exactly as it
does for a non-generic interface. An `IFaceD<int16>`-typed value (`IFaceD<int16> ifd
= cd; ifd.GetVal()`) resolves to `Ref(IFaceD$i16)`, and the **already-working**
`emit_iface_dispatch` (`lower.rs:11878`) dispatches through the itable slot —
returning the real value, not the current `undef` fallback. The deferred
generic-interface constraint (`ConstraintKind::GenericBound`, `constraints.rs:1091`)
is promoted to an enforced kind that validates an arg implements **some**
instantiation of the interface via the existing transitive base walk. The core lift
is purely **type-layout + a new mono-request discovery seam + an env-driven slot
fill**: the dispatch IR, the vtable emission, and the itable composition are
untouched (same zero-`newbf-llvm`-change property the non-generic itable had —
journal §112 "R8-safe").

**The single deepest piece of new code** is the **env-driven `imethods` fill at a
minted mono interface id** — a mono interface has no standalone AST decl, so its
slot signatures must be built from the *template* decl + the mono env (resolving
`T → int16`), with the leading `this` being `Ref(IFaceD$i16)` not `Ref(IFaceD)`,
**and** its interface→interface base links (`IB$i16 → IA$i16`) must be routed into
the class-side flatten map (`collect_iface_bases`'s `iface_links`), not just the
`imethods`-flatten map. Everything downstream of a populated `imethods[IFaceD$i16]`
+ `iface_bases[ClassD].contains(IFaceD$i16)` (with the inherited mono bases present)
is **already built**.

---

## 2. Representation / ABI / IR changes (the sema⊥llvm contract)

### 2.1 No new `IrType`, no new IR instruction, no `newbf-llvm` change

A monomorphized generic interface is an **ordinary registered interface id** —
identical in every respect to a non-generic interface except its name is mangled
(`IFaceD$i16`) and its members are filled from a template+env rather than a
standalone decl. The IR-level artifacts already exist:

- `IrType` stays `Copy` (`newbf-ir/src/ty.rs`). `IFaceD<int16>`-typed values are
  `IrType::Ref(IFaceD$i16_id)` — a plain `ptr` to some object body, carrying the
  mono interface's nominal id. `ty_of` already maps
  `StructKind::Ref | StructKind::Interface => IrType::Ref(id)` (`lower.rs:649-654`),
  so a registered mono interface lowers to `Ref(mono_id)` with **zero new code**.
- `StructKind::Interface` already exists (`lower.rs:123`); `struct_kind` already
  returns `Some(StructKind::Interface)` for `TypeKind::Interface` (`lower.rs:123`),
  and `record_inst` already mints the mono with `kind = struct_kind(decl)
  .unwrap_or(StructKind::Value)` (`lower.rs:1781-1782`). A registered interface mono
  is therefore born with `kinds[mono] = Interface` for free.
- `register_mono` (`lower.rs:907`) already pushes the `iface_bases`/`imethods`/
  `idefaults` parallel-vec slots in lockstep (`lower.rs:924-926`). A mono interface
  id is born with **empty-but-present** itable vecs ready to fill — no new
  id-minting site, no lockstep audit.
- Dispatch (`emit_iface_dispatch`, `lower.rs:11878`), itable composition
  (`apply_itables`, `lower.rs:1238`), `is`/`as` membership (`type_test`,
  `lower.rs:11480`, the `iface_bases[c].contains(&tid)` test at `lower.rs:11489`),
  and the vtable globals (`emit_classvdata` lowering empty entries to `const_null`)
  are **entirely `StructId`-keyed and generic-agnostic** — they index ids, never
  AST decls.

**`newbf-llvm` needs zero changes** — the same property the landed non-generic
itable feature had. No new heap op, no float constant (the JIT FP-constant-pool
MEMORY caveat is N/A), no new IR instruction.

> **What is NOT zero-change in sema.** §2.1 establishes the IR-level surface is
> untouched. But the *sema* lift is more than a guard flip: the new env-driven
> `imethods` fill (Seam C, §3.3), the mono interface→interface link routing (Seam
> C′, §3.4), the `td.bases` collection walk (Seam B, §3.2), the `type_id_of`
> generic-RHS arm for `is`/`as` (Seam F′, §3.6), and the constraint promotion
> (Seam G, §3.7) are all genuine code. The §5 deferred list is honest about the
> rest. **Confirm-only seams are E (§3.5) and the base-routing guard (R6) — and
> they hold only AFTER T-PRE proves the existing corpus classes resolve a complete
> itable.**

### 2.2 The sema⊥llvm contract (what sema emits by name)

The HARD INVARIANT (sema must not depend on `newbf-llvm`; they agree via the IR
contract + named symbols) is preserved trivially: a mono interface's dispatch is the
same `load_vtable_base → elem_addr(slot) → call_indirect` shape the non-generic
itable already emits (`lower.rs:11900-11914`), against a vtable slot whose impl
symbol sema itself mangled (`{ClassD.prefix}GetVal`, an ordinary `MethodSig
.full_name`). The new symbol namespace is the **mangled mono interface name**
`IFaceD$i16` (`mangle_generic` + `type_codes`, `lower.rs:13126` / `lower.rs:6157`;
`int16` → code `i16`, so `IFaceD$i16`), which `ty_of` resolves to `Ref(mono_id)`
(`lower.rs:646-654`). No new mangling scheme — the standard generic-type mangle.

### 2.3 ABI: the mono-interface itable is byte-identical in shape to the non-generic one

`IFaceD$i16`'s slot table (`imethods[IFaceD$i16]`) is `[("GetVal", sig)]` where
`sig.params = [Ref(IFaceD$i16)]` (the `this`), `sig.ret = i16` (the env-resolved
`T`) — **for an inline v1 fixture that declares only the abstract `T GetVal()`**.
(The *feature-suite* `IFaceD<T>` is messier: it also carries a method-generic `T
Add<T2>`, statics, and a separate generic `extension … { T GetVal2() }` — all of
which are dropped or deferred; see §3.9/§5. The dump-ir gate in T2 therefore asserts
the slot shape against a **clean inline fixture**, not against feature-suite
`IFaceD<int16>`.) The itable ABI gate `itable_abi_matches` (`lower.rs:1353`) already
asserts the impl's non-pointer param/return IR types equal the slot sig's (pointers
are ABI-identical; the leading `this` differs in nominal id only — `Ref(ClassD)`
impl vs `Ref(IFaceD$i16)` slot). `ClassD.GetVal`'s `ret = i16` matches
`IFaceD$i16.GetVal`'s slot `ret = i16` exactly, so the slot resolves to the real
symbol, not a null placeholder. **This is the correctness keystone**: a wrong `this`
id or an unresolved `T` (leaving `ret = Ptr` or `Ref(template_id)`) desyncs the ABI
gate, and `resolve_itable_impl` falls through to its terminal
`debug_assert!(false, …)` (`lower.rs:1337`) → a LOUD panic in the debug verify
corpus (Risk R1).

### 2.4 Slot-layout stability — one mono id shared by all implementers

`apply_itables` assigns a **global** `iface_slot_base[I]` so the slot for `(I,
method k)` is identical in *every* implementer (`lower.rs:1249-1250`,
`lower.rs:1275`). For monos this forces: **`IFaceD$i16` must be ONE id** shared by
`ClassD` and `ClassE` (both `: IFaceD<int16>`, `Interfaces.bf:229,247`). The
`seen`-set dedup in `record_inst` (`lower.rs:1779`, `if !seen.iter().any(|s| s ==
&mangled)`) + the `(name, args.len())` keying (`lower.rs:1759`) already guarantees
one mono id per `(name, args)`. The new mono-request discovery (§3.2) routes the
**same** `IFaceD$i16` id into both `iface_bases[ClassD]` and `iface_bases[ClassE]`
— because both classes' `: IFaceD<int16>` bases mangle to the same `IFaceD$i16`
and `collect_iface_bases_type` resolves it through `lower_ty_env → ty_of →
Ref(mono_id)` (`lower.rs:1658`). An off-by-one or a duplicate mono id is caught
loud by the bounds keystone `debug_assert!(slot_base >= vimpls[i].len())`
(`lower.rs:1268`) **in a debug/assertions-on profile**; in a release build it
silently null-pads/null-slots, so the run-corpus value check (123, not garbage) is
the only release-active net (Risk R2; R10 in §6 notes the debug-gating).

---

## 3. Concrete changes (sema / parser / llvm / runtime), with seams

In dependency order. **All work is in `e:/NewBF/NewBF/src/newbf-sema/src/lower.rs`**
(plus `constraints.rs` for the independent constraint lift, Seam G). Parser,
`newbf-ir`, `newbf-llvm`, `newbf-runtime`: **no change**.

### 3.0 T-PRE — triage the EXISTING corpus classes that implement generic interfaces (the ratchet keystone)

Before any seam lands, enumerate every verify-corpus class whose `: I<…>` base is a
**generic interface**. Today these rely on the `Ptr`+undef fallback (the interface
never registers, so `iface_bases[class]` never gets the entry, so `apply_itables`
never composes a slot). Once Seams A–C+C′ monomorphize their interfaces, each class
gets a real `iface_bases` entry that `apply_itables` MUST resolve to an
ABI-matching symbol or `resolve_itable_impl` hits its terminal
`debug_assert!(false, …)` (`lower.rs:1337`) — a panic in the **debug** verify
corpus (`corpus.rs:134/170` run `analyze` + `lower_program` + `verify_module` over
every `feature-suite/src` + `corlib-slice` file standalone, assertions ON). The
confirmed implementers (re-verified by grep):

- `class ClassD : IFaceD<int16>`, `class ClassE : IFaceD<int16>`
  (Interfaces.bf:229/247) — both declare `int16 GetVal()`, so the abstract `GetVal`
  slot resolves. **But** the same file forces three deferred-feature interactions on
  the *same* interface (see §3.9): `ifd.GetVal2()` (generic `extension`),
  `IDAdd`/`Add<T2>` (method-generic), `SGet`/`SMethod` (static-virtual).
- `class IndexTest : IIndexable<float>` (Indexers.bf:96) and
  `class IndexTestExplicit : IIndexable<float>` (Indexers.bf:112, an **explicit**
  `float IIndexable<float>.this[int]` impl). `IIndexable<T>`'s sole member is a
  property/indexer `T this[int] { get; set; }` (Indexers.bf:6), which
  `collect_iface_own_type` **does not** turn into a slot (it walks `Member::Method`
  only, `lower.rs:1471`), so `imethods[IIndexable$float]` is **empty** → no slot to
  resolve → no panic. This is SAFE for the ratchet but means generic-interface
  **properties/indexers** silently do not dispatch in v1 (deferred, §5). The
  explicit-impl key path (`explicit_impls` keyed on the mono iface id) is likewise
  untested → deferred.
- `class EnumeratorTest : IEnumerator<int32>, IDisposable` (Loops.bf:17). `IDisposable`
  is non-generic (already handled). `IEnumerator<int32>` is the risk: its method
  slots (`MoveNext`/`Current`/…) must all resolve on `EnumeratorTest` or panic.

**Acceptance for T-PRE:** run the verify corpus after each of T0/T1/T2 and confirm
NO `resolve_itable_impl` panic for any of the above. If any class has an
unresolvable or ABI-mismatched slot, EITHER (a) the slot is property/indexer-shaped
(empty `imethods`, safe) — document it, OR (b) the class is a genuine gap — in which
case narrow the v1 trigger (e.g. defer that interface shape in §5) so the corpus
stays panic-free. **This is the real R7** — `Interfaces.bf` alone was never the
gate. Treat T-PRE as the gating analysis that bounds T0–T2's scope.

### 3.1 Seam A — register generic interfaces as monomorphizable (the first domino)

`index_generic_decls` (`lower.rs:716`) excludes interfaces at the guard
`td.kind != TypeKind::Interface` (`lower.rs:737`). The doc comment at
`lower.rs:730-734` states the exclusion contract explicitly. **Lift it** so a
generic interface enters the `(name, arity)` `GenericDecls` map (`lower.rs:747`):

```rust
// lower.rs:735-741, CURRENT:
Item::Type(td)
    if !td.generic_params.is_empty()
        && td.kind != TypeKind::Interface          // ← REMOVE this conjunct
        && (struct_kind(td).is_some()
            || (td.kind == TypeKind::Enum && enum_has_payload(td) && enum_is_layoutable(td))) =>
```

`struct_kind(td)` already returns `Some(StructKind::Interface)` for an interface
(`lower.rs:123`), so dropping the `td.kind != Interface` conjunct lets the existing
`struct_kind(td).is_some()` admit a generic interface. **Nothing downstream fires
without this** — until it is in `GenericDecls`, `record_inst` bails at its
`else { return; }` (`lower.rs:1759-1761`) and `lower_ty_env`'s generic arm falls to
`IrType::Ptr` (`lower.rs:13218-13226`).

**`record_inst` is already correct once Seam A lands.** It resolves the decl by
`(name, args.len())` (`lower.rs:1759`), mints the mono via
`let kind = struct_kind(decl).unwrap_or(StructKind::Value); … register_mono(t,
&mangled, kind)` (`lower.rs:1781-1782`) — which sets `kinds[mono] = Interface` —
and builds the `inst_env` binding `T → int16` (`lower.rs:1783-1788`). The mono
interface id exists with the right kind and an empty itable; only its `imethods`
fill and base routing remain.

### 3.2 Seam B — discover interface-base mono requests (NEW seam, currently absent)

`collect_insts_type` (`lower.rs:2045-2107`) walks `Member::Field` / `Member::Method`
/ `Member::Constructor` / `Member::Nested` (`lower.rs:2056-2106`) but **never
`td.bases`** (re-verified: the `match m` over `td.members` has no base arm, and no
`use_in_type` is called over `td.bases`). So `class ClassD : IFaceD<int16>` does
**not** request the `IFaceD$i16` mono even with Seam A lifted — `record_inst` is
never called for it.

**Add a single base-list walk to `collect_insts_type`**, threading the visitor state
already in scope, so each type-arg-bearing base is fed to `use_in_type` (which routes
to `record_inst`):

```rust
// lower.rs:2056, add BEFORE the `for m in &td.members` loop:
for b in &td.bases {
    use_in_type(b, src, generics, gmethods, t, seen, monos, env);
}
```

`use_in_type` (`lower.rs:1708`) already does the `AstType::Path { segments } where
!args.is_empty()` → `record_inst` routing, so `class ClassD : IFaceD<int16>`
requests `IFaceD$i16` in **pass 1** (step 3, `lower.rs:497-508`).

> **ONE edit, not two (correction).** Earlier drafts told the agent to also edit
> "the non-generic-type arm of `collect_insts_items`". That is wrong:
> `collect_insts_items`'s `Item::Type` arm (`lower.rs:2032-2038`) has NO
> member/base logic — it unconditionally delegates **every** type (generic or not)
> to `collect_insts_type` (`lower.rs:2037`). The single edit in
> `collect_insts_type` therefore covers BOTH the generic-template recursion AND the
> common non-generic `class ClassD : IFaceD<int16>` case. Do not add a second edit.

This MUST be pass 1, not pass 2: the `monos2.is_empty()` fixpoint assert
(`lower.rs:571-575`) trips if an interface-base mono is discovered only in the
second collection pass. Bases are a purely syntactic feature (no field-receiver
dependency), so pass-1 discovery is complete (Risk R3).

### 3.3 Seam C — fill the mono interface's `imethods`/`idefaults` at the minted id (the DEEPEST change)

`fill_iface_members` (`lower.rs:1405`) calls `collect_iface_own` →
`collect_iface_own_type` (`lower.rs:1445`), which is **AST-decl-driven over `files`**
and gated on `td.generic_params.is_empty()` (`lower.rs:1453`) AND keyed by
`t.by_name.get(td.name.text(src))` (`lower.rs:1454`). A mono interface `IFaceD$i16`
has **no standalone AST decl** — it is a minted id whose template is `IFaceD<T>`, and
its id is the *mono* id, not `by_name[template-name]`. **The gated, by-name body
cannot be called as-is for a mono.**

**Extract the per-method slot-building body into a helper** callable with
`(mono_id, template_decl, mono_env)` that bypasses the `generic_params.is_empty()`
gate and the `by_name` lookup. The four substituting call sites inside the current
body (`lower.rs:1493/1495/1500/1516`) are already env-aware — they just pass `&[]`
today; the helper threads the mono env instead:

- **Reuse the exact filtering `collect_iface_own_type` already does**
  (`lower.rs:1483-1488`): drop `static` and method-generic interface methods
  (`is_static || !generic_params.is_empty()`) so they never consume a slot index —
  identical to the non-generic path. For the feature-suite `IFaceD<T>` this drops
  `static T SMethod` (line 213), `static T SMethod2` (line 215), and the
  method-generic `T Add<T2>` (line 208). It does **not** see the generic
  `extension`'s `GetVal2` at all (a separate `Item::Type`, §3.9) — leaving exactly
  `[("GetVal", sig)]`.
- **Resolve each slot sig's `ret`/`params` through the mono env**: where the
  non-generic path calls `lower_value_ty(return_ty, src, t, &[])` (`lower.rs:1516`),
  `param_ir_ty(p, src, t, &[])` (`lower.rs:1495`), and `pointer_elem_env(…, &[])`
  (`lower.rs:1500`) with an **empty** env, the helper passes the **mono `env`** so
  `T GetVal()` resolves `ret = i16`. This is the core correctness point (§2.3).
- **The leading `this` is `Ref(mono_id)`** (`lower.rs:1493`, `vec![IrType::Ref(id)]`
  where `id` is the mono id), not `Ref(template_id)`. A wrong `this` id desyncs the
  ABI gate (Risk R1).
- **The default-body symbol** (`idefaults[mono_id][k]`) is
  `{IFaceD$i16.prefix}{Method}` (`lower.rs:1507`, `format!("{}{}",
  t.prefixes[id.0], nm)` with the mono prefix) — so it reconciles with the symbol
  Seam F (§3.5) emits for a default-bodied generic-interface method at the mono id.
  **Note:** the feature-suite `IFaceD<T>` has NO concrete-signature default-bodied
  interface method, so this path is exercised only by the dedicated T4 fixture (§3.5,
  §5) — without it, Seam F is dead code.

**Wiring.** Extend `fill_iface_members` (`lower.rs:1405`): after the AST walk
(`lower.rs:1410-1412`) populates the local `own`/`bases` maps, iterate `t.monos`, and
for each entry whose template decl `kind == TypeKind::Interface`, run the helper to
produce that mono's `own` slots and **merge into the same `own` map** keyed by the
mono id, BEFORE the single `compose_iface_members` flatten loop
(`lower.rs:1416-1418`). By step 4e (`fill_iface_members` at `lower.rs:586`), every
interface mono id exists (minted in pass-1 `record_inst` via Seam B), so iterating
`t.monos` finds them. This is the **ordering keystone (R3)**: `fill_iface_members`
runs at step 4e, AFTER the step-4 mono registration (`lower.rs:523-528`) and BEFORE
`apply_inheritance`/`apply_vtables`/`apply_itables` (`lower.rs:590-600`).

### 3.4 Seam C′ — route the mono interface→interface base links into the CLASS-side flatten (the inherit-chain fix)

**This is the load-bearing fix all three reviews flagged.** For `interface IB<T> :
IA<T>` and `class C : IB<int16>`, the class-side flatten that builds
`iface_bases[C]` is `collect_iface_bases_type` (`lower.rs:1649`), which calls
`add_iface_flat(bid, links, &mut flat)` (`lower.rs:1662`). That `links` map is built
by `collect_iface_links` → `collect_iface_links_type` (`lower.rs:1601/1613`), gated
`td.generic_params.is_empty()` (`lower.rs:1615`) and resolved with the **empty env**
(`lower.rs:1620`). The mono link `IB$i16 → [IA$i16]` is therefore **never** in that
`iface_links` map. The `bases` map populated by Seam C inside `fill_iface_members`
(`lower.rs:1409`) is a **completely separate object** — it feeds only
`compose_iface_members` for `imethods` flattening and is thrown away; it never
reaches `collect_iface_bases`.

Result without this seam: `iface_bases[C] = [IB$i16]` *without* `IA$i16`, so
`apply_itables` never grows C's vtable to cover `iface_slot_base[IA$i16]`.
Dispatching an `IA<int16>` method through an `IB<int16>` value reads an
unwritten/OOB slot → null → segfault (or a debug-assert panic at composition).

**Fix:** inside `collect_iface_bases` (`lower.rs:1588`), after building `iface_links`
from the AST (`lower.rs:1592-1594`), iterate `t.monos` and for each interface-kind
mono insert `iface_links[mono_id] = [resolved mono base ids]`, resolving each
`b in template_decl.bases` through `lower_ty_env(b, src, t, mono_env)` →
`Ref(IA$i16)` (the base's `<T>` arg substituted via the mono env). Then the existing
`add_iface_flat` (`lower.rs:1677`) transitively pulls `IA$i16` into
`iface_bases[C]`. (v1 scopes this to single-type-param `IB<T> : IA<T>`; the gnarly
cases are §5.)

> **Why two link insertions, not one.** Seam C populates the `imethods`-flatten
> `bases` map so `IB$i16`'s slot table *includes* `IA$i16`'s methods. Seam C′
> populates the *class-routing* `iface_links` map so a class implementing `IB$i16`
> *also enumerates* `IA$i16` in its `iface_bases` (and thus `apply_itables`
> composes the `IA$i16` block). Both are required; they are different maps consumed
> by different passes. The `generic_iface_inherit.bf` dump-ir gate must assert
> `iface_bases[C].contains(&IA$i16)`, not just `IB$i16`.

`collect_iface_links_type` itself **stays non-generic-only** — its
`generic_params.is_empty()` gate is correct (a generic-interface *template* has no
concrete base to link; the mono does, and Seam C′ inserts it directly into the
`iface_links` map rather than through `collect_iface_links_type`).

### 3.5 Seam D + E + F — route into `iface_bases`, compose, dispatch, default bodies

**Seam D — `iface_bases[class]` routing (no code change beyond C′).**
`collect_iface_bases_type` (`lower.rs:1649`) resolves each class base via
`lower_ty_env(b, src, t, &[])` (`lower.rs:1658`) and routes `Ref`-kind interface
bases into `iface_bases[id]` (`lower.rs:1656-1665`). For a **concrete** class
`ClassD : IFaceD<int16>`, the base arg `int16` is concrete, so the empty env
resolves `IFaceD<int16> → Ref(IFaceD$i16)` correctly once Seam A+B register it, and
`is_interface(t, bid)` (`lower.rs:1387`, `kinds[mono] = Interface`) is true — the
branch matches with **no code change**. A **generic** class implementing a generic
interface (`class Foo<U> : IFaceD<U>`) would need the class's own monomorph env to
resolve `U`; that is deferred (§5).

**Seam E — `apply_itables` + `emit_iface_dispatch` (confirm-only, post-T-PRE).**
`apply_itables` (`lower.rs:1238`) is **entirely id-keyed**: `N = max over ALL ids of
vimpls[c].len()` (`lower.rs:1242`, monos included), walks interfaces in `StructId`
order assigning `iface_slot_base` (`lower.rs:1246-1252`), and composes each class's
`iface_bases` via `resolve_itable_impl` (`lower.rs:1277/1295`). The bounds keystone
`debug_assert!(slot_base >= vimpls[i].len())` (`lower.rs:1268`) still holds because
`N` is over all ids. `emit_iface_dispatch` (`lower.rs:11878`) is id-keyed:
`imethods[iface_id].position(name)` → `iface_slot_base[iface_id] + midx` →
`load_vtable_base → elem_addr → call_indirect` (`lower.rs:11890-11914`). The dispatch
branch at `lower.rs:12058-12063` fires when `kinds[owner_id] == Interface`. **No
change** — provided T-PRE has proven every existing corpus class resolves a complete
itable. The sibling-dispatch inside a default body (`lower.rs:8406-8414`) is likewise
id-keyed.

**Seam F — emit default-bodied generic-interface methods at the mono id.** A
**concrete-signature** default (bodied) generic-interface method (e.g. an inline
fixture's `int32 Twice() { return 0; }` in `IFaceX<T>`) must be emitted as a real
free function `{IFaceX$i16.prefix}Twice` with `this : Ref(IFaceX$i16)` and `T`
resolved, so the slot symbol Seam C set (`idefaults[IFaceX$i16][k]`) resolves to a
real function. The template is the existing type-mono emit loop (`lower.rs:5682-5692`):
for each `(id, name, env)` in `structs.monos`, re-find the decl by `(name, env.len())`
and call `lower_type_at(decl, Some(id), &prefix, env, …)`, which already emits a
generic type's instance methods with `this_ty = Ref(mono_id)` and `T` resolved via
`lower_ty_env`. Confirm `lower_type_at` emits **default interface methods** (bodied
members of an interface decl) at the mono id and does not skip them on a kind check.
**v1 status:** there is NO concrete-signature default-bodied method in any v1
fixture EXCEPT a dedicated one added in T4 (`IFaceX<T> { T Get(); int32 Twice()
{ return 0; } }`). Without that fixture, Seam F is untested — so it ships ONLY with
its fixture, or it is deferred (§5). It is listed in-scope here **because** T4 adds
the fixture.

### 3.6 Seam F′ — `is`/`as` against a generic-interface RHS (NEW code, not confirm-only)

`obj is IFaceD<int16>` / `obj as IFaceD<int16>` lower through `lower_is`/`lower_as`
(`lower.rs:11531/11545`), which resolve the RHS type via `type_id_of`
(`lower.rs:11521`). `type_id_of` currently handles only `Expr::Ident` and
`Expr::Paren` (`lower.rs:11522-11526`) → returns `None` for a generic RHS, which
parses as `Expr::Generic { base, args: Vec<Type> }` (`ast.rs:307`). So both `is` and
`as` fall straight to `false`/`null` — example #6 cannot work as a confirm-only task.

**Fix (new code):** add an `Expr::Generic { base, args, .. }` arm to `type_id_of`
that, when `base` is an `Expr::Ident`, lowers each `Type` arg via `lower_ty_env`,
mangles via `mangle_generic` (`lower.rs:13126`), and looks up `by_name` for the mono
id (returning `None` if unregistered). Once it returns the mono `StructId`, the
downstream `type_test` (`lower.rs:11480`) membership test
(`iface_bases[c].contains(&tid)`, `lower.rs:11489`) is **id-keyed and unchanged** —
it already handles an interface `tid`. The source value being itself
`IFaceD<int16>`-typed is fine: `type_test` reads `$header` via a raw offset-0 GEP
(`lower.rs:11498-11503`) precisely so an interface-typed source works.

### 3.7 Seam G — generic-interface constraint classification + enforcement (`constraints.rs`, INDEPENDENT of A–F)

**Seam G does NOT depend on Seams A–F.** The constraints pass uses a **separate
def-graph `TypeIndex`** (`constraints.rs:239`), built from `DefGraph` over
`graph.types`, keying every type by `(name, arity)` — so `IFaceD<T>` is already
keyed `("IFaceD", 1)` with `kind = Interface` **today**, with zero relation to
lower.rs's `StructTable` monomorphization. `transitive_reaches`
(`constraints.rs:954`) → `resolve_base` (`constraints.rs:981`) already resolves a
`class : IFaceD<int16>` base by `lookup(name, last.args.len())` (`constraints.rs:988`).
So Seam G can land **first or in parallel**, off the T0–T3 critical path.

`constraints.rs` classifies `T : IFaceD<int16>` as
`ConstraintKind::GenericBound(name)` (`classify_constraint`, the
`segments.len() > 1 || !last.args.is_empty()` arm at `constraints.rs:1090-1091`),
which lumps qualified paths AND generic bounds and is recognized-and-skipped.
Promote the generic-**interface** sub-case to enforced:

- **Classification** (`classify_constraint`, `constraints.rs:1057`): in the
  `segments.len() > 1 || !last.args.is_empty()` arm, add an arity-keyed kind check
  BEFORE returning `GenericBound`. There is **no existing helper** for this:
  `classify_named_bound` (`constraints.rs:1134`) reads `kind_by_name_arity` only at
  **arity 0** (`constraints.rs:1135`). Add the arity-aware path: resolve
  `index.lookup(name, last.args.len())` (`constraints.rs:289`), then check
  `index.kind_by_name_arity_of(id)` (`constraints.rs:269`) is `Interface`. If so,
  return a new `ConstraintKind::GenericInterface(String, u32)` (simple name +
  arity); otherwise keep `GenericBound` (the still-deferred generic-**base-class**
  bound). This split is load-bearing because the regression file declares BOTH a
  generic `interface IFaceD<T>` (arity 1, line 204) and a non-generic `interface
  IFaceD` (arity 0, line 280), with constraints `where T : IFaceD<int16>` (arity 1,
  lines 265/270/275) AND `where T : IFaceD` (arity 0, lines 311/316) — a
  wrong-arity lookup confuses the two.

- **Enforcement** (`check_one`, `constraints.rs:838`): add a `GenericInterface(name,
  arity)` arm. **Do NOT mirror the existing `Interface` arm verbatim** — that arm
  calls `lookup_arity0(iface)` (`constraints.rs:883`), which would resolve an
  arity-1 generic interface to `None` and always `Skip` (a silent no-op). Use the
  arity-aware `index.lookup(name, arity)` (`constraints.rs:289`) for the target,
  then `transitive_reaches(arg_id, target)` (`constraints.rs:954`). The base walk's
  `resolve_base` already keys by `(name, arity)` (`constraints.rs:981-988`), so a
  `class : IFaceD<int16>` base (arity 1) resolves to the same arity-1 target. The
  `Satisfied`/`Violated`/`Skip` tree (`constraints.rs:992-1003`) is reused.

> **HONEST scope — arity-level, not arg-level (Risk R5).** `resolve_base` and
> `lookup(name, arity)` **ignore the type arguments** — both `IFaceD<int16>` and
> `IFaceD<int32>` bases resolve to the SAME arity-1 `IFaceD` target. So the v1
> enforcement is: "the arg implements **some** instantiation of `IFaceD`." It does
> NOT catch `class C : IFaceD<int32>` satisfying `where T : IFaceD<int16>` (a false
> negative on the wrong instantiation). Tightening to arg-level would require
> threading the def-graph's generic-arg representation (a mangled-arg compare) that
> `constraints.rs` does not carry today — **explicitly deferred** (§5). The v1
> positive (`generic_iface_constraint_ok.bf`) and a non-implementer negative
> fixture gate the arity-level enforcement; do NOT claim arg-level soundness.

**Constraint *dispatch* already works by erasure.** `val.GetVal()` under `T :
IFaceD<int16>` monomorphizes `T → ClassD` and resolves statically on `ClassD`
(`generic-constraints.md` §1/§3.4) — it does **not** need this feature. Only
*abstract-T* dispatch through a mono interface would, and that is unreachable in v1
(`targ_is_abstract` refuses abstract type-args, `lower.rs:1973`) — deferred.

### 3.8 llvm + runtime

**`newbf-llvm`:** no change (no new instruction; vtable globals, `call_indirect`,
`elem_addr`, `load` all exist; longer vtables are plain constant arrays with
`const_null` placeholders). **`newbf-runtime`:** no change (the mono interface
allocates nothing; dispatch is a `call_indirect` through an existing vtable global).
**Parser / `newbf-ir`:** no change.

### 3.9 The feature-suite `IFaceD<T>` interaction surface (why T-PRE bounds scope)

`Interfaces.bf`'s `TestDefaults` (lines 458-487) lowers ONE method that, once
`IFaceD<int16>` flips from `Ptr` to `Ref(IFaceD$i16)`, re-resolves several
**deferred-feature** paths on the same interface. Each must stay **verify-clean**
(not necessarily dispatch correctly) after the flip:

- `ifd.GetVal2()` (line 467) — a `T GetVal2()` in a generic `extension IFaceD<T>`
  (lines 221-227). Generic extensions are NOT merged into an interface's method
  table: `fill_extension_at` is skipped for `StructKind::Interface`
  (`lower.rs:3563`), and a *generic* extension is skipped entirely
  (`lower.rs:3559`, the `generic_params.is_empty()` gate; the doc-comment at
  `lower.rs:3554-3555` says "a generic extension follows the monomorph path and is
  not handled here"). So `GetVal2` is **never** an `imethods` slot on `IFaceD$i16`.
  After the flip, `ifd.GetVal2()` finds no slot in `emit_iface_dispatch` (returns
  `None`) → falls to the methods-keyed block (no `GetVal2` entry on the iface id) →
  the undef catch-all. The acceptance check is that this stays **verify-clean** with
  a real `Ref(IFaceD$i16)` receiver. **Generic interface extensions are explicitly
  DEFERRED (§5).**
- `IDAdd(cd)` → `val.Add<T2>(…)` (the method-generic double-generic, line 208/267) —
  dropped from `imethods` (§3.3), DEFERRED (§5).
- `SGet`/`SGet2` → `T.SMethod(…)` (static-virtual, lines 213/272) — filtered out of
  `imethods`, DEFERRED (§5).

**T-PRE acceptance includes:** run the verify corpus immediately after Seam A lifts
and confirm `GetVal2`/`IDAdd`/`SGet` lowering stays verify-clean under the flip. If
any regresses to malformed IR, the 162/162 verify ratchet breaks. The behavioral
proof of dispatch is solely the NEW `generic_iface_*.bf` run-corpus programs (§4),
NOT `Interfaces.bf` (which is verify-only).

---

## 4. Worked examples (the run-corpus programs that prove it)

All under `e:/NewBF/beef-tests/run-corpus/`, `Program.Main -> int32`, `// expect:
N`, JIT-run full-i32 value checks under the Stomp guard (the **authoritative**
gate). Each is self-contained (inline-defines its generic interface + implementing
class; `new`/`malloc`/`free` resolve as for existing `new`-using programs). Each
inline fixture declares ONLY abstract methods on the interface (no method-generics,
no statics, no extension) so the `imethods` slot shape is clean (§2.3). The 13
existing non-generic `iface_*` programs and `interface_constraint.bf` must stay
green (the non-generic itable path is unchanged).

1. **`generic_iface_dispatch.bf` — `expect: 123`** (the headline proof). Inline
   `interface IFaceD<T> { T GetVal(); }`; `class ClassD : IFaceD<int16> { public
   int16 GetVal() { return 123; } }`; `IFaceD<int16> ifd = cd; return
   (int32)ifd.GetVal();` → **123**. Pins: `IFaceD$i16` registered, `ClassD` gets a
   real itable, the interface-typed value dispatches dynamically (today: `undef`).
   This is the `iface_dispatch_basic.bf` analogue for the generic path — and it, not
   `Interfaces.bf`, is the live behavioral regression.

2. **`generic_iface_two_impls.bf` — `expect: 357`** (the shared-mono-id pin). Two
   classes `ClassD : IFaceD<int16>` (`GetVal → 123`) and `ClassE : IFaceD<int16>`
   (`GetVal → 234`); dispatch through an `IFaceD<int16>`-typed view of each, sum →
   **357**. Pins that **one** `IFaceD$i16` id is shared by both implementers and the
   slot base agrees (Risk R2 — an off-by-one or duplicate mono id mis-dispatches).

3. **`generic_iface_param.bf` — `expect: 357`**. `int32 F(IFaceD<int16> v) { return
   (int32)v.GetVal(); }` called with a `ClassD` then a `ClassE`, summed → **357**.
   Pins interface-typed **parameter** passing + dispatch (the upcast at the call
   boundary is the no-op `coerce` of `Ref(ClassD) → Ref(IFaceD$i16)`).

4. **`generic_iface_distinct_args.bf` — `expect: 7`** (distinct-mono independence,
   **rewritten to avoid return-type-only overloading**). `IFaceD<int16>` and
   `IFaceD<int32>` are distinct monos (`IFaceD$i16` vs `IFaceD$i32`, distinct ids,
   distinct slot bases). Because `resolve_itable_impl` discriminates overloads via
   `pick_overload(cands, formals, true)` (`lower.rs:1319-1321`) — by FORMAL PARAMS
   only, **never** return type (`lower.rs:6019`+) — the two views CANNOT differ by
   return type alone (Beef/C# forbid return-type-only overloading anyway). Instead
   use **distinct method names**: `interface IGetA<T> { T A(); }` and `interface
   IGetB<T> { T B(); }`, a class `: IGetA<int16>, IGetB<int32>` with `int16 A()→3`
   and `int32 B()→4`, dispatched through each interface view independently, summed
   → **7**. Pins that two instantiations of distinct generic interfaces are
   unrelated types with independent itables (the no-variance v1 default, Risk R4).
   (A single generic interface at two args — `IFaceD<int16>` + `IFaceD<int32>` on
   one class — also works for the SAME-named `GetVal` only if their formals differ;
   to keep the example unambiguous, distinct interface names are used.)

5. **`generic_iface_inherit.bf` — `expect: 5`**. `interface IA<T> { T GetA(); }`;
   `interface IB<T> : IA<T> { T GetB(); }`; `class C : IB<int16>` implementing both;
   call an `IA<int16>` method (`GetA`) through an `IB<int16>`-typed value (slot from
   the transitively-flattened base block). Pins **Seam C′** — `iface_bases[C]` must
   contain `IA$i16` (not just `IB$i16`), so `apply_itables` composes the `IA$i16`
   block into C's vtable. The dump-ir gate asserts `iface_bases[C].contains(&IA$i16)`.

6. **`generic_iface_is_as.bf` — `expect: 1`**. `obj is IFaceD<int16>` true; `obj as
   IFaceD<int16>` non-null then dispatches. Pins **Seam F′** — the NEW
   `type_id_of` generic-RHS arm (§3.6) resolves `IFaceD<int16>` (an
   `Expr::Generic`) to the mono id; then `type_test`'s
   `iface_bases[c].contains(&mono_id)` membership test is id-keyed and unchanged.

7. **`generic_iface_constraint_ok.bf` — `expect: 11`** (the constraint-enforcement
   positive). `static int32 Use<T>(T v) where T : IFaceD<int16>` called with a
   conforming `ClassD`; no diagnostic, dispatches by erasure → a value. Pins that
   the promoted `GenericInterface` constraint **accepts** a conforming arg (no false
   positive). A paired **verify-corpus** negative fixture (a non-implementer arg)
   asserts the new diagnostic fires (the CT-T3 analogue). NOTE: enforcement is
   **arity-level** (§3.7 R5) — neither fixture probes the
   `IFaceD<int32>`-satisfies-`IFaceD<int16>` false-negative, which is deferred.

8. **`generic_iface_default.bf` — `expect: 7`** (Seam F exerciser). Inline
   `interface IFaceX<T> { T Get(); int32 Twice() { return 0; } }` with a class
   overriding neither/one; dispatch the **default-bodied** `Twice()` through an
   `IFaceX<int16>`-typed value. Pins that `lower_type_at` emits the default at the
   mono id and `idefaults[IFaceX$i16]` resolves to that symbol. Without this fixture
   Seam F is dead code — so it ships with this program or Seam F is deferred (§5).

**Regression (verify-only):** `Interfaces.bf` currently lowers `IFaceD<int16> ifd =
cd; ifd.GetVal()` (lines 462-466) to the verify-clean `Ptr`+undef fallback
(itables.md §6/§10). After this feature, `IFaceD<int16>` flips to `Ref(IFaceD$i16)`.
`Interfaces.bf` is consumed **only** by the verify corpus (`corpus.rs:134/170`,
`analyze` + `lower_program` + `verify_module`) — its `Test.Assert(v == 123)` is
**NEVER JIT-run**. So the gate is "Interfaces.bf stays **verify-clean** after the
flip" (including the deferred-feature paths of §3.9), NOT "v == 123 holds." The
behavioral proof is `generic_iface_dispatch.bf → 123` (run-corpus, JIT, authoritative).

---

## 5. v1 scope vs explicitly deferred (be honest)

### In v1 (the common cases — copy the non-generic itable playbook, parameterized by a mono id)

- A **single-type-param** generic interface (`IFaceD<T>`) with
  **concrete-after-substitution** method signatures (`T GetVal()` → `i16 GetVal()`),
  implemented by a **concrete class** (`class ClassD : IFaceD<int16>`).
- A class implementing `IFaceD<int16>` gets a real `IFaceD$i16`-keyed itable.
- An `IFaceD<int16>`-typed value (local, param, field, return) dispatches
  dynamically through the itable — for **method** members only (not properties/
  indexers; see deferred).
- `interface IB<T> : IA<T>` transitive links (mono-to-mono flattening **into both**
  the `imethods`-flatten map and the class-routing `iface_links` map, Seams C+C′)
  for the single-type-param case.
- `is`/`as` against a mono interface id (the NEW `type_id_of` generic-RHS arm, Seam
  F′).
- Default-bodied **concrete-signature** generic-interface methods emitted at the
  mono id (Seam F) — shipped with its dedicated fixture (`generic_iface_default.bf`).
- Generic-interface constraint **enforcement** diagnostics at **arity level** (`T :
  IFaceD<int16>`, Seam G) — the positive (accept a conforming implementer of *some*
  `IFaceD<…>`) and the negative (reject a non-implementer).

### Explicitly deferred (the gnarly cases — be honest about the hard parts)

- **Generic interface EXTENSIONS** (`extension IFaceD<T> { T GetVal2() {…} }`,
  Interfaces.bf:221). A generic extension is skipped (`lower.rs:3559`) and an
  interface extension is skipped (`lower.rs:3563`); its members are NEVER merged
  into the mono interface's `imethods`. After the type-flip, a call on such a method
  through an interface-typed value falls to the undef catch-all and must stay
  **verify-clean** (§3.9). Merging extension members into the mono template is a
  follow-on.
- **Generic-interface PROPERTIES / INDEXERS** (`T this[int] { get; set; }` in
  `IIndexable<T>`, Indexers.bf:6). `collect_iface_own_type` walks `Member::Method`
  only (`lower.rs:1471`), so a property/indexer never becomes a slot. Safe for the
  ratchet (empty `imethods`, no panic) but means these do not dispatch in v1.
- **Explicit impl of a generic interface** (`float IIndexable<float>.this[…]`,
  Indexers.bf:113). The `explicit_impls` key would need the mono iface id; untested
  in v1.
- **Variance** (`IEnumerator<out T>`, covariant/contravariant assignment). NewBF has
  no variance machinery. Because `coerce` treats all pointer-likes as identical
  no-ops, an *unchecked* `Ref(IEnumerator$Dog) → Ref(IEnumerator$Animal)` would
  dispatch through the wrong itable for any non-pointer-returning method — a
  **miscompile**. **v1 treats distinct monos as unrelated types** (pinned by
  `generic_iface_distinct_args.bf`, example 4). Variance is the genuinely-hard
  deferred case (Risk R4).
- **Arg-level constraint enforcement.** Seam G enforces at **arity** level
  (`IFaceD<int32>` satisfies `T : IFaceD<int16>`, a false negative on the wrong
  instantiation) because `resolve_base`/`lookup` ignore type args
  (`constraints.rs:981-988`). Arg-level needs a mangled-arg compare threaded through
  the def-graph (§3.7 R5).
- **Generic interface methods with their OWN type params** (`T Add<T2>(...)` in
  `IFaceD<T>`, Interfaces.bf:208). `collect_iface_own_type` drops generic methods
  (`lower.rs:1486`); the mono fill keeps dropping them (no itable slot), exactly as
  method-generics are dropped today.
- **Static virtual interface methods** (`static T SMethod(T val)` + `T.SMethod(...)`,
  Interfaces.bf:213,272). The constraint-static path; a separate
  static-virtual-interface feature. The mono itable keeps filtering statics out of
  `imethods` (`lower.rs:1483-1485`).
- **A generic class implementing a generic interface** (`class Foo<U> : IFaceD<U>`).
  Needs the class's own monomorph env to resolve `U` in the base (Seam D passes the
  empty env). v1 handles only **concrete** implementers.
- **Abstract-T constraint dispatch** through a mono interface — unreachable in v1
  (`targ_is_abstract` refuses abstract type-args, `lower.rs:1973`); not a v1 trigger.
- **Boxing value structs to a generic interface** (`struct StructA : IFaceB`,
  Interfaces.bf:34). Value structs have no `$header`/vtable (out of scope for the
  non-generic itable too, itables.md §10). `collect_iface_bases_type` already skips
  non-classes (`td.kind == TypeKind::Class` guard, `lower.rs:1652`).
- **`delete`/`GetType` through a generic-interface-typed value.** No `$dtor` slot in
  `imethods` in v1. An interface-typed `GetType` falls through to the existing
  `StructKind::Interface => {}` slot path (`lower.rs:12041`). (Confirmed: interfaces
  carry no dense type-id — the reflectable set filters `StructKind::Ref` only at
  `lower.rs:5103`, so a mono interface never gets a ClassVData/type-id; `type_test`
  is membership-keyed, not type-id-keyed, so `is`/`as` works regardless.)
- **Multi-type-param generic interfaces** (`IFaceD<T, U>`). The mangle
  (`IFaceD$i16i32`) and env-binding already generalize, but the slot-stability +
  flatten testing scope is single-param for v1.

---

## 6. Load-bearing risks + mitigations

- **R1 — The env-driven `imethods` fill is the deepest correctness point.** A wrong
  `this` id (`Ref(template_id)` not `Ref(mono_id)`) or an unresolved `T` (`ret = Ptr`
  or `Ref(template)`) desyncs the ABI gate `itable_abi_matches` (`lower.rs:1353`),
  and `resolve_itable_impl` falls through to its terminal `debug_assert!(false, …)`
  (`lower.rs:1337`) → a **LOUD panic in the debug verify corpus** (not a silent
  miscompile — the verify ratchet runs assertions-on). *Mitigation:* the fill passes
  the mono `env` to `lower_value_ty`/`param_ir_ty`/`pointer_elem_env` (so `T → i16`)
  and `vec![IrType::Ref(mono_id)]` as the leading `this`; a **dump-ir gate** (§8 T2)
  asserts `imethods[IFaceX$i16] == [("GetVal", sig)]` (on a CLEAN inline fixture, not
  feature-suite `IFaceD`) with `sig.ret == i16` and `sig.params[0] == Ref(IFaceX$i16)`
  BEFORE any dispatch task; the run-corpus value (`123`, not garbage) is the
  behavioral net.

- **R2 — Slot-layout stability: one mono id shared by all implementers.** `ClassD`
  and `ClassE` (both `: IFaceD<int16>`) must route the **same** `IFaceD$i16` id, or
  the global slot base disagrees. *Mitigation:* the `seen`-set dedup
  (`lower.rs:1779`) + `(name, args.len())` keying (`lower.rs:1759`) guarantees one id
  per `(name, args)`; both classes' bases resolve through `ty_of` to the same
  `Ref(mono_id)` (`lower.rs:1658`). The bounds keystone
  `debug_assert!(slot_base >= vimpls[i].len())` (`lower.rs:1268`) catches a desync
  loud **in a debug/assertions-on profile** (R10); `generic_iface_two_impls.bf → 357`
  (example 2) pins the value in any profile.

- **R3 — The monomorph fixpoint assert (`monos2.is_empty()`, `lower.rs:571-575`)
  + the step-4e ordering.** If an interface-base mono is discovered in pass 2, the
  assert trips; if the `imethods` fill runs before the mono is registered, it
  iterates nothing. *Mitigation:* Seam B walks `td.bases` in **pass-1**
  `collect_insts_type` (`lower.rs:2056`, same pass as field/return-type monos; bases
  are syntactic). The fill is wired into `fill_iface_members`'s `t.monos` iteration
  (§3.3), which runs at step 4e (`lower.rs:586`), after step-4 mono registration
  (`lower.rs:523`), so every interface mono id exists.

- **R4 — Variance is a miscompile trap if accidentally allowed.** *Mitigation:* v1
  treats distinct monos as **unrelated** (`IFaceD$i16` ≠ `IFaceD$i32`, distinct ids,
  no subtyping); `coerce`'s pointer no-op never bridges them because they are
  distinct nominal targets only reached via an explicit (deferred) variance rule.
  `generic_iface_distinct_args.bf → 7` (example 4, distinct interface NAMES, no
  return-type overloading) pins independence.

- **R5 — Constraint enforcement is arity-level, not arg-level.** The `GenericBound →
  GenericInterface` promotion (Seam G) enforces "implements **some** instantiation,"
  because `resolve_base`/`lookup` ignore type args (`constraints.rs:981-988`). It
  does NOT catch `IFaceD<int32>` satisfying `T : IFaceD<int16>`. *Mitigation:* be
  honest (§3.7, §5) — the v1 positive + non-implementer negative gate the
  arity-level check, which is sound for "is this even an `IFaceD`" and never a false
  POSITIVE (a non-implementer is still rejected). Arg-level is deferred. The
  enforcement arm uses the arity-aware `lookup(name, arity)` (`constraints.rs:289`),
  NOT `lookup_arity0` (`constraints.rs:883`, which would no-op the arity-1 case).

- **R6 — Base-routing corruption.** The base-routing guard at `lower.rs:3180-3184`
  (`matches!(t.kinds[bid], StructKind::Ref)`) already prevents a class listing an
  interface base from recording it as the single inheritance base — this landed with
  the non-generic itable (IT-T1) and covers the mono interface base too (a mono
  interface is `kinds = Interface`, not `Ref`). *Mitigation:* no new code; confirm in
  T1's acceptance.

- **R7 — Ratchet breakage over the EXISTING corpus classes (the REAL R7).** Once
  Seams A–C+C′ monomorphize generic interfaces, every existing verify-corpus class
  implementing one (`ClassD`/`ClassE` Interfaces.bf:229/247, `IndexTest`/
  `IndexTestExplicit` Indexers.bf:96/112, `EnumeratorTest` Loops.bf:17) gets a real
  `iface_bases` entry that `apply_itables` must resolve completely or
  `resolve_itable_impl` panics (`lower.rs:1337`) → the 162/162 verify ratchet
  regresses. `Interfaces.bf` is **verify-only** — its `Test.Assert(v==123)` is never
  run, so it is NOT a behavioral gate. *Mitigation:* **T-PRE (§3.0)** enumerates and
  triages every such class BEFORE T3; the deferred-feature paths (§3.9) must stay
  verify-clean under the flip; property/indexer interfaces are safe (empty
  `imethods`). The behavioral proof is the NEW `generic_iface_*.bf` run-corpus
  programs (authoritative, JIT, MEMORY).

- **R8 — sema⊥llvm + no-new-surface.** *Mitigation:* every change is in `newbf-sema`
  (+ `constraints.rs`); `newbf-llvm`/`newbf-ir`/parser/runtime untouched. No new heap
  op (JIT FP-pool N/A), no new IR instruction, no comptime sandbox, no SSA cross-yield
  surface — pure type-layout + dispatch, identical IR shape to the landed non-generic
  itable (inline single-block `load_vtable_base → elem_addr → call_indirect`,
  `lower.rs:11900-11914`, journal §112 "R8-safe"). The dispatch dominates its uses
  trivially (no new block/phi).

- **R9 — `is`/`as` generic RHS needs new code (not confirm-only).** `type_id_of`
  (`lower.rs:11521`) resolves only `Expr::Ident`/`Expr::Paren`, so a generic RHS
  (`Expr::Generic`, `ast.rs:307`) returns `None` and `is`/`as` fall to `false`/`null`.
  *Mitigation:* Seam F′ (§3.6) adds the `Expr::Generic` arm (mangle args + `by_name`
  lookup); `generic_iface_is_as.bf → 1` (example 6) gates it.

- **R10 — The dump-ir/assert nets are debug-gated.** The bounds keystone
  (`lower.rs:1268`) and the unresolved-slot assert (`lower.rs:1337`) are
  `debug_assert!` — loud in debug, silent null-pad/null-slot in release. *Mitigation:*
  run the verify corpus and dump-ir gates under a **debug/assertions-on profile** (the
  default `cargo test`); the run-corpus value check (123, not garbage) is the only
  release-active net.

---

## 7. Cross-feature dependency (what it needs from / provides to others)

**This IS the foundational feature** — it needs nothing from the other three wave-4
features. Precise provides/needs (re-verified against each dependent's own doc):

**Provides (the API the dependents would consume):**
- **The monomorphized interface id** (`IFaceD$i16` as a `StructId` with `kinds =
  Interface`, resolved from `IFaceD<int16>` via `ty_of → Ref(mono_id)`,
  `lower.rs:646-654`).
- **The mono interface itable** (`imethods[mono_id]` populated env-driven,
  `iface_slot_base[mono_id]` assigned, the class's vtable carrying the impl symbol,
  the inherited mono bases flattened via Seam C′) — so any feature with an
  interface-typed generic value gets dynamic dispatch for free through
  `emit_iface_dispatch` (`lower.rs:11878`).
- **The arity-level enforced generic-interface constraint** (`GenericInterface`
  kind, `constraints.rs`) — note this lift is **independent** of the itable work
  (it rides the def-graph `TypeIndex`, not the `StructTable` mono), so it can ship
  separately.

**Consumed by (with corrected dependency status):**
- **iterators-lazy** — **v1 is INDEPENDENT** of this feature
  (`iterators-lazy.md` §7: the synthesized enumerator is a concrete monomorphized
  generic **value struct** resolved statically by name; `StructKind::Value`, so the
  exclusion never touched it). Only the **deferred** interface-typed half (`foreach`
  over an `IEnumerable<T>`-typed value, or an `IEnumerator<T>`-typed enumerator)
  needs the mono interface id + itable — a separate work item
  (`iterators-lazy.md` §7 "deferred, separate work item"). The old `iterators.md`
  §525 "Blocked on generic-interface registration/monomorphization" note is
  superseded by `iterators-lazy.md`.
- **generic-constraints `T : IEnumerator<TElement>`** — the constraint
  **enforcement** diagnostic needs only the def-graph `TypeIndex` (which keys
  generic interfaces by `(name, arity)` **today**, `constraints.rs:239`), so Seam G
  delivers it **without** the lower.rs monomorphization. Constraint *dispatch*
  already works by erasure. (Earlier framing tying enforcement to the mono
  interface's `imethods` was inaccurate — enforcement never consults `imethods`.)
- **comptime-metaprogramming-v2** — **NO dependency** (`comptime-metaprogramming-v2.md`
  §7: "a leaf in the wave-4 dependency graph — it neither blocks nor is blocked by
  generic-interface monomorphization"; method/attribute reflection reads value-struct
  metadata, no interface dispatch). Listed only to record that it does NOT consume
  this feature.
- **delegates' generic delegates** (`Action<T>`/`Func<T>`). The Func two-word
  value-struct repr is structural, not interface-dispatched; generic delegates
  monomorphize as value structs, **independent** of this feature for v1.

**What the dependents ship WITHOUT this feature:** eager iterators (done),
lazy-yield's concrete-enumerator half (independent), constraint-dispatch-by-erasure
(done), comptime method/attribute reflection (independent), the Func value-struct
repr (done). **What genuinely needs it:** interface-typed *generic* enumerator
values dispatching dynamically (deferred iterators-lazy half), and the
generic-interface constraint *enforcement* diagnostic (which Seam G provides off the
def-graph, independent of the itable).

---

## 8. Task breakdown (ordered, agent-assignable)

Gates that must stay green at **every** task boundary: verify corpus 162/162,
parser corpus, run-corpus (authoritative, JIT full-i32 under the Stomp guard, run in
a **debug/assertions-on** profile so the itable asserts are live), the 13
non-generic `iface_*` programs, and `interface_constraint.bf`. A task lands only when
its own test plus all prior gates are green.

**T-PRE — Triage the existing corpus generic-interface implementers (Seam-independent analysis).**
*Seed:* enumerate every verify-corpus class whose base is a generic interface
(`ClassD`/`ClassE` Interfaces.bf:229/247, `IndexTest`/`IndexTestExplicit`
Indexers.bf:96/112, `EnumeratorTest` Loops.bf:17). For each, predict whether, once
its interface monomorphizes, `apply_itables` resolves a complete ABI-matching itable
or `resolve_itable_impl` panics (`lower.rs:1337`). Classify each as (a) method-only,
resolves; (b) property/indexer-shaped, empty `imethods`, safe; (c) a genuine gap →
narrow the v1 trigger (defer that shape in §5). Document the §3.9 deferred-feature
paths (`GetVal2`/`IDAdd`/`SGet`) that must stay verify-clean under the flip.
*Deps:* none (analysis only; pairs with T0–T2 acceptance).
*Acceptance:* a written triage covering every implementer + the §3.9 paths; the
v1-trigger scope is bounded so no corpus class can panic `resolve_itable_impl` after
T2.

**T0 — Register generic interfaces as monomorphizable (Seam A).**
*Seed:* in `index_generic_decls` (`lower.rs:716`), remove the `td.kind !=
TypeKind::Interface` conjunct (`lower.rs:737`) so a generic interface enters the
`(name, arity)` `GenericDecls` map; confirm `record_inst` mints the mono with
`kinds = Interface` (`lower.rs:1781-1782`, no code change) and `lower_ty_env`'s
generic arm returns `Ref(mono_id)` once registered (`lower.rs:13218-13226`, no code
change).
*Deps:* none.
*Acceptance:* verify 162/162, parser, run-corpus all green (and the §3.9
deferred-feature paths in Interfaces.bf stay verify-clean under the flip — run the
verify corpus and triage per T-PRE); a **dump-ir gate** that `IFaceD<int16>` (with a
`class ClassD : IFaceD<int16>` present + Seam B from T1) resolves to a registered
`Ref` id with `kinds = Interface`. Behavior-neutral except where a generic interface
is referenced in a type position.

**T1 — Discover interface-base mono requests (Seam B).**
*Seed:* add ONE `td.bases` walk (via `use_in_type`) to `collect_insts_type`
(`lower.rs:2056`, before the member loop), threading the visitor state already in
scope, so `class ClassD : IFaceD<int16>` requests the `IFaceD$i16` mono in **pass 1**
(`lower.rs:497-508`). **Do NOT** edit `collect_insts_items` (it only delegates,
`lower.rs:2037`).
*Deps:* T0.
*Acceptance:* all gates green (incl. T-PRE corpus triage); the `monos2.is_empty()`
assert (`lower.rs:571`) holds; a dump-ir gate that `IFaceD$i16` is a registered id
(kind `Interface`) when a class implements it. Still no `imethods`, no dispatch.

**T2 — Env-driven `imethods`/`idefaults` fill + mono-link routing (Seams C + C′ + D — RISKIEST).**
*Seed:* (a) **extract** the per-method slot body of `collect_iface_own_type`
(`lower.rs:1469-1527`) into a helper taking `(mono_id, template_decl, mono_env)` that
bypasses the `generic_params.is_empty()`/`by_name` gate and threads the mono env into
`param_ir_ty`/`lower_value_ty`/`pointer_elem_env` (sites at `lower.rs:1495/1516/1500`)
+ `this = Ref(mono_id)` (`lower.rs:1493`); (b) extend `fill_iface_members`
(`lower.rs:1405`) to iterate `t.monos`, run the helper per interface-kind mono, and
merge into the `own` map before `compose_iface_members` (`lower.rs:1416`); (c) **Seam
C′:** in `collect_iface_bases` (`lower.rs:1588`), after building `iface_links`
(`lower.rs:1592`), insert `iface_links[mono_iface_id] = [resolved mono base ids]` for
each interface-kind mono (resolving template bases through the mono env), so
`add_iface_flat` (`lower.rs:1677`) pulls `IA$i16` into `iface_bases[C]`; (d) **Seam
D** routing (`collect_iface_bases_type`, `lower.rs:1649`) matches `Ref(mono_id)` with
no code change once T0+T1 register it.
*Deps:* T1.
*Acceptance:* all gates green (incl. T-PRE); the **dump-ir gate** (R1):
`imethods[IFaceX$i16] == [("GetVal", sig)]` (on a clean inline fixture) with `sig.ret
== i16`, `sig.params == [Ref(IFaceX$i16)]`; `iface_bases[ClassD].contains(&IFaceX$i16)`;
for the inherit case `iface_bases[C].contains(&IA$i16)` (Seam C′); `apply_itables`
composes the slot (dump-ir check that `ClassD$vtable` carries `ClassD.GetVal` at
`iface_slot_base[IFaceX$i16]`). **Riskiest task** — the env-driven fill at a minted
id + the dual link routing (R1, R3, plus the C′ map-routing trap all three reviews
flagged).

**T3 — Mono-interface dispatch + first run-corpus programs (Seams E/F).**
*Seed:* confirm `emit_iface_dispatch` (`lower.rs:11878`) and the dispatch branch
(`lower.rs:12058`) fire for a mono interface id (no code change — id-keyed); confirm
default-bodied generic-interface methods emit at the mono id via the type-mono emit
loop (`lower.rs:5682-5692`, Seam F); add `generic_iface_dispatch.bf`,
`generic_iface_two_impls.bf`, `generic_iface_param.bf`, `generic_iface_inherit.bf`,
`generic_iface_default.bf`.
*Deps:* T2.
*Acceptance:* `generic_iface_dispatch.bf → 123`, `generic_iface_two_impls.bf → 357`,
`generic_iface_param.bf → 357`, `generic_iface_inherit.bf → 5`,
`generic_iface_default.bf → 7` pass under the JIT run-corpus harness; verify 162/162
(Interfaces.bf **verify-clean** — NOT a behavioral gate). This is the
minimal-but-correct first behavioral slice.

**T4 — Distinct-args independence + `is`/`as` (Seam F′).**
*Seed:* add `generic_iface_distinct_args.bf` (R4 pin, **distinct interface names**,
no return-type overloading) + `generic_iface_is_as.bf`; add the `Expr::Generic` arm
to `type_id_of` (`lower.rs:11521`) so a generic RHS resolves to the mono id (Seam
F′, NEW code — mangle args via `lower_ty_env` + `mangle_generic`, look up `by_name`).
*Deps:* T3.
*Acceptance:* `generic_iface_distinct_args.bf → 7`, `generic_iface_is_as.bf → 1`
pass; all gates green.

**T5 — Constraint classification + enforcement (Seam G — PARALLELIZABLE, deps none/T-journal).**
*Seed:* in `classify_constraint` (`constraints.rs:1057`), split the
`segments.len() > 1 || !last.args.is_empty()` arm (`constraints.rs:1090`): add an
arity-keyed kind check (`index.lookup(name, last.args.len())` at `constraints.rs:289`
→ `index.kind_by_name_arity_of(id)` at `constraints.rs:269` == `Interface`) and
return a new `ConstraintKind::GenericInterface(name, arity)`; otherwise keep
`GenericBound`. In `check_one` (`constraints.rs:838`), add a `GenericInterface` arm
using the **arity-aware** `lookup(name, arity)` (NOT `lookup_arity0`) +
`transitive_reaches` (`constraints.rs:954`). Add `generic_iface_constraint_ok.bf`
(positive, `expect: 11`) + a verify-corpus negative fixture (a non-implementer arg →
diagnostic fires). Document the arity-level (not arg-level) scope (R5).
*Deps:* none for the constraints code (rides the def-graph, independent of T0–T4);
sequence after T4 only to bundle the doc/journal. Can land first or in parallel.
*Acceptance:* `generic_iface_constraint_ok.bf → 11` passes; the verify negative
fixture's diagnostic fires (no false positive on the positive); all gates green; the
`IFaceD<T>` (arity 1) vs `IFaceD` (arity 0) coexistence in Interfaces.bf does not
confuse the classifier (the arity-keyed split).

**T6 — Journal + verify-corpus pin + doc cross-link.**
*Seed:* add a numbered journal entry to `docs/journals/` (design + outcome); add a
focused verify-corpus fixture mirroring `generic_iface_dispatch.bf` (pin the mono
itable IR shape); cross-link this doc from `itables.md` §6/§10, `iterators-lazy.md`
§7, and `generic-constraints.md` §5 (the deferral sites this lifts/supersedes).
*Deps:* T5.
*Acceptance:* journal entry present; verify corpus count incremented and green;
commit pairs with the entry (conventional style + Co-Authored-By trailer).

**Dependency chain:** `T-PRE` (analysis, parallel) gates the scope of `T0 → T1 → T2
→ T3 → T4 → T6`; `T5` (Seam G) is **independent** of T0–T4 (it rides the def-graph)
and may land first or in parallel, bundled before T6 for the doc. T2 is the critical
sub-task (the env-driven fill + the dual mono-link routing); T3 is the behavioral
core. **Final task count: 8** (T-PRE, T0–T6).
