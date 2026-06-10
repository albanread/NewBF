# Comptime metaprogramming v2 — comptime **method** + **attribute** reflection → attribute/method-driven codegen

> Status: design (implementation-ready). Wave-4 feature. The direct sequel to
> [`comptime-reflection.md`](comptime-reflection.md) (CR, wave-3 — **fields-only**
> comptime reflection), composing it with [`custom-attributes.md`](custom-attributes.md)
> (CA — the `AttributeInfo` metadata, landed) and [`reflection.md`](reflection.md)
> (the `MethodInfo` metadata, landed). All `file:line` anchors below were
> **re-verified against the live tree** at the §95 wave (`fc18a6d`); the code lives
> under `NewBF/src/<crate>/…`. Re-grep before editing — the `try_lower_emit_type_body`
> wall and `emit_metadata` neighbours drift a few lines per commit.
>
> **Revision note (hardened after adversarial review).** Three reviews converged on one
> real defect the original draft denied: a **method/field asymmetry**. The
> `[Comptime, EmitGenerator]` generator method is itself an ordinary body-having method,
> so it IS recorded into the reflected method set (verified — there is **no** comptime
> filter in either method-registration site, §2.5), and an emitted *method* member
> re-enters that set next round. Field reflection is structurally immune (a generator is
> a *method*, not a field; an emitted *method* is never a field). Consequences carried
> through this revision: (1) §4.1's old `expect: 2` was wrong (raw count = 3, and
> self-referentially non-convergent); (2) the "no sema edit for v1" headline is **false**
> for the method axis — v1 needs one small, real lowering filter (now **CMV2-T1.5**,
> §2.5/§3.2/§8); (3) the method-count marquee is restructured so the generator and its
> emitted member are **not** in the counted set; (4) T1's *attribute* half is **not** a
> mechanical mirror of the FieldInfo pin (the precedent hand-builds an 8-field `Type` via
> `TypeMeta::new`, which hard-codes empty attributes — §3.3); (5) the run-corpus path is
> the absolute `e:/NewBF/beef-tests/run-corpus/`, a sibling of the inner workspace, not
> under it. The **attribute axis (§4.3/§4.4) is sound and ship-ready as-is** and is now
> the spine of v1; the method axis is gated behind the CMV2-T1.5 filter.

---

## 1. Overview & the v1 capability

Wave-3's comptime-reflection (CR) lifted the literal-only `Compiler.EmitTypeBody`
wall so a `[Comptime, EmitGenerator]` generator can build a `String` from reflected
**field** metadata and emit it as source — but CR §5 explicitly **deferred** comptime
*method* and *attribute* reflection, and §5/§8 of custom-attributes deferred the
"`[Comptime]` reading a decl's attributes" composition (the
[journal §131](../journals/2026-05-31.md) "attribute-driven codegen" merge). That
deferral is now **almost** a pure coverage gap: the wave-3 work landed the *complete
runtime read-side* for methods AND attributes — `MethodInfo`
(`newbf-llvm/src/lower.rs:515-539`, RF-T7) and `AttributeInfo`
(`newbf-llvm/src/lower.rs:551-612`, CA-T4) arrays are emitted into **every** module
including the sandbox clone, the corlib accessors all ship
(`Type.GetMethod`/`GetMethodCount` `Type.bf:81-90,:56`;
`Type.GetCustomAttribute`/`GetCustomAttributeCount` `Type.bf:105-114,:96`;
`MethodInfo.*` `MethodInfo.bf:28-33`; `AttributeInfo.*` `AttributeInfo.bf:35-60`),
and all of it is proven at runtime (`reflect_method_count.bf → 2`,
`attr_int_arg.bf → 42`, `attr_str_arg.bf → 1`).

**The one exception — and the only lowering change in v1 — is the method axis.** The
generator method is an ordinary method; it (and any method emitted this round) is
recorded into the reflected method set (§2.5, verified). So "count my methods" reads a
count inflated by the generator and perturbed by the emitted member. Closing the method
axis honestly requires a small, real sema edit (**CMV2-T1.5**): exclude
`module.comptime` (i.e. `[Comptime]`) methods from `MethodMeta`. The attribute axis
needs **no** lowering change (attributes are type-level brackets; a generator method and
emitted members add no attributes — proven, §4.3/§4.4 run green verbatim).

**v1 capability (one paragraph).** Lift the CR fields-only boundary so a
`[Comptime, EmitGenerator]` generator can additionally (a) read
`typeof(T).GetMethodCount()` and bind `MethodInfo m = typeof(T).GetMethod(i)` then
`m.GetName()` (**method reflection** — emit a name/arity-driven visitor or dispatch
from the *non-comptime* method set, after CMV2-T1.5 excludes the generator), and (b) read
`typeof(T).GetCustomAttributeCount()` and bind
`AttributeInfo a = typeof(T).GetCustomAttribute(i)` then `a.GetTypeId()` /
`a.GetIntArg(j)` / `a.GetStrArg(j)` (**attribute reflection** — *attribute-driven
codegen*: emit a serialization/column member from a type's `[Serialize]`/`[Column(n)]`
attributes), all **at compile time** in the emission sandbox JIT, build a `String`,
and pass it to `Compiler.EmitTypeBody(...)`. The enabling insight (verified, §2.1): the
EmitTypeBody seam already carries an **arbitrary computed `String`**
(`newbf-sema/src/lower.rs:10643-10698`, CR-T0's `Ref(String)` path) — it does not care
whether the bytes came from field, method, or attribute reflection — and the metadata
is **already in the sandbox clone** (`emit_module` calls `emit_metadata`
unconditionally). So v1 adds **no new IR, no new ABI, no new metadata, no new sandbox**,
and **one small sema filter on the method axis only** (CMV2-T1.5). It is the symmetric
sibling of CR-T3/CR-T4 (count/name marquees) for the attribute axis (no lowering change),
plus that method-axis filter, plus the CR-T1-style sandbox pin proving a
`MethodInfo`/`AttributeInfo` value-struct return runs inside `$ct_emit_run`.
**Concrete, non-generic types only; generic-T reflection (a generator inside `List<T>`
reflecting `typeof(T)`) is the one genuinely-hard deferred lift (§5, §7).**

The v1 marquee (the run-corpus proofs, §4):
- (A-arg, the headline) a `[Reflect, Column(42)]` class whose generator reads
  `typeof(Self).GetCustomAttribute(0).GetIntArg(0)` and emits a member returning `42` —
  *attribute-driven codegen*, the headline use the wave names. **No lowering change.**
- (A-id) a `[Reflect, Marker]` class whose generator reads
  `typeof(Self).GetCustomAttribute(0).GetTypeId()` and emits a runtime id-compare.
  **No lowering change.**
- (M) a method marquee that, **after CMV2-T1.5**, reflects the *non-comptime* method
  count of a **separate** `[Reflect(.Methods)]` target (so neither the generator nor the
  emitted member pollutes the counted set — §4.1) and a method-NAME path (§4.2).

---

## 2. Representation / ABI / IR changes — and the `sema ⊥ llvm` contract

**The headline: nothing new is *represented*. There is no new IR instruction, no new
ABI, no new metadata, no `%struct.Type` change.** Every seam this feature needs was
landed in wave 3. The **only** lowering edit is a one-predicate filter on the method
axis (CMV2-T1.5, §2.5). The attribute axis is **new run-corpus programs + sandbox unit
tests** only — no code change.

### 2.1 No new IR — the three seams already carry everything

1. **The `EmitTypeBody` seam is feature-complete.** `try_lower_emit_type_body`
   (`newbf-sema/src/lower.rs:10598-10711`) accepts a literal fast-path (`:10625-10641`)
   **and an arbitrary runtime `Ref(String)`** (`:10643-10698`): it lowers the arg once
   (`:10648`), reads `text.Ptr()` (→ `char8*`) and `text.Length()` (→ i64) via the
   methods-table lookup (`structs.methods[string_id]["Ptr"]/["Length"]`,
   `:10661-10676`), narrows the length with `self.coerce(len64, len_sig.ret, IrType::I32)`
   (`:10690`), and calls `__newbf_ct_emit(owner, ptr, i32 len)` (`:10691-10695`).
   Anything else emits the loud `__newbf_ct_emit_error` marker (`:10706-10709`, the
   `emit_ct_emit_error` helper `:10721`) — never a silent decline. **This carries a
   String built from method/attribute reflection byte-for-byte, unchanged.**

2. **The metadata is fully emitted, in the sandbox too.** `emit_metadata`
   (`newbf-llvm/src/lower.rs:399`) emits, for every type in `ir.type_meta`:
   - the `MethodInfo` array (`{name, symbol, paramCount}`, `:515-539`) when
     `policy.has(METHODS) && !methods.is_empty()`, pointed at by `mMethods` (Type
     field 7);
   - the `AttributeInfo` array (`{attrTypeId, argCount, args}`, `:551-612`) with the
     uniform `[n x i64]` arg encoding (int/bool→i64; string→`ptrtoint` of the `.rodata`
     cstr, `:572-582`) when `!tm.attributes.is_empty()`, pointed at by `mAttributes`
     (Type field 9); `tm.attributes` is already FIELDS-gated by sema (§2.2) so no extra
     policy check;
   - null `mMethods`/`mAttributes` + count 0 when stripped (the strip differential).
   The aggregate type bodies are at `lower.rs:413-441` (`%struct.Type` `:413-428`,
   `%struct.MethodInfo` `:433-434`, `%struct.AttributeInfo` `:440-441`); the per-type
   `Type` global carrying `mMethods`@7/`mAttrCount`@8/`mAttributes`@9 is at `:616-646`.
   `emit_module` calls `emit_metadata` **unconditionally**, so the **sandbox clone holds
   all of it** (the CR §3.1 insight, re-confirmed: the sandbox is built by
   `run_generators` → `OrcJit::from_ir` over `module.clone()`, `emit.rs:533,543`).

3. **The corlib read API is plain Beef** — `Type.GetMethod(i)`/`GetMethodCount()`
   (`Type.bf:81-90,:56`), `Type.GetCustomAttribute(i)`/`GetCustomAttributeCount()`
   (`Type.bf:105-114,:96`), `MethodInfo.GetName/GetSymbol/GetParamCount`
   (`MethodInfo.bf:28-33`), `AttributeInfo.GetTypeId/GetArgCount/GetIntArg/GetStrArg`
   (`AttributeInfo.bf:35-60`) — byte-identical to the emitted aggregates. They lower and
   JIT-resolve in the sandbox exactly as in the app JIT (the runtime `reflect_method_*` /
   `attr_*` tests prove the app-JIT side; the sandbox uses the *same* `OrcJit::from_ir`
   path, §3.3 pins it).

### 2.2 The metadata population (sema) — already methods + attributes

`assign_type_ids_and_meta` (`newbf-sema/src/lower.rs:5098`, signature
`(structs: &StructTable, m: &mut Module)` — already takes `&mut Module`, the seam
CMV2-T1.5 uses) already populates:
- `methods: Vec<MethodMeta>` when `policy.has(METHODS)` (`:5153-5168`), one per overload
  (mangled symbol), **sorted by `(name, symbol)`** (`:5164` — load-bearing for fixpoint
  determinism, §6 R6), `param_count = explicit_param_count(sig)` (`:5047-5054`, subtracts
  the leading `this`). **This loop currently records EVERY method including the
  `[Comptime, EmitGenerator]` generator — CMV2-T1.5 adds the filter here, §2.5.**
- `attributes: Vec<AttrMeta>` when `policy.has(FIELDS)` (`:5181-5196`), resolving each
  raw `(simple_name, args)` via `by_name → StructId → type_id_of → dense attr_type_id`
  (`:5186-5187`), in **source order** (`:5180`), skipping `ATTR_BUILTIN_MARKERS`
  (`:5184` / the const at `:5086-5096`), unresolved names, and value-struct attributes
  (only `StructKind::Ref` classes are in `type_id_of`, `:5101-5103`).

**v1 reads attributes as-is (adds nothing); it adds the one method-axis filter (§2.5).**

### 2.3 The one *scope* decision: what the generator may read + emit

| Axis | Generator may read (in the sandbox) | Generator may emit (source text) | Gate the target type needs |
| --- | --- | --- | --- |
| **method** | `GetMethodCount()`; `MethodInfo m = GetMethod(i)` then `m.GetName()` / `m.GetParamCount()` (the count/index space is the **non-comptime** methods after CMV2-T1.5) | source that **names** methods syntactically (`this.Foo()`, a name table, a nullary-method dispatch); **must not** emit a method that re-enters the reflected method set it just read (§6 R7) | `[Reflect(.Methods)]` (or bare `[Reflect]`/`[AlwaysInclude]`) |
| **attribute** | `GetCustomAttributeCount()`; `AttributeInfo a = GetCustomAttribute(i)` then `a.GetTypeId()` / `a.GetIntArg(j)` / `a.GetStrArg(j)` | a member built from the attribute's **type-id + scalar args** (a column/serialization member) | `[Reflect]`/`[Reflect(.Fields)]` (attributes piggyback the **FIELDS** bit, §2.2; there is **no** ATTRIBUTES bit) |

Three hard sub-rules, all verified:
- **R5 bound-local discipline (the only sharp edge).** `GetMethod(i)`/
  `GetCustomAttribute(i)` return `MethodInfo`/`AttributeInfo` **by value**
  (`IrType::Struct(id)` rvalue). `struct_base` (`newbf-sema/src/lower.rs:10280-10307`)
  accepts a `Struct(id)` only through its **lvalue** first arm (`:10282`); a method-call-
  *result* rvalue has no lvalue, falls to the rvalue arm (`:10299-10304`) which accepts
  only `Ref` (`:10300`), **not `Struct`** → the receiver collapses to undef. So you may
  **not** chain `typeof(T).GetMethod(0).GetName()`; bind `MethodInfo m = …; m.GetName()`
  in **both** the generator code AND the emitted runtime text. (The corpus already
  documents this for `FieldInfo` `comptime_reflect_field_name.bf:13-15`, for
  `MethodInfo` `reflect_method_count.bf:18`, and for `AttributeInfo`
  `attr_int_arg.bf:14-18,:23`.)
- **Emit *names*, never mangled symbols.** `MethodInfo.GetSymbol()`
  (`MethodInfo.bf:30`) is the **mangled** emitted symbol (`sig.full_name`,
  `lower.rs:5159`) — not parseable as a source call. Method-driven codegen emits source
  that *names* methods by `GetName()` (the source name) and re-resolves, exactly as CR
  emits `this.mX` for fields (CR §5 "names members syntactically"). v1 is
  **name+arity-driven** (`param_count` is the only shape reflected — `MethodMeta` carries
  no parameter types/return type, `:5157-5161`), never signature-driven.
- **Never emit a member that re-enters the reflected set it reads (method axis, §6 R7).**
  After CMV2-T1.5 the *generator* is excluded, but an emitted **method** member re-enters
  `type_meta` at round *k+1* and would shift a method-count read. The method-count
  marquee (§4.1) therefore reflects a **separate** target with **no** generator on it
  (the generator lives on a distinct probe class), so the emitted member never enters the
  counted set. (The attribute axis is structurally free of this: an emitted member adds
  no attribute bracket, so the attribute set is invariant — §4.3/§4.4.)

### 2.4 The `sema ⊥ llvm` contract (preserved)

The HARD invariant (newbf-sema must not depend on newbf-llvm) is **preserved
untouched** — CMV2-T1.5 (the method filter) is wholly inside sema
(`assign_type_ids_and_meta` reading `m.comptime`, a sema-owned `Vec<String>`) and adds
**no** cross-crate coupling. The generator reads reflection *through the emitted corlib
`.bf` API in the sandbox*, exactly as CR decided (CR §3.0): no Rust-side reflection
view of `module.type_meta`, no new host shim. The only host shims remain
`__newbf_ct_emit` / `__newbf_ct_emit_error` (carrying text/diagnostic bytes out,
`emit.rs:587`).

| Symbol | Defined by | Referenced by sema (by name/id only) |
| --- | --- | --- |
| `%struct.Type`/`%struct.MethodInfo`/`%struct.AttributeInfo` aggregates | **llvm** `emit_metadata` (`lower.rs:413-441`) | never named in sema; corlib `Type`/`MethodInfo`/`AttributeInfo` struct ids via `by_name` |
| per-type `Type` global (carries `mMethods`@7, `mAttrCount`@8/`mAttributes`@9) | **llvm** `emit_metadata` (`lower.rs:616-646`) | sema emits `GlobalAddr(type_global_name(prefix))` by name (the `typeof` lowering) |
| `MethodInfo`/`AttributeInfo` arrays (`.methodinfo`/`.attrinfo`) | **llvm** `emit_metadata` (`:532`/`:605`) | never named; reached by the corlib accessor's `mMethods[i]`/`mAttributes[i]` index |
| `__newbf_ct_emit` / `__newbf_ct_emit_error` host shims | **newbf-comptime** Rust `extern "C"`, bound absolute (`emit.rs:547,554`) | sema emits the calls by name (`lower.rs:10691,10724`) |

### 2.5 CMV2-T1.5 — the one real lowering edit: exclude comptime methods from `MethodMeta`

**The defect (verified).** A `[Comptime, EmitGenerator] static void Generate()` is an
ordinary body-having method. It is registered into `structs.methods[id]` by the
`Member::Method` arm (`newbf-sema/src/lower.rs:3222-3338`, pushed at `:3335-3337`) with
**no** `[Comptime]`/`[EmitGenerator]` filter (the only `continue`s are for generic
methods `:3235` and uncallable body-less members `:3271`). `MethodSig` (`:5858-5878`)
carries **no** comptime flag. Then `assign_type_ids_and_meta` records a `MethodMeta` for
**every** entry of `structs.methods[i]` (`:5153-5168`) with no attribute skip. So
`typeof(Widget).GetMethodCount()` for `Widget { Area, Width, Generate }` returns **3**,
not 2 — and the existing `reflect_method_count.bf → 2` (`:1-22`) returns 2 **only because
that `Widget` has no generator** (verified: its body is exactly `Area`/`Width`).

**The fix (one predicate, no `MethodSig` change).** `module.comptime` is already a
`Vec<String>` of every `[Comptime]` method's `full_name`, populated during lowering at
`lower.rs:6304` (`m.comptime.push(full_name.clone())` whenever `has_comptime_attr`).
`assign_type_ids_and_meta` already holds `&mut Module`, so build a
`HashSet<&str>` from `m.comptime` once and skip any `sig` whose `full_name` is in it when
pushing `MethodMeta` at `:5155-5162`:

```rust
let comptime_syms: std::collections::HashSet<&str> =
    m.comptime.iter().map(String::as_str).collect();
// … inside the `for (name, sigs)` loop:
for sig in sigs {
    if comptime_syms.contains(sig.full_name.as_str()) { continue; } // CMV2-T1.5
    ms.push(MethodMeta { … });
}
```

This excludes **all** `[Comptime]` methods (which already includes every
`[Comptime, EmitGenerator]` generator — `comptime_emitter_of` requires `[Comptime]`,
`:12896-12898`) from the reflected method set, so a generator never counts itself. It
does **not** address the emitted-this-round member (an emitted *non-comptime* method
re-enters the set at *k+1*) — that is handled at the **example** level (§2.3 R7 / §4.1:
reflect a target with no generator and emit into nothing it counts) and stays a deferred
general hazard (§6 R7).

**Why a name-set, not a `MethodSig.comptime` flag:** the flag would touch the
`MethodSig` struct + every construction site for one read; the name-set is local to the
population loop, O(methods), and reuses data already recorded. Both are correct; the
name-set is smaller and keeps `MethodSig` unchanged.

**Scope honesty:** this is a genuine `newbf-sema/src/lower.rs` lowering change with its
own verify-corpus + unit-test gate. It is the reason the original draft's "no sema edit
for v1" claim is retracted for the method axis. The attribute axis still needs **zero**
lowering change.

---

## 3. The concrete changes, with file:line anchors for every seam

> **The summary up front: the attribute axis needs no lowering change; the method axis
> needs exactly one (CMV2-T1.5, §2.5).** The four read-side seams (EmitTypeBody,
> `emit_metadata`, the metadata population, the corlib API) are all landed and exercised
> by the FIELDS/attribute path. v1 = CMV2-T1.5 (the method filter) + run-corpus programs
> (T2/T3/T4) + sandbox pins (T1). The further generic-T lift (§5) is **deferred**.

### 3.1 Parser — no change

`typeof` already parses to `Expr::TypeOf { ty }`; `[Comptime]`/`[EmitGenerator]`/
`[Reflect(.Methods)]`/user-attribute brackets all parse with the existing attribute
grammar. The generator body is ordinary Beef calling ordinary methods
(`GetMethodCount`, `GetCustomAttribute`, `Append`). **Parser corpus ratchet does not
move.**

### 3.2 Sema (`newbf-sema/src/lower.rs`) — one filter on the method axis (CMV2-T1.5)

The EmitTypeBody relaxation (CR-T0, `:10598-10711`), the policy gates
(`reflect_policy` `:12900`+, METHODS/FIELDS bits), and the metadata population
(`:5098`, methods `:5153-5168`, attributes `:5181-5196`) are **all already in place**.
A method/attribute-reflecting generator routes through the **identical**
`Ref(String) → __newbf_ct_emit` path a field-reflecting generator uses; sema does not
distinguish *what* reflection produced the String.

**The one v1 lowering edit is CMV2-T1.5** (§2.5): exclude `module.comptime` methods from
`MethodMeta` at `:5155-5162`, so the method axis reflects only the user's real methods.
The **attribute axis needs no sema edit.**

> The single *additional* sema edit that would be needed for the **deferred** generic-T
> case is to lift the generic-comptime guard at `record_method_inst` (`:1851-1857`): the
> `… || has_comptime_attr(attributes, mdecl_src) { return; }` refusal (`:1854`) that
> declines to monomorphize a `[Comptime]` generic method. v1 leaves it **as-is** (§5,
> §7).

### 3.3 Comptime (`newbf-comptime/src/emit.rs`) — no code change, one new pin

The sandbox (`run_generators` `:520-580`), the strip (`strip_emitter_and_shim`
`:589-648`, `EMIT_SHIM_SYMBOLS` `:587`), the dedup/round/byte caps, and the
analyze-abort all work **unchanged** for a method/attribute-reflecting generator — the
`$ct_emit_run` wrapper is nullary `void` (`:536-540`) and the generator returns `void`,
communicating only via the shim, so there is **no struct-return/FFI-marshalling
problem** and the `eval_const` struct-return gate (`eval.rs:107-116` — `IrType::Struct`
returns `Unsupported`, the value-fold path) is **not** on this path. The strip keeps the
corlib `Type`/`MethodInfo`/`AttributeInfo`/`String` methods (non-comptime) and drops the
generator + shim.

**T1 is the one net-new test, and its *attribute* half is NOT a mechanical mirror of the
FieldInfo pin.** The cited precedent
`from_ir_sandbox_shaped_value_struct_fieldinfo_return_in_ct_emit_run` (`emit.rs:909`)
hand-builds `%struct.Type` with **only 8 fields** (`mSize..mMethods`, `:919-928`) — it
has **no `mAttrCount`@8 / `mAttributes`@9** — and registers its `TypeMeta` via
`TypeMeta::new(...)` (`:953`), which **hard-codes `attributes: Vec::new()`**
(`newbf-ir/src/module.rs:152-171`, the field at `:169`; `::new` takes no attributes arg).
So the **method** half of T1 *can* mirror it (index `mMethods`@7, already a field), but
the **attribute** half **cannot**: it must (a) extend the hand-built `Type` to the full
10-field layout (add `mAttrCount`@8, `mAttributes`@9 to match `emit_metadata`'s
`lower.rs:413-428` body), (b) build the `TypeMeta` **struct-literally** (not via `::new`)
to set a non-empty `attributes: Vec<AttrMeta>`, (c) index `mAttributes`@9 (not `mFields`@6),
and (d) rely on `emit_metadata` synthesizing the `.attrinfo` array from it. This is
**net-new IR scaffolding**, not "symmetric with the FieldInfo pin" — T1's risk estimate
is raised accordingly (§6 R5, §8). The strip half of T1 (corlib accessors survive, the
generator + shim are gone) does mirror the existing CR-T1 precedent.

### 3.4 LLVM / codegen (`newbf-llvm`) — no change

`emit_metadata` (`:399`) already emits the MethodInfo array (`:515-539`) and the
AttributeInfo array (`:551-612`) for every module including the sandbox clone, in JIT
and AOT identically. **No backend change.** The strip (§3.3) ensures the shims are gone
before final app/AOT codegen.

### 3.5 Corlib (`newbf-corlib/bf`) — read API ships; one optional overload

All read accessors ship (§2.1). The **only** possible corlib add is **T4**: a
`String.Append` overload for whatever the chosen examples Append. `Append(char8*)`
already exists (`String.bf:213`, landed as CR-T2), so a generator that
`Append(m.GetName())` / `Append(a.GetStrArg(0))` (both `char8*`) needs **nothing new**.
`Append(int)`/i64 exists (`String.bf:231`); `GetIntArg` returns `int64`
(`AttributeInfo.bf:43`), so binding to an `int` local and `Append(int)` already works —
an explicit `int64` overload is unnecessary. **T4 is recorded only for honesty; likely a
no-op.**

---

## 4. Worked examples (run-corpus programs that prove it)

Each is a self-contained `Program.Main() -> int32` with `// expect: N`, dropped in the
**absolute** `e:/NewBF/beef-tests/run-corpus/` directory (a **sibling** of the inner
workspace `e:/NewBF/NewBF/`, **not** under it — the harness resolves
`CARGO_MANIFEST_DIR/../../../beef-tests/run-corpus`,
`tests/newbf-tests/tests/run_corpus.rs:20`). Run by the JIT full-i32 harness under the
Stomp guard (`set_guard_mode(GuardMode::Stomp)` `:89`, before the corpus loop; the
harness never calls `report_leaks` `:114`). All compose with the existing
`reflect_method_*` / `attr_*` / `comptime_reflect_*` corpus. Every example keeps probe
values ≤255 (the AOT exit-code truncation note in MEMORY) even though the JIT harness
reads the full i32.

**The attribute axis (§4.3/§4.4) is the spine and needs no lowering change. The method
axis (§4.1/§4.2) is gated behind CMV2-T1.5.**

### 4.1 `comptime_reflect_method_count.bf` — the method marquee (**expect: 2**, after CMV2-T1.5)

The headline pitfall the original draft missed: if the generator lived on the **same**
class it reflects, the count would include the generator (3, not 2) and the emitted
member would re-enter the set (→ 4, non-convergent, §6 R7). So this program **separates**
the generator from the reflected target: a `Probe` class carries the generator and
reflects a **distinct** `Widget` that has **no** generator on it. After CMV2-T1.5 (which
excludes the generator from any count anyway), `Widget`'s reflected non-comptime method
set is exactly `{Area, Width}` → **2**. The generator emits its member onto `Probe`
(which is not method-counted by anyone), so nothing it emits perturbs the reflected
`Widget` count. Reuses `Append(int)` + literal auto-wrap (no T4). Note `int n` (i64
widening) so `Append(int)` (decimal) is the unambiguous overload — a bare `int32` ties
`Append(char8)` and emits a char code (the CR-T3 gotcha,
`comptime_reflect_field_count.bf:11-15`).

```beef
// expect: 2
[Reflect(.Methods)]
class Widget {
    public int32 Area()  { return 1; }
    public int32 Width() { return 2; }
}
// The generator lives on a SEPARATE probe class so neither it (excluded by
// CMV2-T1.5) nor its emitted member is in Widget's reflected method set.
class Probe {
    [Comptime, EmitGenerator]
    public static void Generate() {
        // Reflect at COMPILE TIME the non-comptime method count of a DIFFERENT
        // type (Widget). typeof(Widget) is a Ref(Type) rvalue → GetMethodCount()
        // resolves directly (no value-struct chain). The Type global lives in the
        // sandbox. After CMV2-T1.5 the count excludes any [Comptime] method.
        int n = typeof(Widget).GetMethodCount();        // 2 (widened to int=i64)
        String s = new String("public int32 MethodCount() { return ");
        s.Append(n);                                    // "...return 2" (Append(int))
        s.Append("; }");                                // literal auto-wraps to String
        Compiler.EmitTypeBody(s);                       // runtime String, NOT a literal
        delete s;                                       // exactly once → no double-free
    }
}
class Program {
    public static int32 Main() {
        Probe p = new Probe();
        int32 r = p.MethodCount();                      // the EMITTED member returns 2
        delete p;
        return r;
    }
}
```

> Without CMV2-T1.5 this returns 2 only if `Widget` is generator-free (it is here) —
> but the *robust* guarantee that "a generator never inflates its own type's count" is
> exactly what CMV2-T1.5 provides, so an idiomatic self-reflecting generator (generator
> on the reflected class) also works. The separate-probe form additionally dodges the R7
> emitted-member hazard, which CMV2-T1.5 alone does **not** fix.

### 4.2 `comptime_reflect_method_name.bf` — method-NAME-driven emission (**expect: 1**)

The sibling of CR-T4's name path. The generator **binds a `MethodInfo` local** (R5),
reads `m.GetName()` (→ `char8*`, `Append(char8*)` at `String.bf:213`), and emits a
predicate that re-derives the same name at runtime and `StrEq`s it. Both the generator
code AND the emitted text bind a `MethodInfo` local before `.GetName()`. After CMV2-T1.5,
`Shape`'s reflected non-comptime methods are `{Area, Width}` sorted `(name, symbol)`, so
`GetMethod(0)` names **`"Area"`** (alphabetically first of the two). The generator lives
on a **separate** `ShapeGen` probe and emits its predicate there, so the emitted member
`FirstMethodIsArea` never enters `Shape`'s reflected set — `GetMethod(0)` is invariant
across rounds. (This is also why the value is robust, not alphabetical luck: index 0 is
pinned to the first of a **stable two-element set**, not "first of whatever happens to be
present this round".) Depends on **`Append(char8*)` only**, which already exists.

```beef
// expect: 1
[Reflect(.Methods)]
class Shape {
    public int32 Area()  { return 1; }
    public int32 Width() { return 2; }
}
class ShapeGen {
    [Comptime, EmitGenerator]
    public static void Generate() {
        // Emitted method binds a MethodInfo LOCAL (not a chained rvalue, R5),
        // re-derives Shape's first method name at RUNTIME, and StrEqs it against
        // the literal the generator read at COMPILE TIME — both must be "Area".
        // Shape's reflected set is the stable {Area, Width} (no generator on Shape,
        // and CMV2-T1.5 would exclude one anyway), so GetMethod(0) is "Area".
        String s = new String(
            "public bool FirstShapeMethodIsArea() { MethodInfo m = typeof(Shape).GetMethod(0); return Internal.StrEq(m.GetName(), \"");
        MethodInfo gm = typeof(Shape).GetMethod(0);     // generator-side: bind a local too
        s.Append(gm.GetName());                         // Append(char8*) — reflected name "Area"
        s.Append("\"); }");
        Compiler.EmitTypeBody(s);
        delete s;
    }
}
class Program {
    public static int32 Main() {
        ShapeGen g = new ShapeGen();
        bool ok = g.FirstShapeMethodIsArea();
        delete g;
        return ok ? 1 : 0;
    }
}
```

### 4.3 `comptime_reflect_attr_typeid.bf` — the attribute marquee (**expect: 1**, no lowering change)

Verified to run green verbatim through the real `run_emission` + JIT pipeline. The
generator binds `AttributeInfo a = typeof(Job).GetCustomAttribute(0)` (R5), reads
`a.GetTypeId()`, and emits a member that compares it to `typeof(Marker).GetTypeId()` at
runtime. `Marker` must be a **class** (v1 attribute = class, so it has a dense type-id;
a value-struct attribute surfaces zero, §5). `Job` is `[Reflect]` so the FIELDS bit
surfaces attributes (§2.2/§2.3). The attribute set is **invariant** under emitting a
method (the generator and emitted members add no attribute bracket), so this is
convergent with **no** method-axis caveat. Uses only `Append(int)` + auto-wrap (no T4).

```beef
// expect: 1
class Marker : Attribute { public this() { } }
[Reflect, Marker]
class Job {
    public int32 mX;

    [Comptime, EmitGenerator]
    public static void Generate() {
        // Reflect the FIRST attribute's dense type-id at COMPILE TIME (bind a
        // local, R5 — never chain off the by-value AttributeInfo rvalue).
        AttributeInfo a = typeof(Job).GetCustomAttribute(0);
        int id = a.GetTypeId();                          // Marker's dense type-id (int32 → int)
        // Emit a member returning 1 iff that id equals Marker's id at runtime.
        // GetTypeId() returns int32 and `id` is rendered as a small dense decimal,
        // so the emitted `int32 == <literal>` compares in i32 range (dense ids are
        // small — NOT a general int64 pattern).
        String s = new String(
            "public bool HasMarker() { AttributeInfo a = typeof(Job).GetCustomAttribute(0); return a.GetTypeId() == ");
        s.Append(id);                                    // the compile-time-read id
        s.Append("; }");
        Compiler.EmitTypeBody(s);
        delete s;
    }
}
class Program {
    public static int32 Main() {
        Job j = new Job();
        bool ok = j.HasMarker();                         // EMITTED: first attr IS Marker
        delete j;
        return ok ? 1 : 0;
    }
}
```

### 4.4 `comptime_reflect_attr_arg.bf` — attribute-ARG-driven codegen (the ORM/serialization seed) (**expect: 42**, no lowering change)

The literal "attribute-driven codegen" the wave names — the **headline of v1**, verified
to run green verbatim. A `[Reflect, Column(42)]` class generator binds
`AttributeInfo a = typeof(Row).GetCustomAttribute(0)` (R5), reads `a.GetIntArg(0)`
(→ `int64`, bind to an `int` local), and emits a member returning it — directly reusing
`attr_int_arg.bf`'s runtime proof (`→ 42`), now in the **sandbox**. This is the seed for
emitting a real serializer from `[Column(n)]`/`[Serialize("name")]` attributes. `Column`
is a class; `Row` is `[Reflect]`. `42 ≤ 255` (MEMORY). The attribute set is invariant
under emission, so no method-axis caveat applies.

```beef
// expect: 42
class Column : Attribute { public this(int32 n) { } }
[Reflect, Column(42)]
class Row {
    public int32 mX;

    [Comptime, EmitGenerator]
    public static void Generate() {
        // Read the attribute's first scalar ctor arg at COMPILE TIME (bind a local,
        // R5). GetIntArg returns int64; bind to int so Append(int) renders decimal.
        AttributeInfo a = typeof(Row).GetCustomAttribute(0);
        int col = (int)a.GetIntArg(0);                   // 42 (the folded ctor arg)
        String s = new String("public int32 ColumnIndex() { return ");
        s.Append(col);                                   // "42"
        s.Append("; }");
        Compiler.EmitTypeBody(s);
        delete s;
    }
}
class Program {
    public static int32 Main() {
        Row r = new Row();
        int32 v = r.ColumnIndex();                       // the EMITTED member returns 42
        delete r;
        return v;
    }
}
```

> A `GetStrArg`-driven variant (`comptime_reflect_attr_strarg.bf`, **expect: 1**) is the
> obvious fifth program: `[Reflect, Table("users")]` → the generator
> `Append(a.GetStrArg(0))` (a `char8*`, `Append(char8*)` exists) into the emitted text's
> nested literal, emitting a `StrEq(GetStrArg(0), "users")` predicate. Same shape as
> §4.2's name path. Recorded under T3 as an optional extension.

These pin: (A-arg) attribute *scalar args* drive emitted code (the marquee, no lowering
change); (A-id) attribute type-ids reach the sandbox (no lowering change); (M-count)
non-comptime method count reaches the sandbox (after CMV2-T1.5); (M-name) method *names*
flow into emitted text. Plus the existing `comptime_emit_member.bf` (literal path,
**expect: 42**) and `comptime_reflect_field_count.bf` (**expect: 2**) stay green (the
back-compat + CR-fields gates), and `reflect_method_count.bf → 2` must stay green under
CMV2-T1.5 (its `Widget` has no comptime method, so the filter is a no-op for it — the
control case).

---

## 5. v1 scope vs explicitly-deferred (honest, esp. the hard parts)

**v1 (this design):**
- One lowering edit: **CMV2-T1.5** — exclude `[Comptime]` methods from the reflected
  method set (§2.5). The attribute axis needs no lowering edit.
- A `[Comptime, EmitGenerator]` generator on a **concrete, non-generic** type may call:
  - `typeof(T).GetMethodCount()`; `MethodInfo m = typeof(T).GetMethod(i)` (**binding a
    local**, not chaining) then `m.GetName()` / `m.GetParamCount()` — over the
    **non-comptime** method set;
  - `typeof(T).GetCustomAttributeCount()`; `AttributeInfo a = typeof(T).GetCustomAttribute(i)`
    (**binding a local**) then `a.GetTypeId()` / `a.GetIntArg(j)` / `a.GetStrArg(j)`;
  - build text with `String` (`Append(int/String/char8/bool/char8*)`) and emit it via
    `Compiler.EmitTypeBody` (the landed CR-T0 `Ref(String)` path).
- Method reflection needs the target type `[Reflect(.Methods)]` (or bare
  `[Reflect]`/`[AlwaysInclude]`); attribute reflection needs `[Reflect]`/`[Reflect(.Fields)]`
  (attributes piggyback the FIELDS bit). Attribute classes must be **classes** (dense id).
- Emitted text **names** methods syntactically and is **name+arity-driven** (`param_count`
  only), never mangled-symbol or signature-driven. Reflects only **pre-existing** declared
  members; on the method axis, **must not** emit a member that re-enters the reflected
  method set it read (§2.3 R7; the §4.1/§4.2 examples emit onto a separate probe class).

**Deferred (the hard parts called out honestly):**
- **Generic-T reflection — THE hard one.** A `[Comptime]` generic generator inside
  `List<T>` reflecting `typeof(T)`. Blocked by **two** seams: (a) the generic-comptime
  guard at `record_method_inst` (`newbf-sema/src/lower.rs:1851-1857`, the
  `has_comptime_attr` refusal at `:1854`) refuses to monomorphize a `[Comptime]` generic
  method ("legal Beef the corlib relies on — only our v1 lowering can't
  instantiate-and-fold it", `:1849-1850`); and (b) `typeof(generic-T)` itself is deferred
  in reflection.md §10 (`lower_typeof` falls to `__newbf_type_unknown` for an unresolved
  generic param). Lifting it means monomorphizing the comptime generator per type-arg and
  running each monomorph in the sandbox — touching the monomorph keying
  (`record_method_inst`'s `GenMKey`), emit-job recording for generic owners (a template
  context "records the job but skips the body rewrite"), and `typeof` resolution for a
  bound `T`. **This is the gnarly case; keep it out of v1** (§7 states the precise
  dependency). It is NOT blocked by generic-interface monomorphization.
- **Reflecting emitted-this-round members (the R7 hazard — only partially solved).**
  CMV2-T1.5 excludes the *generator* from the reflected method set, but an emitted
  **non-comptime method** member re-enters `type_meta` at round *k+1* and would shift a
  later method-count read → non-convergence. v1 handles this only at the example level
  (emit onto a separate non-reflected probe, §4.1/§4.2); a *general* "emit-and-reflect on
  the same method set" capability is deferred (§6 R7).
- **Signature-driven codegen.** Reflecting parameter *types* / return type to emit
  type-correct forwarding/proxies. `MethodMeta` carries only `{name, symbol, param_count}`
  (`lower.rs:5157-5161`) — a metadata extension, deferred.
- **Value-struct attribute types.** Idiomatic Beef `struct` attributes have no dense
  type-id (`lower.rs:5101-5103` — only `StructKind::Ref`), so a generator reading them
  sees zero attributes silently (inherited CA §8 constraint).
- **Constructed attribute instances** (`GetCustomAttribute<T>() -> T`). The sandbox
  can't return a struct on the value-fold path (`eval.rs:107-116`); irrelevant on the
  void+shim emission path but still unbuilt (CA §5).
- **Member/parameter-level attribute reflection.** v1 is type-level only
  (`FieldDef.attributes`/`MethodDef.attributes` exist in the def-graph but are unsurfaced,
  CA §5).
- **Float attribute args** — the JIT `__real@` constant-pool gap (MEMORY); CA already
  defers them.
- **A dedicated `ReflectPolicy::ATTRIBUTES` bit** — v1 piggybacks FIELDS (CA §3.2).

---

## 6. Load-bearing risks & mitigations

1. **R5 — the value-struct method-chain trap (highest-probability bug; already
   understood).** `GetMethod(i)`/`GetCustomAttribute(i)` return `MethodInfo`/
   `AttributeInfo` **by value** (`IrType::Struct(id)` rvalue). `struct_base`
   (`newbf-sema/src/lower.rs:10280-10307`) accepts a `Struct(id)` only via its lvalue
   first arm (`:10282`); a method-call-result rvalue falls to the rvalue arm
   (`:10299-10304`), which accepts only `Ref` (`:10300`) → undef receiver. *Mitigation
   (the established pattern):* bind `MethodInfo m = …; m.GetName()` /
   `AttributeInfo a = …; a.GetIntArg(0)` in **both** the generator code and the emitted
   runtime text. Pinned by the existing corpus (`reflect_method_count.bf:18`,
   `attr_int_arg.bf:23`) and the §4 examples. **Not new risk** — a known discipline to
   spell into every example.
2. **R-METHODFILTER — comptime methods inflate the reflected method count (the central
   review finding; FIXED by CMV2-T1.5).** A `[Comptime, EmitGenerator]` generator is an
   ordinary body-having method recorded into `structs.methods[id]` (`:3335-3337`) and
   thence into `MethodMeta` (`:5153-5168`) with **no** filter — verified. Without the fix,
   `GetMethodCount()` on a class carrying a generator is off by one (or more) and an
   emitted method member makes it non-convergent. *Mitigation:* **CMV2-T1.5** (§2.5)
   excludes `module.comptime` symbols at the `MethodMeta` push; the §4.1/§4.2 examples
   additionally emit onto a **separate probe** so no emitted member re-enters the counted
   set. Acceptance: `reflect_method_count.bf → 2` (control, generator-free) stays green
   AND a new unit test asserts a `[Reflect(.Methods)]` class **carrying** a generator
   counts only its non-comptime methods.
3. **`GetSymbol()` is the mangled symbol, not a source name** (`MethodInfo.bf:30` =
   `sig.full_name`, `lower.rs:5159`). Emitting `m.GetSymbol()` into source text would not
   parse as a call. *Mitigation:* method-driven codegen emits source that *names* methods
   via `GetName()` and re-resolves (§2.3), never raw symbols — the CR §5 "names members
   syntactically" boundary.
4. **Attribute reflection is class-only + FIELDS-gated** (two inherited CA constraints).
   (a) Only `StructKind::Ref` attribute classes get a dense id (`lower.rs:5101-5103`) — a
   `struct` attribute surfaces zero silently; (b) attributes are gated by the **FIELDS**
   bit (`:5181`), not a dedicated bit. *Mitigation:* §4 examples pin the attribute class as
   a `class` and mark the annotated type `[Reflect]`; the design states this in §2.3/§5.
   A generator reading attributes on a `[Reflect(.Methods)]`-only type sees zero (FIELDS
   unset) — called out in the task seeds.
5. **Memory under the Stomp guard (inherited, well-trodden).** The generator's
   `new String` object body routes through `newbf_alloc` → the Stomp ledger *during
   compilation* (run-corpus runs the sandbox under `GuardMode::Stomp`,
   `run_corpus.rs:89`). Acceptance is **`delete s` exactly once → no double-free** (a
   double-free faults the compiler; a pure leak is tolerated — the harness never calls
   `report_leaks`, `:114`). The `char8*` buffer is CRT malloc/free, not guard-tracked.
   Identical to CR-T3/CR-T4 — no new hazard. *Mitigation:* every §4 generator `delete`s
   exactly once; T2/T3 assert no double-free under Stomp (NOT "balance").
6. **Sandbox completeness for struct-by-value method/attr returns — T1 is NOT a
   mechanical mirror on the attribute side.** The existing `reflect_method_*` / `attr_*`
   tests prove the read-side in the **app** JIT; the only thing unpinned is a
   `MethodInfo`/`AttributeInfo` value-struct return inside the `$ct_emit_run` **sandbox**
   wrapper. The FieldInfo precedent (`emit.rs:909`) hand-builds an **8-field** `Type` via
   `TypeMeta::new` (empty attributes, `module.rs:169`) and indexes only `mFields`@6.
   *Mitigation:* **T1** must (method half) mirror the precedent indexing `mMethods`@7, and
   (attribute half) **extend** the hand-built `Type` to the full **10-field** layout
   (`mAttrCount`@8/`mAttributes`@9), build `TypeMeta` **struct-literally** with non-empty
   `attributes`, and index `mAttributes`@9. **Raised risk** vs the original "low, the
   FieldInfo pin already passes" framing — the attribute half is net-new scaffolding.
7. **Fixpoint determinism / dedup convergence.** Method iteration is `(name, symbol)`-
   sorted (`lower.rs:5164`); attribute order is source order (`:5180`) — both stable
   round-to-round, so the `seen` dedup (`emit.rs`) converges. *Mitigation:* §4 generators
   emit a **single idempotent member** from a stable reflection read (the property
   CR-T3 already satisfies). The `MAX_EMIT_ROUNDS`+byte-cap+dedup guard is inherited.
8. **Reflecting emitted-this-round members (a real fixpoint-ordering hazard — only
   partially solved by CMV2-T1.5).** CMV2-T1.5 removes the *generator* from the reflected
   method set, but a member emitted in round *k* still enters `type_meta` at *k+1*; a
   generator that reflects a method count AND emits a method onto the **same** reflected
   type would see a shifting set → non-convergence. *Mitigation:* v1 emits onto a separate
   probe class (§4.1/§4.2), so the reflected count is over a generator-free,
   emission-free type. A general same-set emit-and-reflect is deferred (§5).
9. **The genuinely-hard deferred part — generic-T reflection.** Lifting the
   `record_method_inst` `[Comptime]`+generic guard (`:1851-1857`) + `typeof(generic-T)`
   (reflection.md §10) is real engineering (monomorph keying, emit-job recording for
   generic owners, bound-`T` typeof). *Mitigation:* **keep it out of v1** and state the
   dependency precisely (§5, §7); v1's concrete-type examples need none of it. NOT blocked
   by generic interfaces.
10. **SSA dominance / `sema ⊥ llvm`.** CMV2-T1.5 is a sema-local filter reading a
   sema-owned `Vec<String>` (`m.comptime`); it adds no SSA surface and no cross-crate
   edge. The generator body is straight-line Beef; `typeof` is a constant `GlobalAddr`.
   *Mitigation:* the verify-corpus LLVM-clean ratchet remains the gate, and the
   generator's reparsed members go through the normal lowering path.

---

## 7. Cross-feature dependency (generic-interface monomorphization)

**v1 has NO dependency on generic-interface monomorphization.** Method/attribute
reflection reads **value-struct metadata** (`MethodInfo`/`AttributeInfo` are value
structs over `.rodata` constants) through `typeof(T)` (a constant `GlobalAddr`) — no
interface dispatch, no itable, no `emit_iface_dispatch`, no generic-interface monomorph
anywhere on the path. CMV2-T1.5 (the method filter) is likewise a plain sema-side
metadata edit with no interface coupling. This is categorically different from the
wave's other candidates (iterators-lazy's `IEnumerable<T>`/`IEnumerator<T>`, delegates'
generic `Action<T>`, generic-constraints' `T : IEnumerator<TElement>` — all of which
*do* need generic interfaces).

The **only** generic coupling for this feature is **generic-T reflection** (a generator
inside `List<T>` reflecting `typeof(T)`), and that is blocked **not** by generic
interfaces but by:
- the generic-comptime guard at `record_method_inst` (`newbf-sema/src/lower.rs:1851-1857`),
  and
- `typeof(generic-T)` resolution (reflection.md §10 — `lower_typeof` sentinel fallback).

**Precise statement for the sprint sequencer:**
- **v1 (no generic-interface, no generic-T) — ships independently:** reflect methods
  (after CMV2-T1.5) + attributes on a **concrete** `[Reflect(.Methods)]` / `[Reflect]`
  class, emit name/arity-driven dispatch + attribute-driven members. Consumes nothing
  from generic-interfaces; provides nothing to it. Can land in **any** wave-4 slot,
  parallel to generic-interfaces.
- **Deferred (needs the generic-comptime-guard lift + bound-`T` typeof, NOT generic
  interfaces):** reflect a generic param `T` inside a generic `[Comptime]` generator.
  This is a **comptime-metaprogramming-v3** item, sequenced after generics/typeof work,
  independent of generic-interfaces.

So: this feature is a **leaf** in the wave-4 dependency graph — it neither blocks nor is
blocked by generic-interface monomorphization. (The two hard sibling features the wave is
sequencing — lazy-yield's cross-yield state machine and the generic-interface itable lift
— are **not** in this doc and are correctly out of scope here.)

---

## 8. Task breakdown

Ordered; each task is agent-assignable with a one-line seed + a concrete acceptance
gate. **Naming:** `CMV2-T<n>` (comptime-metaprogramming-v2). The attribute axis (T3) is
behavior-additive (corpus programs over landed machinery); the method axis carries the
**one real lowering keystone, CMV2-T1.5**. The critical path is
**CMV2-T0 → CMV2-T1 → CMV2-T1.5 → {CMV2-T2 ∥ CMV2-T3} → CMV2-T5** (T3 can start as soon
as T1 lands; T2 needs T1.5).

**CMV2-T0 — Audit + confirmation pass (the "is it really already built?" gate).**
Seed: in a single test/dump pass, confirm (don't change) that `emit_metadata` emits the
MethodInfo array (`newbf-llvm/src/lower.rs:515-539`) and AttributeInfo array (`:551-612`)
into a **sandbox-shaped** `from_ir` module, and that the corlib `Type.GetMethod` /
`Type.GetCustomAttribute` lower/JIT-resolve; record the exact `(name, symbol)` method
sort and the FIELDS-gates-attributes fact; **explicitly verify the method-count
inflation** by reading `GetMethodCount()` on a `[Reflect(.Methods)]` class that carries a
`[Comptime, EmitGenerator]` method and confirming it counts the generator (the motivation
for T1.5).
Accept: a dump/assert shows a sandbox-shaped module carrying non-null `mMethods`
(for `[Reflect(.Methods)]`) and `mAttributes` (for `[Reflect, Attr]`); the inflation is
reproduced; **all existing corpora unchanged**. *Behavior-preserving; de-risks the
premise + motivates T1.5.*

**CMV2-T1 — Sandbox method/attribute value-struct-return pin (net-new test; HARD gate —
attribute half is net-new scaffolding, NOT a mirror).**
Seed: in `newbf-comptime/src/emit.rs` tests, (method half) mirror the FieldInfo pin
`from_ir_sandbox_shaped_value_struct_fieldinfo_return_in_ct_emit_run` (`:909`) for
`MethodInfo` — index `mMethods`@7, bind a local, read `GetName()`. (attribute half — net
new) **extend** the hand-built `%struct.Type` to the full **10-field** layout (add
`mAttrCount`@8, `mAttributes`@9, matching `emit_metadata` `lower.rs:413-428`), build the
`TypeMeta` **struct-literally** (not `TypeMeta::new`, which hard-codes
`attributes: Vec::new()`, `module.rs:169`) with a non-empty `attributes`, index
`mAttributes`@9, bind an `AttributeInfo` local, read `GetTypeId()`/`GetIntArg(0)`; all
inside `$ct_emit_run`. Assert the corlib `Type.GetMethod`/`Type.GetCustomAttribute`
survive the strip and the generator + shim are gone.
Accept: both tests pass — pins struct-by-value method/attr reflection present + callable
in the sandbox (R5), not just the app JIT. **Risk: medium** (the attribute half builds new
IR scaffolding the FieldInfo precedent never exercised — §3.3/§6 R6). Deps: CMV2-T0.

**CMV2-T1.5 — Exclude `[Comptime]` methods from `MethodMeta` (the one real lowering edit).**
Seed: in `assign_type_ids_and_meta` (`newbf-sema/src/lower.rs:5153-5168`), build a
`HashSet<&str>` from `m.comptime` (the `Vec<String>` of `[Comptime]` method `full_name`s,
populated at `:6304`) and `continue` over any `sig` whose `full_name` is in it before
pushing `MethodMeta` (§2.5). No `MethodSig` change; `&mut Module` is already in scope.
Accept: a unit test asserts a `[Reflect(.Methods)]` class **carrying** a
`[Comptime, EmitGenerator]` method reports `GetMethodCount()` = (non-comptime count) and
`GetMethod(i)` never names the generator; `reflect_method_count.bf → 2` (the generator-free
control) stays green; verify-corpus + run-corpus clean. *This is a genuine sema lowering
change — it is why "no sema edit for v1" is retracted for the method axis.* Deps: CMV2-T0.

**CMV2-T2 — Method marquee (run-corpus). Depends on CMV2-T1.5.**
Seed: land `comptime_reflect_method_count.bf` (§4.1, **expect: 2**) and
`comptime_reflect_method_name.bf` (§4.2, **expect: 1**) in
`e:/NewBF/beef-tests/run-corpus/` (absolute; sibling of the inner workspace). Both put
the generator on a **separate probe class** so neither it (excluded by T1.5) nor its
emitted member enters the reflected target's method set (§2.3 R7). The name program binds
a `MethodInfo` local in the emitted text (R5) and uses `Append(char8*)` (`String.bf:213`,
landed).
Accept: both pass under the JIT full-i32 Stomp harness; the final module JIT- and
AOT-links clean (the strip property); an integration test asserts the generator runs
under Stomp with **no double-free** (R5 — not "balance"). Deps: CMV2-T1, **CMV2-T1.5**.

**CMV2-T3 — Attribute marquee + attribute-driven codegen (run-corpus). The v1 spine; no
lowering change.**
Seed: land `comptime_reflect_attr_typeid.bf` (§4.3, **expect: 1**) and
`comptime_reflect_attr_arg.bf` (§4.4, **expect: 42**) — the *attribute-driven codegen*
headline (read `GetIntArg(0)` in the sandbox, emit a member returning it) — in
`e:/NewBF/beef-tests/run-corpus/`; attribute classes are **classes**, annotated types are
`[Reflect]`. Optionally add `comptime_reflect_attr_strarg.bf` (**expect: 1**, `GetStrArg`
into a nested literal).
Accept: both (or three) pass under the Stomp harness; `attr_int_arg.bf → 42` /
`attr_str_arg.bf → 1` (the runtime read-side) stay green; no double-free under Stomp.
Deps: CMV2-T1. **(Independent of CMV2-T1.5 — the attribute axis needs no lowering edit;
can run in parallel with T1.5/T2.)**

**CMV2-T4 — (optional) corlib `String.Append` overload, only if an example needs one.**
Seed: confirm the §4 examples need no new `Append` (they use `Append(int)`/`Append(char8*)`,
both landed `:231`/`:213`); if a chosen variant needs e.g. `Append(int64)`, add it to
`String.bf` mirroring `Append(int)`.
Accept: a standalone `string_append_*.bf` smoke if added; existing `append_overload.bf` /
`string_append_int.bf` stay green; verify corpus clean. *Likely a no-op — recorded for
honesty.* Deps: —.

**CMV2-T5 — Docs + journal (behavior-preserving).**
Seed: cross-link this doc from `docs/COMPTIME.md`, resolve `comptime-reflection.md` §5's
"methods/attributes deferred" note → "v1 landed (CMV2)", and resolve
`custom-attributes.md` §5/§8's "comptime-attribute composition deferred" → "landed
(CMV2)"; add a journal entry pairing the CMV2-T2/T3 corpus commits + values **and noting
CMV2-T1.5 (the comptime-method exclusion) as the one lowering change**; state the
generic-T deferral as a comptime-metaprogramming-v3 item.
Accept: docs build; journal references the T2/T3 corpus values (`2`, `1`, `42`) and the
T1.5 filter. Deps: CMV2-T0..T4.

**Critical path:** CMV2-T0 (audit) → CMV2-T1 (sandbox pin) → CMV2-T1.5 (method filter) →
{CMV2-T2 (methods) ∥ CMV2-T3 (attributes — can start at T1)} → CMV2-T5 (docs). CMV2-T4 is
an off-path no-op. **Riskiest task: CMV2-T1** — the net-new sandbox struct-by-value
pin whose **attribute half is genuinely new IR scaffolding** (10-field Type +
struct-literal `TypeMeta`, not the 8-field FieldInfo mirror, §3.3/§6 R6); CMV2-T1.5 is the
riskiest *lowering* edit but is a single well-localized predicate with a clear control
(`reflect_method_count.bf` must stay green).

**Staged beyond v1 (recorded, §5):** generic-T comptime reflection (lift the
generic-comptime guard `lower.rs:1851` + bound-`T` `typeof`) → comptime-metaprogramming-v3;
general same-set emit-and-reflect (the R7 hazard CMV2-T1.5 only partially solves);
signature-driven codegen (extend `MethodMeta` with param/return types); value-struct
attribute types; constructed attribute instances (`GetCustomAttribute<T>()`);
member/parameter-level attribute reflection; reflecting emitted-this-round members.
