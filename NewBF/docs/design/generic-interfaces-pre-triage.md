# GI-PRE — Existing-Corpus Generic-Interface Implementer Triage (the scope contract for the GI chain)

> **Status: ANALYSIS (wave 4, GI-PRE).** This is the gating triage that bounds the
> scope of `GI-T0`'s flip (lifting the `td.kind != TypeKind::Interface` exclusion at
> `lower.rs:737`). It enumerates **every** existing verify-corpus class whose base is a
> generic interface, predicts the post-flip `apply_itables` outcome for each, and bounds
> the v1 trigger so that after `GI-T2` **no corpus class panics `resolve_itable_impl`**
> (`lower.rs:1337`) and the verify corpus stays **162/162**. It is ANALYSIS only — no
> compiler logic changes here.
>
> Companion to [`generic-interfaces.md`](generic-interfaces.md) (§3.0 T-PRE / §3.9 / §5)
> and [`SPRINT-PLAN-4.md`](SPRINT-PLAN-4.md) (the GI-PRE row, R2/R-A, the **F2** addendum).
> Every load-bearing claim is anchored to a re-verified `file:line` in
> `e:/NewBF/NewBF/src/newbf-sema/src/lower.rs` and the corpus.

---

## 0. The decisive structural fact: the verify corpus is STANDALONE, file-by-file

The verify corpus (`newbf-sema/tests/corpus.rs:56-66`, `:118-137`, `:160-180`) analyzes
and lowers **each `.bf` file as its own one-file program** — one `SourceFile { file:
FileId(0), … }` slice per `path`, `analyze(&srcs)` then `lower_program(&srcs, …)`. It
collects **only** `corlib-slice/` + `feature-suite/src/` (`corpus.rs:45-46/120-121/153-154`);
**`feature-suite/BeefLinq/` is NOT in the corpus**.

This is the keystone that bounds the entire triage. A generic interface monomorphizes for
an implementing class **only if its interface TEMPLATE decl is in the SAME file** as the
class. The chain is:

1. `index_generic_decls` (`lower.rs:716`) only indexes a generic interface template that
   is present in the unit (after the `:737` flip). If the template is in another file, it
   is never in `GenericDecls`.
2. `record_inst` (`lower.rs:1745`) bails at `else { return; }` (`lower.rs:1759-1761`) when
   `(name, arity)` is not in `GenericDecls` — so no mono id is minted.
3. `lower_ty_env`'s generic arm (`lower.rs:13218-13226`) resolves the unregistered base via
   `structs.ty_of(&mangled).unwrap_or(IrType::Ptr)` → **`IrType::Ptr`**, never `Ref`.
4. `collect_iface_bases_type` (`lower.rs:1649`) only routes a base into `iface_bases[class]`
   when it resolves to `IrType::Ref(bid)` with `is_interface(bid)` (`lower.rs:1658-1664`). A
   `Ptr` base never matches → the class gets **no `iface_bases` entry** → `apply_itables`
   composes **nothing** for it → `resolve_itable_impl` is never called → **no panic**.

So **a cross-file generic-interface implementer is panic-proof under the flip**, regardless
of whether its impl methods would ABI-match. Only a **same-file** template can monomorphize
and thus reach `resolve_itable_impl`. That cleanly partitions the corpus.

---

## 1. The itable machinery (verified read)

For a class `C` implementing interface `I`, `apply_itables` (`lower.rs:1238`) walks
`imethods[I]` and, for each slot, calls `resolve_itable_impl` (`lower.rs:1295`) which
resolves the impl symbol in priority order:

1. **explicit impl** — `explicit_impls[(C, I, name)]` filtered by `itable_abi_matches`
   (`lower.rs:1310-1316`).
2. **implicit / inherited** — `pick_overload(methods[C][name], formals, members=true)`
   filtered by `itable_abi_matches` (`lower.rs:1319-1325`).
3. **interface default** — `idefaults[I][k]` if `Some` (`lower.rs:1327-1329`).
4. **NONE of the above** → the terminal `debug_assert!(false, "class … does not implement
   …")` (`lower.rs:1337-1344`) — a **LOUD panic in the assertions-on verify corpus**.

`itable_abi_matches` (`lower.rs:1353`) = same arity, `abi_compatible(ret)`, and
`abi_compatible` on every param. `abi_compatible(a, b) = a == b || (both pointers)`
(`lower.rs:1205-1207`) — so the leading `this` (`Ref(C)` impl vs `Ref(I)` slot, both
pointers) always matches, and a concrete `i16` return matches an env-resolved `i16` slot
return exactly.

**Which interface members become slots** (`collect_iface_own_type`, `lower.rs:1445-1534`):

- It walks **`Member::Method` ONLY** (`lower.rs:1470-1471`). `Member::Property` (indexers
  `T this[int] { get; set; }` parse as a property) and `Member::Constructor`/`Member::Field`
  are **never** turned into a slot.
- It **filters out** `static` methods and method-generic methods (`is_static ||
  !generic_params.is_empty()`, `lower.rs:1483-1488`) so they never consume a slot index.
- An interface with no qualifying instance non-generic method → **empty `imethods`** →
  `apply_itables` composes zero slots → no `resolve_itable_impl` call → **structurally
  panic-proof** even when the class has the `iface_bases` entry.

**Only classes are routed** (`collect_iface_bases_type`, `lower.rs:1652`, `td.kind ==
TypeKind::Class` guard): value `struct`s that list an interface base are skipped (boxing
out of scope), so a `struct … : I<…>` never enters `iface_bases` and never panics.

The Seam-C env-driven fill (GI-T2) resolves a same-file generic interface template's slot
sigs through the mono env (`T → i16`) with `this = Ref(mono_id)` — that is the keystone the
ABI gate depends on (R1), but it does not change **which members are slots**: the same
`Member::Method`-only / static-and-generic-filtered rules apply at the mono id.

---

## 2. Per-class triage table

Every existing-corpus class whose base is a generic interface (re-verified by grep over
`feature-suite/src` + `corlib-slice` for a class base of the form `I…<…>`). The
**"template in same file?"** column is the decisive predictor (§0).

| Class | Interface base | Template in same file? | Monomorphizes in corpus? | `imethods[I_mono]` slots | Class's matching impl | Prediction | Classification |
| ----- | -------------- | ---------------------- | ------------------------ | ------------------------ | --------------------- | ---------- | -------------- |
| `ClassD` (Interfaces.bf:229) | `IFaceD<int16>` | **YES** (`IFaceD<T>` Interfaces.bf:204) | **YES** → `IFaceD$i16` | `[("GetVal", ret=i16, params=[Ref(IFaceD$i16)])]` | `int16 GetVal()` (:231) → ret `i16`, this `Ref(ClassD)` → ABI-match | **RESOLVES** | **(a) method-only / resolves** |
| `ClassE` (Interfaces.bf:247) | `IFaceD<int16>` | **YES** (same `IFaceD<T>`, **same mono id**) | **YES** → shared `IFaceD$i16` | `[("GetVal", ret=i16, …)]` | `int16 GetVal()` (:249) → ABI-match | **RESOLVES** | **(a) method-only / resolves** |
| `IndexTest` (Indexers.bf:96) | `IIndexable<float>` | **YES** (`IIndexable<T>` Indexers.bf:4) | **YES** → `IIndexable$f32` | **EMPTY** — sole member is a property/indexer `T this[int]{get;set;}` (parses `Member::Property`, never a `Member::Method` slot, `lower.rs:1470-1471`) | n/a (no slot to resolve) | **NO SLOT → no `resolve_itable_impl` call** | **(b) property/indexer empty-imethods / safe** |
| `IndexTestExplicit` (Indexers.bf:112) | `IIndexable<float>` | **YES** (same `IIndexable<T>`) | **YES** → shared `IIndexable$f32` | **EMPTY** (same as above) | n/a (explicit `float IIndexable<float>.this[int]`, but no method slot exists to key it against) | **NO SLOT → no panic** | **(b) property/indexer empty-imethods / safe** |
| `EnumeratorTest` (Loops.bf:17) | `IEnumerator<int32>` (+ `IDisposable`) | **NO** — there is **no `interface IEnumerator` declaration anywhere** in the verify corpus (grep: zero `interface IEnumerator` matches in `feature-suite/src` or `corlib-slice`) | **NO** → `IEnumerator<int32>` stays `IrType::Ptr` (unregistered, `lower.rs:13226`) | n/a (interface never registers; no `iface_bases` entry) | n/a | **NEVER MONOMORPHIZES → no `iface_bases` entry → no panic** (see §3, F2) | **(d) cross-file template, never-monomorphized / safe** |
| `TimeZoneInfo` (TimeZoneInfo.bf:57) | `IEquatable<TimeZoneInfo>` | **NO** — `IEquatable<T>` is in `corlib-slice/IEquatable.bf:8`, a **separate file** | **NO** → stays `Ptr` | n/a | (also declares `bool Equals(TimeZoneInfo other)` :890, so it would resolve even if combined) | **NEVER MONOMORPHIZES (standalone) → no panic** | **(d) cross-file template / safe** — *plan missed this; see §3.1* |
| `AdjustmentRule` (TimeZoneInfo.bf:3191) | `IEquatable<AdjustmentRule>` | **NO** — `IEquatable<T>` in separate `IEquatable.bf` | **NO** → stays `Ptr` | n/a | (declares `bool Equals(AdjustmentRule other)` :3257) | **NEVER MONOMORPHIZES (standalone) → no panic** | **(d) cross-file template / safe** — *plan missed this; see §3.1* |

**Non-implementers explicitly excluded** (their generic base is a CLASS, not an interface,
so they are class-inheritance, never an itable — re-verified):

| Class | Base | Why excluded |
| ----- | ---- | ------------ |
| `Constraints.bf:23` `Dicto : Dictionary<int,float>` | `Dictionary<…>` (class) | generic class base, not an interface |
| `Generics.bf:89` `ClassC : Singleton<ClassC>` | `Singleton<T>` is a generic **class** (`class Singleton<T>` Generics.bf:79) | class inheritance |
| `Params.bf:299` `ClassB : ClassA<(int a, float b)>` | `ClassA<…>` (class) | class inheritance |
| `TypeLookup.bf:126` `DictExt : Dictionary<int,float>` | `Dictionary<…>` (class) | class inheritance |
| `Indexers.bf` `StructA`, `Interfaces.bf` `StructA` etc. | value structs : iface | `collect_iface_bases_type` skips non-classes (`lower.rs:1652`) — never in `iface_bases` |
| `corlib-slice/Type.bf:129/1010/1167` `struct Enumerator : IEnumerator<…>` | value `struct` : generic iface | value struct → skipped (`:1652`); AND `IEnumerator` not declared in the unit → `Ptr` anyway |

**Net:** the only classes that actually monomorphize a generic interface in the corpus are
the **four same-file** ones — `ClassD`, `ClassE` (resolve, case a) and `IndexTest`,
`IndexTestExplicit` (empty `imethods`, case b). **None is a gap (c).** Every cross-file
implementer (`EnumeratorTest`, `TimeZoneInfo`, `AdjustmentRule`) stays `Ptr` and is
structurally panic-proof.

---

## 3. F2 — explicit resolution of `EnumeratorTest : IEnumerator<int32>`

The completeness critic (SPRINT-PLAN-4 addendum **F2**) requires this one be **settled**,
not silently carved out, because its `MoveNext`/`Current`/`Dispose` are genuine method
slots (not the empty-`imethods` property case) and `IEnumerator<int32>` is the very
interface the deferred IL/DE halves need GI to deliver.

**Resolution: `EnumeratorTest` does NOT panic — but NOT because its methods resolve.** It
does not panic because **`IEnumerator<int32>` never monomorphizes in the verify corpus**:

- **There is no `interface IEnumerator` declaration anywhere in the verify corpus.** Grep
  for `interface IEnumerator` over `feature-suite/src` + `corlib-slice` returns **zero
  matches**. (`IEnumerator` *appears* only as a base in `feature-suite/BeefLinq/` — which
  is NOT in the corpus — and as the base of value `struct Enumerator` decls in
  `corlib-slice/Type.bf`, which never declare the interface either.) SPRINT-PLAN-4:206
  independently records "an `IEnumerator`/`IEnumerable` pair **in corlib** (verified: zero
  matches in `newbf-corlib`)."
- Therefore, when `Loops.bf` is lowered **standalone** (the only way the corpus lowers it),
  `IEnumerator<T>` is not in `GenericDecls`, `record_inst` bails (`lower.rs:1759-1761`),
  and `IEnumerator<int32>` resolves to **`IrType::Ptr`** (`lower.rs:13226`). The base never
  matches `IrType::Ref` in `collect_iface_bases_type` (`lower.rs:1658`), so `EnumeratorTest`
  gets **no `iface_bases` entry**, `apply_itables` composes nothing, and
  `resolve_itable_impl` is **never invoked**. No panic, under GI-T0/T1/T2.
- The non-generic `IDisposable` base of `EnumeratorTest` is already handled today (it is a
  non-generic interface; `Dispose()` :21 resolves through the existing non-generic itable
  path, unchanged by GI).

**Why this is the *correct* settlement and NOT a circular carve-out.** The F2 concern is
that "narrow the v1 trigger to skip `IEnumerator<int32>`" would un-deliver GI's reason to
exist. **No narrowing is required here.** `EnumeratorTest` is panic-safe by the *general*
standalone-isolation property (§0), the same property that makes `ClassD` resolve cleanly —
**not** by any `IEnumerator`-specific carve-out. The GI-T0 flip does **not** add an
exception for `IEnumerator`; it simply never sees an `IEnumerator<T>` template to
monomorphize in the corpus. So GI's trigger stays uniform, and nothing about delivering
`IEnumerator<int32>` *for a real future caller* (an inline run-corpus fixture, or a corlib
`IEnumerator` added by the deferred IL half) is weakened.

**The HEADLINE existing-corpus proof of GI is therefore `ClassD`/`ClassE : IFaceD<int16>`,
NOT `EnumeratorTest`.** `ClassD`/`ClassE` are the real same-file generic-interface
implementers that monomorphize cleanly (case a): `IFaceD<int16>` → `IFaceD$i16`, slot
`[("GetVal", i16)]`, both classes' `int16 GetVal()` ABI-match → the itable resolves
completely with no panic. `EnumeratorTest` cannot be the headline because it never
monomorphizes in the corpus (its template is absent). The *behavioral* headline remains the
new run-corpus `generic_iface_dispatch.bf → 123` (generic-interfaces.md §4), which
inline-declares its interface so it actually monomorphizes under the JIT.

**Caveat flagged for the deferred IL/DE half (NOT a GI-PRE blocker).** The moment a future
work item adds a real `interface IEnumerator<T> { … MoveNext()/Current/… }` to corlib (the
IL §7 deferred dependency `(c)`), any class implementing it **in the same compilation unit**
WILL monomorphize and MUST declare ABI-matching `MoveNext`/`get_Current`/etc. — at that
point `EnumeratorTest`'s shape (it declares `GetNext()`/`Dispose()`, **not**
`MoveNext`/`Current`) would matter. But: (i) that corlib interface does not exist today, so
it is out of GI-PRE scope; (ii) `EnumeratorTest` and the future corlib `IEnumerator<T>`
would still be in **different files** (Loops.bf vs corlib), so even then the standalone
corpus keeps them apart. This is recorded so the IL deferred-half author re-runs this triage
when they land the corlib `IEnumerator<T>`.

### 3.1 Additional generic-interface implementers the plan missed

The GI-PRE seed (and SPRINT-PLAN-4 R2/R-A) listed `ClassD`/`ClassE`/`EnumeratorTest`/
`IndexTest`/`IndexTestExplicit`. A grep over `corlib-slice` for class bases of the form
`I…<…>` surfaced **two more** that the plan did not enumerate:

- **`TimeZoneInfo : IEquatable<TimeZoneInfo>`** (`corlib-slice/TimeZoneInfo.bf:57`).
- **`AdjustmentRule : IEquatable<AdjustmentRule>`** (`corlib-slice/TimeZoneInfo.bf:3191`).

`IEquatable<T>` (`corlib-slice/IEquatable.bf:8`) is a **generic interface with a genuine
method slot**: `bool Equals(T val2)` (`IEquatable.bf:10`). So if it monomorphized in the
same unit as these classes, the slot `[("Equals", ret=bool, params=[Ref(IEquatable$…), Ref(…)])]`
would need an ABI-matching impl.

**Both are SAFE (case d), on two independent grounds:**

1. **Standalone isolation (the primary, structural reason).** `IEquatable<T>` lives in
   `IEquatable.bf`, a **separate file** from `TimeZoneInfo.bf`. Lowered standalone,
   `IEquatable<TimeZoneInfo>` is unregistered → `Ptr` → no `iface_bases` entry → no panic.
   (The mangled arg `TimeZoneInfo`/`AdjustmentRule` is itself a class in the same file, but
   the *interface template* is not, which is what gates registration.)
2. **They would resolve even if combined.** `TimeZoneInfo` declares `bool Equals(TimeZoneInfo
   other)` (:890) and `AdjustmentRule` declares `bool Equals(AdjustmentRule other)` (:3257),
   each ABI-matching the env-resolved `Equals(T)` slot (`ret=bool`, the `T`-param a pointer
   on both sides). So this shape is a latent *case (a)* — not a gap — should a future
   single-unit build ever combine them.

These are recorded for completeness; neither changes the v1-trigger bound (both are
cross-file → already panic-proof).

---

## 4. The bounded v1 trigger (GI-T0's scope contract)

GI-T0 lifts the `td.kind != TypeKind::Interface` conjunct (`lower.rs:737`) uniformly — it
does **not** need any per-interface carve-out, because the triage proves the *unconditional*
flip is already panic-safe over the existing corpus. State the scope as **enabled shapes**
(which the flip causes to monomorphize + get a real itable) vs **shapes that stay inert**
(no itable, no panic):

### Enabled by the flip (monomorphize + compose an itable) — all SAFE

- **Same-file, single-type-param generic interface with a concrete-after-substitution
  instance *method* slot, implemented by a concrete class with an ABI-matching method.**
  Exactly `ClassD`/`ClassE : IFaceD<int16>` (case a). The itable resolves completely. This
  is the in-scope v1 shape (generic-interfaces.md §5) — and the existing-corpus proof.
- **Same-file generic interface whose only members are properties/indexers (or are all
  static / method-generic).** Exactly `IndexTest`/`IndexTestExplicit : IIndexable<float>`
  (case b). `imethods` is **empty** (`collect_iface_own_type` `Member::Method`-only +
  static/generic filter, `lower.rs:1470-1488`), so `apply_itables` composes zero slots and
  never calls `resolve_itable_impl`. Safe, but these **do not dispatch** in v1 (deferred,
  generic-interfaces.md §5 "properties/indexers").

### Stay inert under the flip (never get an `iface_bases` entry) — all SAFE

- **Cross-file generic-interface implementers** — `EnumeratorTest : IEnumerator<int32>`,
  `TimeZoneInfo`/`AdjustmentRule : IEquatable<…>`. The template is in another file (or
  absent), so standalone lowering leaves the base `Ptr` (§0). Panic-proof by construction,
  no narrowing required.
- **Value `struct`s implementing a generic interface** — skipped by the `TypeKind::Class`
  guard (`lower.rs:1652`).
- **Generic-class bases that merely *look* generic** (`Dictionary<…>`, `Singleton<T>`,
  `ClassA<…>`) — class inheritance, never routed to `iface_bases`.

### NOT yet enabled (must NOT trigger a real itable in v1 — deferred, §5 of the design doc)

These are **genuine (c)-style gaps IF they were triggered**, so the v1 trigger must keep
them inert. The triage confirms **none of them is reached by the existing corpus** (so the
uniform flip is safe), and the design doc already defers each:

- **Generic interface PROPERTIES/INDEXERS dispatching** — `IIndexable<float>` is safe only
  because the slots are empty; v1 does not make them dispatch. Deferred.
- **Explicit impl of a generic interface** — `IndexTestExplicit`'s `float
  IIndexable<float>.this[int]`; the `explicit_impls` key on the mono iface id is untested.
  Safe here only because there is no method slot to key it against. Deferred.
- **Generic interface methods with their OWN type params** (`IFaceD<T>.Add<T2>`,
  Interfaces.bf:208), **static-virtual interface methods** (`IFaceD<T>.SMethod`, :213),
  **generic interface EXTENSIONS** (`extension IFaceD<T>{ GetVal2 }`, :221) — all dropped
  from / never added to `imethods` (filters at `lower.rs:1483-1488`; extension skip at
  `lower.rs:3559/3563`). They never consume a slot, so the `IFaceD$i16` slot stays exactly
  `[("GetVal", i16)]`. Deferred (§5).
- **A generic class implementing a generic interface** (`class Foo<U> : IFaceD<U>`) — needs
  the class's own monomorph env to resolve `U`; Seam D passes the empty env. **Not present
  in the corpus.** Deferred.
- **Variance** — distinct monos are unrelated types in v1. **Not present in the corpus.**
  Deferred.

**Bound, stated precisely:** *GI-T0's uniform lift of `lower.rs:737` enables exactly the
same-file (a)+(b) shapes above; every other generic-interface shape in the existing corpus
is cross-file/value-struct/class-base and stays inert. No existing corpus class can reach
`resolve_itable_impl` with an unresolved slot, so the verify corpus stays 162/162
panic-free after GI-T2. No trigger-narrowing edit is required.*

---

## 5. §3.9 deferred-path verify-cleanliness confirmation

`Interfaces.bf`'s `TestDefaults` (lines 458-500) exercises several deferred-feature paths on
the **same** `IFaceD` once `IFaceD<int16>` flips from `Ptr` to `Ref(IFaceD$i16)`. Per
generic-interfaces.md §3.9, each must stay **verify-clean** (not necessarily dispatch) after
the flip. Re-confirmed against the machinery:

- **`ifd.GetVal2()`** (:467/481) — `GetVal2` lives in a **generic `extension IFaceD<T>`**
  (:221-227). A generic extension is skipped (`lower.rs:3559`, the `generic_params.is_empty()`
  gate) and an interface extension is skipped (`lower.rs:3563`), so `GetVal2` is **never** an
  `imethods` slot on `IFaceD$i16`. After the flip it finds no slot in `emit_iface_dispatch`,
  falls to the methods-keyed block (no `GetVal2` entry on the iface id), and lands on the
  undef catch-all with a real `Ref(IFaceD$i16)` receiver — **verify-clean** (no malformed
  IR; the receiver is a well-typed pointer). Deferred (§5).
- **`IDAdd(cd)` → `val.Add<T2>(…)`** (:469/267, the method-generic) — `Add<T2>` is dropped
  from `imethods` (method-generic filter, `lower.rs:1486`). `IDAdd` is a **constraint-static**
  use (`where T : IFaceD<int16>`, resolved by erasure: `T → ClassD`, `val.Add` resolves
  statically on `ClassD.Add`, generic-constraints.md §1/§3.4) — it does **not** create an
  itable slot or a dynamic dispatch, so the flip does not touch its lowering. **Verify-clean.**
- **`SGet`/`SGet2` → `T.SMethod(…)`** (:471-487/272-277, static-virtual) — `SMethod`/`SMethod2`
  are filtered out of `imethods` (static filter, `lower.rs:1483-1485`). These are
  **constraint-static-path** uses (`T.SMethod`, resolved statically on the monomorphized
  `T = ClassD`/`ClassE`) — no itable, no dynamic dispatch. The flip does not alter their
  static resolution. **Verify-clean.**

All three are **constraint-static-path or extension uses that never create an itable slot**,
so the type-position flip (`IFaceD<int16>` becoming `Ref(IFaceD$i16)`) leaves their lowering
verify-clean. `Interfaces.bf` is a **verify-only** fixture (its `Test.Assert(v == 123)` is
never JIT-run, generic-interfaces.md §4) — so the only gate is "stays verify-clean under the
flip," which the above confirms. The behavioral proof of dispatch is the new run-corpus
`generic_iface_*.bf` programs, not `Interfaces.bf`.

---

## 6. Summary for GI-T0 (the scope contract)

- **Every existing-corpus generic-interface implementer is classified:**
  - `ClassD`, `ClassE : IFaceD<int16>` → **(a) resolves** (same-file; `int16 GetVal()`
    ABI-matches the `i16` slot) — the **headline existing-corpus proof of GI**.
  - `IndexTest`, `IndexTestExplicit : IIndexable<float>` → **(b) empty-`imethods` safe**
    (same-file; sole member is a property/indexer → no method slot).
  - `EnumeratorTest : IEnumerator<int32>` → **(d) never-monomorphized safe** (no
    `IEnumerator` decl in the corpus; standalone → `Ptr`). **F2 settled: panic-proof by the
    general standalone-isolation property, no carve-out, no foundation-thesis weakening.**
  - `TimeZoneInfo`, `AdjustmentRule : IEquatable<…>` → **(d) cross-file safe** (plan missed;
    template in separate `IEquatable.bf`; would also resolve if combined).
- **No (c) genuine gap exists in the corpus** → **no v1-trigger narrowing is required**.
  GI-T0 may lift `lower.rs:737` **unconditionally**.
- **§3.9 deferred paths** (`GetVal2`/`IDAdd`/`SGet`) **stay verify-clean** under the flip
  (extension-skip / constraint-static-path, no itable slot created).
- **Net acceptance:** after GI-T2 the verify corpus stays **162/162**, no
  `resolve_itable_impl` panic for any corpus class.

### Uncertainties / watch-items for GI-T0…T2

1. **Re-run the verify corpus after EACH of GI-T0/T1/T2** (per T-PRE acceptance) — the
   triage predicts panic-free, but the `debug_assert!` nets are debug-gated (R10), so the
   assertions-on `cargo test` profile is the live detector. The prediction is high-confidence
   (it rests only on the well-verified standalone-isolation + `Member::Method`-only +
   `TypeKind::Class`-guard properties), but the run is the confirmation.
2. **The `Member::Property` assumption for indexers.** This triage asserts `T this[int]
   { get; set; }` parses as `Member::Property` (not `Member::Method`), so `IIndexable<T>`
   yields empty `imethods`. This matches the design doc's claim (generic-interfaces.md §3.0,
   §5) and `collect_iface_own_type`'s `Member::Method`-only walk. If the parser ever routed
   an indexer to `Member::Method`, `IndexTest` would flip from (b) to needing a `this[]` slot
   resolved — re-verify in GI-T2's dump-ir gate that `imethods[IIndexable$f32]` is empty.
3. **The deferred IL/DE half re-triggers this triage.** When a corlib `interface
   IEnumerator<T>` is added (IL §7 deferred dependency (c)), any **same-file** implementer of
   it must declare ABI-matching `MoveNext`/`get_Current`/`Dispose` or it becomes a real (c)
   gap. `EnumeratorTest` (declares `GetNext`/`Dispose`, in a different file) is unaffected by
   the corpus, but the IL author must re-run this analysis for their new fixtures.
