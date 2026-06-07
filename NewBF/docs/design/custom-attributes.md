# Custom Attributes — User Attribute Types, Queryable via Reflection

*Wave-3 design. Companion to [`reflection.md`](reflection.md) (the metadata
pipeline this rides on), [`comptime-breadth.md`](comptime-breadth.md) (the
deferred comptime-reflection composition), and
[`SPRINT-PLAN-2.md`](SPRINT-PLAN-2.md) (the gate cadence). All file:line
anchors re-verified against the tree on 2026-06-07; the reflection metadata
pipeline (RF-T0..T7) has **landed and is green**, so this feature is an
**additive table alongside `mFields`/`mMethods`**, not a new subsystem.*

---

## 1. Overview + the v1 capability

Attributes are **fully parsed** today — `Attribute { span, name: Type, args: Vec<Expr> }`
(`newbf-parser/src/ast.rs:739-743`), parsed by `Parser::attributes`
(`newbf-parser/src/parser.rs:2797-2830`), attached to every declaration —
but downstream they are reduced to **string-name matching only**: built-in
markers (`[Comptime]`, `[EmitGenerator]`, `[Reflect]`, `[AlwaysInclude]`,
`[Intrinsic]`, `[LinkName]`) are detected by comparing the last path
segment's source text against a literal (`attr_simple_name`,
`newbf-sema/src/lower.rs:12081-12086`; `reflect_policy`, `:12107+`).
**Attribute argument expressions are evaluated nowhere and stored nowhere past
the AST**: the def-graph `AttrRef` keeps only `name` + `arg_count` + `span`
(`newbf-sema/src/model.rs:290-294`), `lower_attrs` drops `a.args`
(`newbf-sema/src/build.rs:554-563`), and `resolve.rs` never touches attributes
at all (grep-verified: zero matches). There is no `Attribute` base type in the
**active** corlib prelude (`newbf-corlib/src/lib.rs:17-58` lists **15** files —
Internal, String, FieldInfo, MethodInfo, Type, Reflection, Compiler, Console,
Pool, Handle, List, Probe, Option, Result, Math; none is `Attribute.bf`), no
`GetCustomAttribute`, and `TypeMeta` (`newbf-ir/src/module.rs:97-112`) carries
no attribute data.

> **Reference point (not in the active prelude).** A full Beef
> `Attribute.bf` *does* exist in the tree as a corpus reference at
> `beef-tests/corlib-slice/Attribute.bf` (`Attribute`, `AttributeTargets`,
> `AttributeUsageAttribute`, `ReflectAttribute`, …). It is **not** in the
> active prelude (`lib.rs`), so it does not participate in lowering. It is the
> authoritative shape reference, and it shows the load-bearing fact that drives
> §1's representation decision below: **every real Beef attribute is a value
> `struct`** — `public struct Attribute` (line 4), `public struct
> ReflectAttribute : Attribute` (line 125), `LinkNameAttribute`,
> `AlwaysIncludeAttribute`, etc. (all `struct`). See the v1 boundary note in §1
> and §5 for why that matters.

**v1 capability (one paragraph).** A user declares a normal `class`
`[MyAttr(args)]` and attaches it to a **type** (`[MyAttr] class C { … }`). Sema
**resolves the attribute name to its `StructId`** (excluding built-in markers),
**const-folds its primitive/string constructor args** (reusing the existing
literal-folding path), and records a per-type list of `AttrData { attr_type_id,
args: Vec<Const> }` into the IR `TypeMeta` (gated by the same strip policy as
fields). The backend emits a policy-gated `[k x %struct.AttributeInfo]` array
per type plus a new `mAttributes`/`mAttrCount` pair **appended** to
`%struct.Type`; corlib `Type` gains `GetCustomAttributeCount() -> int32` /
`GetCustomAttribute(i) -> AttributeInfo`, and `AttributeInfo` exposes
`GetTypeId()` + a small fixed set of primitive arg accessors
(`GetIntArg(i)` / `GetStrArg(i)` / `GetArgCount()`). The whole thing is
**runtime-only, type-level-only, primitives-and-strings-only**, and observable
on the authoritative JIT run-corpus gate via differentials like
`typeof(C).GetCustomAttribute(0).GetTypeId() == typeof(MyAttr).GetTypeId()`.

**The v1 representation boundary — read this before §4 (it explains the
demos).** Two boundaries are load-bearing and shape every example:

1. **The attribute *type* must be a `class` in v1, not a `struct`.** A dense
   reflection type-id is minted **only** for `StructKind::Ref` (classes) —
   `reflectable` filters `matches!(kinds[id], StructKind::Ref)`
   (`newbf-sema/src/lower.rs:4970-4973`), so `type_id_of`
   (`:4979-4982`) has **no entry** for a value `struct`. Since v1 keys an
   attribute by its dense type-id (so `GetTypeId()` round-trips against
   `typeof(MyAttr).GetTypeId()`), the attribute type must be a class to have an
   id at all. **This is non-idiomatic** — real Beef attributes are all
   `struct`s (see the reference note above) — so v1 *cannot reflect an
   idiomatically-declared Beef attribute*. We accept this honestly: v1 demos
   use `class` attributes; the value-`struct` attribute path is the single most
   important **deferred** item (§5), since extending `reflectable` to attribute
   structs churns the entire name-sorted dense-id space and the reflection
   golden, and is its own task.

2. **No constructed attribute *instance*.** v1 does **not** build
   `GetCustomAttribute<T>() -> T`. The comptime sandbox marshals back only
   Int/Bool and **rejects `Ptr`/`Struct`/`Ref`** returns
   (`newbf-comptime/src/eval.rs:107-116`), and the JIT can't resolve
   non-foldable float constants (MEMORY: `__real@` gap) — so constructing and
   returning a populated `T` is deferred. v1 stores **const-folded scalar args
   in an emitted `%struct.AttributeInfo` table** and surfaces the attribute's
   *type-id* + its *scalar args*, mirroring exactly how `FieldInfo`/`MethodInfo`
   already work.

`[Reflect]` on the attribute class is **not** required for it to have a
type-id (every class is reflectable, regardless of policy — `:4970-4973`), so
the §4 examples mark only the *annotated* type with `[Reflect]` (where the
FIELDS gate actually decides whether attributes surface), and the attribute
classes carry no `[Reflect]`.

---

## 2. Representation / ABI / IR changes

### 2.1 The sema ⊥ llvm contract (what sema emits by-name vs what llvm defines)

The HARD INVARIANT holds unchanged: **`newbf-sema` never imports `newbf-llvm`**;
the two agree via owned IR data + named symbols. Concretely:

- **sema emits (owned IR data):** a new `attributes: Vec<AttrMeta>` field on
  `TypeMeta` (`newbf-ir/src/module.rs:97-112`), populated in
  `assign_type_ids_and_meta` (`newbf-sema/src/lower.rs:4967-5049`). Each
  `AttrMeta` carries an `attr_type_id: u32` (the dense reflection id of the
  attribute *class*, via the same `type_id_of` map already built at
  `lower.rs:4979-4982`) and `args: Vec<Const>` (owned primitive/string
  constants — `newbf-ir/src/inst.rs:41-52`). **No `StructId`/`TypeRef` held
  across emission rounds beyond what `FieldMeta`/`MethodMeta` already do**
  (they hold `IrType`/`StructId`, which is fine; `IrType` stays `Copy`).
- **llvm defines (named globals):** `emit_metadata`
  (`newbf-llvm/src/lower.rs:399-633`) defines a `%struct.AttributeInfo`
  aggregate, one private `[k x %struct.AttributeInfo]` const array per type
  (gated on policy), and appends two fields to the `%struct.Type` aggregate
  (body at `newbf-llvm/src/lower.rs:408-420`). The contract between the two
  layers is **the `%struct.Type` / `%struct.AttributeInfo` field order**, pinned
  by the same layout unit tests that pin `Type`/`FieldInfo`
  (`newbf-sema/src/lower.rs:13954`, `:14013`).

**No new IR instruction.** Unlike reflection's `LoadTypeId`, custom attributes
add **zero** instructions: attribute reflection is *all const data* read through
`typeof(T)` (already a `GlobalAddr`, `lower.rs:9654`) and the existing
`%struct.Type` field-load path. SSA-dominance is therefore trivially safe.

### 2.2 IR (`newbf-ir`)

`module.rs` — extend `TypeMeta` (owned data, no lifetimes; `module.rs:97-112`):

```rust
#[derive(Clone, PartialEq, Debug)]
pub struct AttrMeta {
    pub attr_type_id: u32,   // dense reflection id of the attribute CLASS
    pub args: Vec<Const>,    // const-folded scalar args (Int/Bool/Str only in v1)
}

pub struct TypeMeta {
    // … existing: type_id, struct_id, name, policy, is_ref, fields, methods …
    pub attributes: Vec<AttrMeta>,   // NEW. Empty unless policy gates it in.
}
```

`Const` (`newbf-ir/src/inst.rs:41-52`) has **six** variants —
`Int(i128, IrType)`, `Float(f64, IrType)`, `Bool(bool)`, `Null`,
`Undef(IrType)`, `Str(String)`. **v1 stores only `Int`/`Bool`/`Str`** (folded
from int/char/bool/string literals; float deferred §5). The collector
(`attr_arg_const`, §3.2) returns **`None`** for anything else — `Float`,
`Null`, `Undef`, and any non-literal expression are **silently dropped**, which
*shrinks `argCount`* and shifts subsequent `GetIntArg(i)` indices (a
silent-miscount footgun — see §3.2's index-shift note). The backend emit
`match` over `args` therefore only ever sees `Int`/`Bool`/`Str` and uses
`unreachable!()` (or a skip) for the rest. `add_type_meta`
(`module.rs:220-222`) is unchanged (it takes the whole `TypeMeta`). Update
**every** `TypeMeta` constructor and `newbf-ir/src/print.rs::format_ir` for the
new field (see §3.2-constructors and CA-T0 for the **five** sites — this is not
one site).

### 2.3 ABI / `%struct.Type` extension — **three** aggregate sites, atomic

Append two fields to the emitted aggregate and corlib `Type.bf` and the layout
pin — **and to the never-null sentinel initializer, which the draft previously
missed**. The `%struct.Type` aggregate is constructed in **three** distinct
places, all of which must move in lockstep or LLVM rejects the module
(initializer arity ≠ aggregate type, or inkwell's field-count assertion fires):

| # | Site | What changes |
|---|------|--------------|
| 1 | `set_body` (the type definition), `newbf-llvm/src/lower.rs:408-420` | append `i32` (mAttrCount) + `ptr` (mAttributes) → **10 fields** |
| 2 | per-type initializer `type_ty.const_named_struct(…)`, `:528-537` | append `i32 mAttrCount` value + `ptr mAttributes` value (null when stripped) |
| 3 | **sentinel** `unknown_init = type_ty.const_named_struct(…)`, `:558-567` | append `i32_ty.const_zero()` (mAttrCount=0) + `ptr_ty.const_null()` (mAttributes=null) |

Site 3 is the `__newbf_type_unknown` sentinel (mTypeId = -1). It currently
passes **exactly 8 field values** (`:559-566`); if `set_body` grows to 10 but
this stays at 8, the module fails to build — so CA-T2 cannot be "atomic" or
"corpus unchanged" without it. (Plus the corlib + layout-pin updates below.)

```
%struct.Type = type { i32, i32, i32, i32, i32, ptr, ptr, ptr, i32, ptr }
;  mSize mTypeId mFlags mFieldCount mMethodCount mName mFields mMethods
;                                                       mAttrCount(i32) mAttributes(ptr)
%struct.AttributeInfo = type { i32, i32, ptr }
;                            attrTypeId  argCount  args(i64*)   ; see §2.4 for the arg encoding
```

Corlib side: `Type.bf:29-37` gains the two matching fields. Layout-pin side:
`corlib_type_layout_matches_struct_type_aggregate`
(`newbf-sema/src/lower.rs:13954-14002`) currently asserts **exactly 8** fields
(`expected: [IrType; 8]` at `:13974`, `assert_eq!(ty.fields.len(), 8)` at
`:13984`) — bump to **10**, ending `…, IrType::I32, IrType::Ptr`.

`mAttributes` is **null** (and the `[k x %struct.AttributeInfo]` array is *not
emitted*) when the policy strips it — exactly as `mFields`/`mMethods` already
behave (`newbf-llvm/src/lower.rs:456-496`). So `GetCustomAttributeCount()`
returns 0 for an unmarked type — the strip differential is observable, like
`GetFieldCount`.

### 2.4 The attribute-arg encoding (the one genuinely new ABI decision)

`FieldInfo`/`MethodInfo` are fixed-shape; an attribute's args are **variable
count + heterogeneous scalar type**. v1 encodes them as a flat
**`i64[]` per attribute** (one global `[n x i64]` const array, pointed at by
`AttributeInfo.args`), with `argCount` the element count:

- an **int/bool arg** widens to `i64` (the value, sign-extended for ints);
- a **string arg** stores the `char8*` `.rodata` pointer **`ptrtoint`'d to
  i64** (a pointer is 8 bytes; the corlib `GetStrArg` accessor `inttoptr`s it
  back). v1 surfaces two typed accessors — `GetIntArg(i) -> int64` (reads the
  slot) and `GetStrArg(i) -> char8*` (reinterprets the same slot) — and the
  **test program knows the static arg type**. Recording a per-arg *type tag* is
  deferred (§5); this keeps the table a uniform `[n x i64]` with no per-arg tag
  word.

**Constant-expression note (the one novel emit pattern).** The i64 slot for a
string is a **`ptrtoint` constant expression** over the `.rodata` `emit_cstr`
global — *not* a runtime cast in `Main`'s body. Build it with inkwell's
`GlobalValue::as_pointer_value().const_to_int(i64_ty)` and place it directly in
the `[n x i64]` initializer (FieldInfo stores its name pointer as `ptr`; here
we additionally `ptrtoint`-fold it into the uniform i64 slot). `GetStrArg`
lowers to `inttoptr` (an integer→pointer reinterpret, *not* a truncating
bitcast — int↔ptr coercion already exists at `newbf-llvm/src/lower.rs:973-980`).

This is the **JIT-safe** encoding: all entries are integer/pointer constants
(no `__real@` float pool — MEMORY), so the run-corpus JIT gate resolves them.
ORC/RTDyld resolves `ptrtoint`-of-global relocations (it cannot resolve only
`__real@` float-pool entries). Float args are deferred (§5).

### 2.5 corlib types (all ship complete in CA-T2 — pure field reads, no emit dep)

- **`Attribute.bf`** — a new empty base **`class`** added to the active prelude
  (`newbf-corlib/src/lib.rs:17-58`). **It is a `class`, not a `struct`**, for
  two reasons: (1) v1 attribute types are classes (§1 boundary), and a class
  base must resolve to `IrType::Ref` for the `: Attribute` clause to be
  recorded at all — base-routing only records a base when it resolves to
  `IrType::Ref(bid)` **and** `kinds[bid] == StructKind::Ref`
  (`newbf-sema/src/lower.rs:3087-3097`); a `struct` base resolves to
  `IrType::Struct` and is **silently dropped**; (2) it keeps the minimal
  `Attribute.bf` from colliding with future corlib attribute machinery. v1 does
  **not** enforce `: Attribute` (any resolvable class name works, §5) — the
  base exists only so Beef-style `[MyAttr] : Attribute` declarations resolve
  rather than discard a base. *If v1 demos do not use `: Attribute` at all, this
  file can be dropped; it is kept for parse-faithfulness, and §6 pins that it
  verifies clean standalone.*
- **`AttributeInfo.bf`** — a new value `struct`, layout byte-identical to
  `%struct.AttributeInfo`, modelled exactly on `FieldInfo.bf`
  (`newbf-corlib/bf/FieldInfo.bf:20-33`): fields `int32 mAttrTypeId; int32
  mArgCount; int64* mArgs;` + the **complete** accessor set (all pure field
  reads — no emit dependency, so they verify standalone in CA-T2):
  `GetTypeId() -> int32`, `GetArgCount() -> int32`, `GetIntArg(int32 i) ->
  int64` (bounds-checked, returns 0 out of range — the same sentinel discipline
  as `Type.GetField`, `Type.bf:57-66`), and `GetStrArg(int32 i) -> char8*`
  (reinterprets the slot via `inttoptr`).
- **`Type.bf`** — add `GetCustomAttributeCount() -> int32` and
  `GetCustomAttribute(int32 i) -> AttributeInfo`, copy-pasted from `GetField`
  (`Type.bf:57-66`): bounds-check `i`, return an empty `AttributeInfo`
  (`mAttrTypeId = -1`) when `mAttributes == null` or out of range, else
  `this.mAttributes[i]`. Register `AttributeInfo.bf` **before `Type.bf`** in
  the prelude (Type references `AttributeInfo*`), exactly as `FieldInfo.bf`
  precedes `Type.bf` (`lib.rs:21-33`). `Attribute.bf` may register anywhere
  before user code (it has no corlib dependents).

---

## 3. The sema + parser + llvm + runtime changes (with file:line anchors)

### 3.1 Parser — **no change**

Attribute args are already real `Expr`s (`ast.rs:742`), parsed by the arg list
(`parser.rs:2811-2817`), and already printed by `print.rs`. The parser corpus
already reflects attribute args, so **parser-corpus is untouched** — unlike
reflection's Task 0, this feature needs no grammar change. (`[return:]` target
specifiers are still parsed-and-discarded at `parser.rs:2802-2807`; out of v1.)

### 3.2 Sema — attribute-name resolution + arg const-folding + collection

**Seam 1 — a new parallel-vector on `StructTable`** (the established pattern;
`policies: Vec<ReflectPolicy>` at `lower.rs:277` is the exact template). Add
`type_attr_data: Vec<Vec<AttrDataRaw>>` keyed by struct id, where
`AttrDataRaw = { simple_name: String, args: Vec<Const> }` (sema-local,
**unresolved** raw simple-names; becomes `AttrMeta` with the dense id at
Seam 3). It must stay in **lockstep with `defs`** — assert it next to the
existing policy lockstep assert (`lower.rs:574-578`), and push an empty `Vec` at
**every** synthetic id-minting site that pushes `ReflectPolicy::TYPE`:
`register_mono` (`lower.rs:865`), and the two other synthetic-struct minters at
`:835`, `:2829`. (The real registration site, `register_type_struct` `:2538`,
pushes the *collected* vec, Seam 2.) These four push-sites match the four
`policies.push` sites and keep the lockstep assert valid.

**Seam 2 — collect at `register_type_struct`** (`lower.rs:2503-2542`). This is
the only place the type's `td.attributes` AST is in hand alongside its fresh
struct id. **The push must sit *inside* the `if !t.by_name.contains_key(&name)`
dedup guard (`lower.rs:2514`), directly beside the existing
`t.policies.push(reflect_policy(…))` (`:2538-2539`)** — so a duplicate
simple-name type (which gets no `defs`/`policies` entry) also gets no
`type_attr_data` entry, keeping all three vectors parallel.

We collect **raw** here — simple-name + const-folded args — and **do not
resolve names to `StructId` yet**, because the attribute class may be declared
*after* the annotated type and `by_name` is complete only after
`register_struct_names` finishes (`lower.rs:2491-2501`). Resolution happens in
Seam 3.

Concretely, `collect_attr_data(&td.attributes, src) -> Vec<AttrDataRaw>` records
per attribute:
- `simple_name = attr_simple_name(a, src)` (reuse `lower.rs:12081-12086`);
- `args = a.args.iter().filter_map(|e| attr_arg_const(e, src)).collect()`.

`attr_arg_const(e: &Expr, src: &str) -> Option<Const>` is a new helper modelled
on `const_field_init` (`lower.rs:12446-12465`), which matches
`Int`/`Float`/`Bool`/`Char`/`Null`/`Paren`/`Unary{op: UnOp::Neg}` — note the
negation arm is a **nested `Expr::Unary { op: UnOp::Neg, operand }`**
(`:12454-12462`), *not* a flat `Expr::Neg`; replicate that shape or negative
literals are missed. Differences from `const_field_init`:
- there is **no target field type**, so integer/char literals fold to
  `Const::Int(_, IrType::I64)` (the i64-slot encoding, §2.4; `IrType::I64`
  exists at `newbf-ir/src/ty.rs:61`);
- add a **string-literal arm**: `Expr::Str(s) =>
  Const::Str(decode_string_literal(s.text(src)))` (`Expr::Str(Span)` at
  `ast.rs:215`; `decode_string_literal(raw: &str) -> String` at
  `lower.rs:11969`, the same decoder reused at `:12164`);
- everything else returns **`None`** (so `Float`/`Null`/non-literal args are
  dropped). **Footgun:** dropping an arg shrinks `argCount` and shifts every
  later `GetIntArg(i)`/`GetStrArg(i)` index. v1 demos use only directly-foldable
  int/bool/string literals so no drop occurs; document this so a future
  non-foldable arg doesn't silently miscount.

**Seam 3 — resolve simple-names → `StructId` + dense id, in
`assign_type_ids_and_meta`** (`lower.rs:4967-5049`). This function takes only
`&StructTable` (no diagnostic sink) but that is exactly enough: it has
`structs.by_name` and the `type_id_of` dense map (built at `:4979-4982`). For
each reflectable type, read its collected `(simple_name, args)` list, and for
each:
1. **skip built-in markers** — `Reflect`, `AlwaysInclude`, `Comptime`,
   `EmitGenerator`, `Intrinsic`, `LinkName`. This is **defense in depth**, not a
   correctness gate: these short-form names have no backing class, so step 2's
   `by_name` lookup would already skip them. The set is an optimization +
   documentation. *Caveat for future corlib growth:* if a real
   `*Attribute`-suffixed class (e.g. `ReflectAttribute`) is ever registered,
   its simple name is `ReflectAttribute`, not `Reflect` — Beef strips the
   `Attribute` suffix, but we do not yet. The exclusion set assumes the
   short-form spelling; suffix-canonicalization is a follow-on. v1 is safe
   because no backing `*Attribute` class exists in the active prelude.
2. `attr_struct_id = structs.by_name.get(simple_name)` — **skip if unresolved**
   (v1 tolerates an unknown attribute name rather than hard-erroring; lowering
   has no diagnostic sink, per the reflection.md §6 precedent);
3. `attr_type_id = type_id_of.get(&attr_struct_id)` — the attribute class must
   itself be reflectable to have a dense id. **Every `StructKind::Ref` is
   reflectable** (`lower.rs:4970-4973`), so a `class` attribute always resolves;
   a value-`struct` attribute is **not** in `type_id_of` and is **skipped**
   (the v1 class-only constraint, §1/§5). Skip if absent;
4. push `AttrMeta { attr_type_id, args }` onto the `TypeMeta.attributes` built
   at `lower.rs:5039-5047`. **Gate on `policy.has(ReflectPolicy::FIELDS)`** in
   v1 (reuse the existing fields gate at `lower.rs:5003`) — i.e. `[Reflect]`
   surfaces attributes, an unmarked type strips them. (A dedicated
   `ReflectPolicy::ATTRIBUTES` bit is a clean follow-on; v1 piggybacks the
   FIELDS bit to avoid touching the policy enum + `reflect_flag_bits`
   `lower.rs:12092-12105`.)

**No `resolve.rs` change** — attribute resolution lives entirely in the
`StructTable`/`assign_type_ids_and_meta` path, sidestepping the resolve.rs gap
(it has zero attribute handling today, grep-verified).

**The five `TypeMeta` constructors.** Adding the non-defaulted `attributes`
field breaks **every** `TypeMeta { … }` literal. There are **five** (not one):
`newbf-sema/src/lower.rs:5039` (the real population site) **plus** four in the
`newbf-llvm` test/AOT build: `newbf-llvm/src/aot.rs:223`,
`newbf-llvm/src/lower.rs:1900`, `:1909`, `:1957`. All five must push
`attributes: vec![]` or `cargo test -p newbf-llvm` won't compile (CA-T0). To
stop future field-adds re-breaking five sites, consider adding a
`TypeMeta::new(type_id, struct_id, name, policy, is_ref, fields, methods)`
constructor (defaulting `attributes` to empty) and routing the four test/AOT
sites through it — optional, but it pays for itself.

### 3.3 LLVM (`newbf-llvm`) — emit the AttributeInfo table

In `emit_metadata` (`newbf-llvm/src/lower.rs:399-633`):
1. Define `%struct.AttributeInfo = { i32 attrTypeId, i32 argCount, ptr args }`
   once, beside `%struct.FieldInfo`/`%struct.MethodInfo` (`lower.rs:421-426`).
2. Extend the `%struct.Type` `set_body` (`lower.rs:408-420`) with `i32`
   (mAttrCount) + `ptr` (mAttributes) at the end — **and the two other
   `const_named_struct` sites, §2.3 (`:528-537` and the sentinel `:558-567`).**
3. Per `TypeMeta`, **policy-gated** (mirror the FieldInfo block exactly,
   `lower.rs:456-496`): for each `AttrMeta`, emit a private `[n x i64]` arg
   array (the emit `match` over `Const`: `Int`/`Bool` → widened i64 const;
   `Str` → `ptrtoint` of `emit_cstr(s)` per §2.4; `unreachable!()` for the
   others, which `attr_arg_const` never produces), then an `AttributeInfo`
   const `{ attrTypeId, argCount, args_ptr }`; collect into a
   `[k x %struct.AttributeInfo]` private const global; else `(null, 0)`.
4. Append `mAttrCount`/`mAttributes` to the `%struct.Type` per-type initializer
   (`lower.rs:528-537`).

The registry/accessor (`lower.rs:574-633`) is **untouched** — attribute data
hangs off each Type global, which is already in the `__newbf_type_table`. All
data is `constant` → `.rodata`; **no runtime-crate change, no new symbol**
(reflection's in-module-accessor decision already pays off here — JIT and AOT
resolve identically with zero link work, reflection.md §4.4). AOT serializes
the attribute globals like `FieldInfo` already does (no `aot_parity` accessor
change).

### 3.4 Runtime — **no change**

v1 introduces no Rust runtime code and no heap activity (all metadata in
`.rodata`), so the stomp allocator / crash-dump path is untouched — the same
clean property reflection v1 has (reflection.md §4.3).

---

## 4. Worked examples (run-corpus programs that prove it)

Each is a self-contained `Program.Main -> int32` with a `// expect: N` header,
in `e:/NewBF/beef-tests/run-corpus/` (the authoritative JIT gate; ids compared
**relationally**, never hardcoded — type-ids are name-sorted and churn with
corlib growth, `lower.rs:4974-4976`). Modelled on the existing
`reflect_typeof_name.bf` / `reflect_gettype_id_roundtrip.bf`. **The attribute
classes carry no `[Reflect]`** (every class is reflectable regardless of policy,
`:4970-4973`) — only the *annotated* type is `[Reflect]`, where the FIELDS gate
decides whether attributes surface.

```beef
// attr_present_typeid.bf   // expect: 1   (CANONICAL first-slice green)
// The attribute on C is MyAttr: its recorded attr-type-id equals typeof(MyAttr)'s id.
// A differential a null/garbage AttributeInfo can't satisfy.
class MyAttr : Attribute { }                 // a class (v1 attribute = class), no [Reflect] needed
[Reflect, MyAttr] class C { public int32 mX; }
class Program {
    public static int32 Main() {
        let t = typeof(C);
        return (t.GetCustomAttributeCount() == 1
             && t.GetCustomAttribute(0).GetTypeId() == typeof(MyAttr).GetTypeId()) ? 1 : 0;
    }
}
```

```beef
// attr_strip_vs_marked.bf  // expect: 1   (DIFFERENTIAL strip — like reflect_strip_vs_marked)
// A [Reflect]-marked type surfaces its attributes (count 1); an unmarked type strips them (count 0).
class Tag : Attribute { }
[Reflect, Tag] class Marked   { public int32 mX; }
          [Tag] class Unmarked { public int32 mX; }   // no [Reflect] ⇒ attrs stripped
class Program {
    public static int32 Main() {
        return (typeof(Marked).GetCustomAttributeCount() == 1
             && typeof(Unmarked).GetCustomAttributeCount() == 0) ? 1 : 0;
    }
}
```

```beef
// attr_int_arg.bf          // expect: 42   (const-folded primitive arg surfaces)
// 42 <= 255, so the AOT exit-code truncation note (MEMORY) is not tripped; the
// JIT run-corpus harness checks the full i32 regardless. A future arg value >255
// must stay JIT-only.
class Priority : Attribute { public this(int32 p) { } }
[Reflect, Priority(42)] class Job { }
class Program {
    public static int32 Main() {
        let a = typeof(Job).GetCustomAttribute(0);
        return (int32)a.GetIntArg(0);   // 42 — the folded ctor arg (i64 slot, narrowed)
    }
}
```

```beef
// attr_str_arg.bf          // expect: 1   (string arg surfaces; reuses Internal.StrEq)
class Named : Attribute { public this(char8* n) { } }
[Reflect, Named("hi")] class Widget { }
class Program {
    public static int32 Main() {
        return Internal.StrEq(typeof(Widget).GetCustomAttribute(0).GetStrArg(0), "hi") ? 1 : 0;
    }
}
```

```beef
// attr_count_multi.bf      // expect: 2   (two attributes on one type, order preserved;
//                                          the built-in Reflect marker is skipped, not counted)
class A1 : Attribute { } class A2 : Attribute { }
[Reflect, A1, A2] class C { }
class Program {
    public static int32 Main() { return typeof(C).GetCustomAttributeCount(); }
}
```

(`[Reflect, A1, A2]` parses to three ordered attributes in one bracket,
`parser.rs:2808-2826`; `Reflect` is the marker that gates surfacing and is
excluded from the count, leaving 2.)

**Unit pins (deterministic, non-JIT):**
- `corlib_attributeinfo_layout_matches_struct_attributeinfo_aggregate` — new,
  modelled on `corlib_fieldinfo_layout_matches_struct_fieldinfo_aggregate`
  (`newbf-sema/src/lower.rs:14013`): corlib `AttributeInfo` lowered field order
  `{ i32, i32, ptr }` == `%struct.AttributeInfo`.
- Extend `corlib_type_layout_matches_struct_type_aggregate`
  (`lower.rs:13954-14002`) from **8** to **10** fields ending `…, IrType::I32,
  IrType::Ptr` (mAttrCount, mAttributes).
- `emit_metadata` strip pin: for an unmarked class, no `%struct.AttributeInfo`
  array global is emitted (`mAttributes` null), mirroring
  `emit_metadata_strips_fields_unless_marked`
  (`newbf-llvm/src/lower.rs:1869`).

**Golden note (no churn in this slice).** `format_reflection`
(`newbf-ir/src/print.rs:227-276`) backs a byte-for-byte golden
(`tests/newbf-tests/tests/golden/reflection_report.golden`, 5 type rows of the
form `type X [POLICY] kind=… fields=N methods=N`). **v1 deliberately does NOT
add an `attrs=` column** to the line-244 format string — the run-corpus
differentials above already prove attribute behavior, and adding the column
would rewrite all 5 golden rows (and add per-attribute child rows once data
populates) for no behavioral coverage gain. If a future task wants attributes in
the human-readable report, that is a **deliberate golden regeneration** (every
row gains `attrs=N`), scoped as its own change — not a free edit.

---

## 5. v1 scope vs explicitly deferred

**v1 (observably green, runtime-only, type-level, primitives+strings):**
1. `Attribute.bf` (empty base **class**) + `AttributeInfo.bf` (metatype struct
   with all accessors) in the active prelude (`lib.rs:17-58`); `AttributeInfo`
   before `Type`.
2. `StructTable.type_attr_data` parallel vector (template: `policies`,
   `lower.rs:277`), collected raw at `register_type_struct` inside the dedup
   guard (`lower.rs:2514`/`:2538`), resolved + densified in
   `assign_type_ids_and_meta` (`lower.rs:4967`).
3. `attr_arg_const` const-folder (Int/Bool/Char/Str/Neg/Paren; everything else
   → `None`) — reuse `const_field_init` (`lower.rs:12446`) + a `Str` arm.
4. `TypeMeta.attributes: Vec<AttrMeta>` (`module.rs:97`) + backend
   `%struct.AttributeInfo` emission + `%struct.Type` extension across all
   **three** aggregate sites (`newbf-llvm/lower.rs:408-420`, `:528-537`,
   `:558-567`).
5. `Type.GetCustomAttributeCount()` / `GetCustomAttribute(i)` +
   `AttributeInfo.GetTypeId/GetArgCount/GetIntArg/GetStrArg`.
6. Gated by the **FIELDS** policy bit (a type must be `[Reflect]`/
   `[Reflect(.Fields)]`/`[AlwaysInclude]` to surface attributes).

**Deferred (honestly):**
- **Value-`struct` attribute types — the most consequential gap.** Real Beef
  attributes are all value `struct`s (`corlib-slice/Attribute.bf`), but only
  `StructKind::Ref` (class) types get a dense id (`lower.rs:4970-4973`), so v1
  **cannot reflect an idiomatic Beef attribute**; demos use `class` attributes.
  The fix — extend `reflectable` to include attribute-typed value structs (give
  them dense ids too) — churns the entire name-sorted dense-id space and the
  reflection golden, so it is its own task.
- **Generic annotated types** (`[MyAttr] class Box<T>`) surface **no**
  attributes. `register_mono` (`lower.rs:842-868`) builds each monomorph with
  an **empty** attribute slot and hard-codes `ReflectPolicy::TYPE`
  (`:865`, comment `:862-864`: *"a generic template's attributes are NOT yet
  propagated to its monomorphs"*) — `TYPE` lacks the FIELDS bit the §3.2 gate
  requires, so even if the slot were populated it would strip. Template→
  monomorph attribute (and policy) propagation is an RF-level gap, deferred —
  exactly like field/method reflection on monomorphs (RF-T3 simplification).
- **`GetCustomAttribute<T>() -> T`** returning a *constructed* attribute
  instance — needs an attribute-ctor-emission story; the comptime sandbox can't
  return a struct (`eval.rs:107-116`). v1 returns the type-id + scalar args via
  `AttributeInfo`, **not** a populated object.
- **Comptime-reflection composition** (`[Comptime]` reading a decl's
  attributes / attribute-driven codegen) — blocked by **both** the sandbox
  struct-return wall (`eval.rs:107`) **and** the documented "no `Type`/`typeof`
  inside comptime" v1 boundary. Out of v1.
- **Non-scalar args** — `typeof(X)` args, nested objects, **float args** (JIT
  `__real@` pool risk — MEMORY: keep v1 args integer/string only). `Const::Float`
  /`Null`/`Undef` args are dropped by `attr_arg_const`.
- **`: Attribute` base enforcement / `AttributeTargets` / `AttributeUsage`** —
  v1 treats any resolvable class name as an attribute, no base check, no target
  validation. `[return:]`/`[field:]` target specifiers stay
  parsed-and-discarded (`parser.rs:2802-2807`).
- **Member/parameter-level attribute reflection** — v1 is **type-level only**.
  `FieldDef.attributes`/`MethodDef.attributes` exist in the def-graph
  (`model.rs:189`, `:219`) but aren't surfaced.
- **Per-arg type tag in the table** — v1 uses a uniform `[n x i64]` and relies
  on the caller knowing the static arg type (`GetIntArg` vs `GetStrArg`); a
  tagged variant arg is a follow-on.
- **A dedicated `ReflectPolicy::ATTRIBUTES` bit** — v1 reuses FIELDS.
- **An `attrs=` column in `format_reflection`** — deferred (no coverage gain;
  it's a golden regen, §4 golden note).

---

## 6. Load-bearing risks + mitigations

- **(ABI) `%struct.Type` has three aggregate-construction sites.** Appending
  `mAttrCount`/`mAttributes` must update the `set_body`
  (`newbf-llvm/src/lower.rs:408-420`), the per-type initializer (`:528-537`),
  **and the never-null sentinel `unknown_init` (`:558-567`)** — plus corlib
  `Type.bf:29-37` and the layout pin
  `corlib_type_layout_matches_struct_type_aggregate`
  (`newbf-sema/src/lower.rs:13954-14002`, 8→10), **atomically**. Missing the
  sentinel is a hard compile-fail (initializer arity ≠ aggregate type).
  *Mitigation:* CA-T2 lands all of this in one change; the layout test is the
  deterministic detector (catches drift without running the JIT). Same RF-T6/T7
  discipline that added `mFields`/`mMethods`.
- **(constructors) five `TypeMeta` literals.** The new non-defaulted field
  breaks `lower.rs:5039` + `aot.rs:223` + `llvm/lower.rs:1900`/`:1909`/`:1957`.
  *Mitigation:* CA-T0 patches all five (push `attributes: vec![]`); a
  `TypeMeta::new` centralizes future adds.
- **(sandbox) comptime can't return structs / no `Type` in comptime**
  (`eval.rs:107-116`). *Mitigation:* the comptime-composition goal is **out of
  v1** (§5); v1 is pure runtime reflection over `.rodata`. No `eval.rs` path is
  touched.
- **(JIT float-pool) MEMORY: ORC can't resolve `__real@` constants.**
  *Mitigation:* v1 arg encoding is `[n x i64]` — integer/pointer constants
  only; float args are deferred (§5). The string slot is a `ptrtoint` constant
  expression (§2.4), which ORC/RTDyld *does* resolve. The run-corpus gate is
  JIT, so this is a hard constraint, respected by construction.
- **(resolution) built-in markers + simple-name collisions + unresolved
  names.** `Reflect`/`Comptime`/`AlwaysInclude`/`EmitGenerator`/`Intrinsic`/
  `LinkName` have no backing class; `register_type_struct` skips duplicate
  simple names (`lower.rs:2514`) and resolve.rs never resolves attribute names.
  *Mitigation:* the marker-exclusion set is **defense in depth** (an excluded
  marker would already fail `by_name` and be skipped — §3.2 Seam 3 step 1);
  resolve against `by_name` and **skip** (don't error) unknown names; require
  the attribute type to be a reflectable class (so `type_id_of` has it). A class
  named e.g. `Type` would collide with the metatype — documented as a v1 user
  constraint.
- **(SSA) dominance.** No new instruction; attribute reflection is
  `GlobalAddr` + const field-loads through `typeof(T)` (already R9-safe,
  `lower.rs:9654`). Trivially safe.
- **(memory-safety-under-guard) no heap, no UAF surface.** All metadata is
  `.rodata`; v1 adds no `new`/`delete` and no runtime symbols, so the stomp
  guard / crash-dump path (memory-safety.md) is untouched. (Same as
  reflection v1.)
- **(value-struct attribute / generic monomorph) silent zero attributes.** A
  value-`struct` attribute (no dense id) and a generic annotated type's
  monomorph (empty data + `TYPE` policy, `register_mono` `:842-868`) both
  surface **zero** attributes with no diagnostic — both are **deferred** (§5),
  not v1 bugs, but document it so an implementer doesn't expect coverage.
- **(ratchet) the four standing gates, each pinned for CA-T2.** Parser-corpus:
  **unchanged** (args already parse/print). Verify-corpus
  (`newbf-sema/tests/corpus.rs`, **160/160** clean-LLVM): the prelude grows by
  two structs + the `%struct.Type` extension, so *every* verify file re-emits
  the larger Type + the new `AttributeInfo`; assert
  **`Attribute.bf`/`AttributeInfo.bf` verify clean standalone** (some corpus
  files lower without consumer use of the new types, per the `lower_typeof`
  note `lower.rs:9645-9648`) and **verify-corpus stays 160/160 after CA-T2**.
  Run-corpus (`tests/newbf-tests/tests/run_corpus.rs`): the **9 existing
  `reflect_*.bf` programs stay green** after the Type ABI change, and the five
  new `attr_*.bf` programs are the behavioral proof. `aot_parity`: attribute
  globals are `constant` `.rodata` hanging off the existing Type globals —
  serialize for AOT like `FieldInfo` (no in-module accessor change).

---

## 7. Task breakdown

Home-doc prefix **`CA-`** (custom attributes), mirroring the SPRINT-PLAN-2
convention (`RF-T2` etc., `SPRINT-PLAN-2.md:49-52`). Each task is
agent-assignable with a one-line seed + a concrete acceptance gate. Tasks 0-1
are **behavior-preserving plumbing**; 2-5 are behavior-changing, each pinned by
a run-corpus `// expect:` program or a layout/emission unit test. Critical path
is **serial** CA-T0 → CA-T1 → CA-T2 → CA-T3 → CA-T4; CA-T5 depends on T4.

**CA-T0 — IR: `AttrMeta` + `TypeMeta.attributes` (plumbing).**
*Seed:* add `pub struct AttrMeta { attr_type_id: u32, args: Vec<Const> }` and
`pub attributes: Vec<AttrMeta>` to `TypeMeta` (`newbf-ir/src/module.rs:97-112`);
patch **all five** `TypeMeta` constructors to push `attributes: vec![]`
(`newbf-sema/src/lower.rs:5039`, `newbf-llvm/src/aot.rs:223`,
`newbf-llvm/src/lower.rs:1900`, `:1909`, `:1957`) + extend
`newbf-ir/src/print.rs::format_ir`. (Do **not** touch `format_reflection` — §4
golden note.) Deps: none.
*Accept:* `cargo build` **and** `cargo test -p newbf-llvm` compile (proves all
five constructors patched); verify + run corpus **unchanged** (no emission yet);
the IR (`format_ir`) golden is updated, the reflection golden is **untouched**.

**CA-T1 — sema: `StructTable.type_attr_data` + `attr_arg_const` collector
(behavior-preserving collection).**
*Seed:* add the parallel `type_attr_data: Vec<Vec<AttrDataRaw>>` (template:
`policies`, `lower.rs:277`) with a lockstep empty-push at the three synthetic
id-minting sites (`lower.rs:865`, `:835`, `:2829`) and a collected-push **inside
the dedup guard** at `register_type_struct` (`:2514`/`:2538`) + extend the
lockstep assert (`lower.rs:574`); write `attr_arg_const(e, src) -> Option<Const>`
(reuse `const_field_init` `:12446`, nested `Unary{Neg}` shape, + a `Str` arm via
`decode_string_literal` `:11969`/`:12164`, returns `None` outside Int/Bool/Char/
Str); collect `(simple_name, args)` raw. Deps: CA-T0.
*Accept:* compiles; verify + run corpus unchanged (data collected, unread). The
data-content assertion lives in CA-T3 (where `Module::type_meta` is public);
`type_attr_data` is a private `StructTable` field with no public accessor, so
CA-T1's gate is plumbing-only. *(Optional:* add a `#[cfg(test)]` accessor on
`StructTable` and assert `type_attr_data[id]` for `[Tag(7)] class C` records
`("Tag", [Const::Int(7, I64)])` — otherwise defer that to CA-T3.)*

**CA-T2 — ABI: `%struct.Type` (3 sites) + `%struct.AttributeInfo` + complete
corlib types (load-bearing, atomic — the largest task).**
*Seed:* extend `%struct.Type` at **all three** sites — `set_body`
(`newbf-llvm/src/lower.rs:408-420`), per-type init (`:528-537`), **sentinel
`unknown_init` (`:558-567`)** — with `i32 mAttrCount, ptr mAttributes`; bump
`corlib_type_layout_matches_struct_type_aggregate`
(`newbf-sema/src/lower.rs:13954`) **8→10**; add `Type.bf:29-37` fields; define
`%struct.AttributeInfo` in `emit_metadata` (`:421-426` neighbor); add the
**complete** `AttributeInfo.bf` (struct + `GetTypeId/GetArgCount/GetIntArg/
GetStrArg` — all pure field reads, no emit dep) + register before `Type.bf`
(`lib.rs:21-33`); add `Attribute.bf` empty base **class**; add
`Type.GetCustomAttributeCount/GetCustomAttribute` (template `GetField`,
`Type.bf:57-66`); add the
`corlib_attributeinfo_layout_matches_struct_attributeinfo_aggregate` pin
(template `:14013`). Deps: CA-T0.
*Accept (the ABI wall):* the two layout pins green (Type now **10** fields,
AttributeInfo `{i32,i32,ptr}`); `Attribute.bf`/`AttributeInfo.bf` verify clean
standalone; **verify-corpus stays 160/160** and **all 9 `reflect_*.bf`
run-corpus programs stay green** (no attribute data emitted into the new fields
yet — `mAttributes` null everywhere). No new reflection behavior yet.

**CA-T3 — sema: resolve + densify into `TypeMeta.attributes`
(populates the data).**
*Seed:* in `assign_type_ids_and_meta` (`lower.rs:4967-5049`), per reflectable
type read `type_attr_data`, skip the built-in marker set
(`Reflect`/`AlwaysInclude`/`Comptime`/`EmitGenerator`/`Intrinsic`/`LinkName`),
resolve each `simple_name` via `structs.by_name` + `type_id_of` (`:4979`, skip
unresolved / value-struct attrs), gate on `policy.has(FIELDS)` (`:5003`), push
`AttrMeta` onto the `TypeMeta` built at `:5039`. Deps: CA-T1, CA-T2.
*Accept:* a sema unit test asserts `module.type_meta` for a marked
`[Reflect, MyAttr] class C` (MyAttr a class) has `attributes == [AttrMeta{
attr_type_id: <MyAttr's dense id>, args: []}]`, and an unmarked class has
`attributes == []`. Goldens (IR + reflection) **unchanged** (data exists but is
not yet printed/emitted).

**CA-T4 — llvm: emit the AttributeInfo table + `typeof` surfaces it.**
*Seed:* in `emit_metadata` (`lower.rs:399-633`), per `AttrMeta` emit the
`[n x i64]` arg array (the `Const` match: Int/Bool → widened i64;
Str → `ptrtoint`/`const_to_int` of `emit_cstr`, §2.4; `unreachable!()` else) +
`AttributeInfo` const, collect into a policy-gated `[k x %struct.AttributeInfo]`
private global, set `mAttrCount`/`mAttributes` in the per-type Type initializer
(`:528-537`). (Corlib accessors already shipped in CA-T2.) Deps: CA-T3.
*Accept:* run-corpus **`attr_present_typeid.bf` (1)**, **`attr_strip_vs_marked.bf`
(1)**, **`attr_count_multi.bf` (2)** pass; the emission strip pin (no
`%struct.AttributeInfo` array for an unmarked class) passes; verify-corpus +
the 9 `reflect_*.bf` still green.

**CA-T5 — args: surface primitive + string ctor args.**
*Seed:* validate the arg path end-to-end: `attr_arg_const` folds the ctor args
(CA-T1), the `[n x i64]` encoding lands them (CA-T4),
`AttributeInfo.GetIntArg(i)` reads `mArgs[i]` (i64) and `GetStrArg(i)`
`inttoptr`s the slot to `char8*` (CA-T2 accessors). Add a tiny emission pin: an
attribute with one string arg → the `[1 x i64]` global holds a `ptrtoint` of a
`.rodata` cstr. Deps: CA-T4.
*Accept:* run-corpus **`attr_int_arg.bf` (42)** and **`attr_str_arg.bf` (1)**
pass (`GetStrArg` + `Internal.StrEq`).

---

## 8. Open questions / decisions deferred

- **Value-`struct` attribute types — the headline follow-on.** v1 requires a
  class (dense-id prerequisite, `lower.rs:4970-4973`); since every real Beef
  attribute is a `struct`, extend `reflectable` to mint dense ids for
  attribute-typed structs — but that churns the name-sorted dense-id space + the
  reflection golden, so it's a standalone task.
- **Template→monomorph attribute (+ policy) propagation** — `register_mono`
  (`:842-868`) drops both; surfacing attributes on `Box<int>` needs the same
  propagation RF-level field/method reflection on monomorphs awaits.
- **`ReflectPolicy::ATTRIBUTES` bit** vs piggybacking FIELDS — v1 piggybacks;
  a dedicated bit (with a `[Reflect(.Attributes)]` flag in `reflect_flag_bits`,
  `lower.rs:12092`) is a clean follow-on.
- **Per-arg type tags** — v1's uniform `[n x i64]` defers a tagged-union arg;
  revisit when `GetCustomAttribute<T>()` (constructed instance) lands.
- **`: Attribute` base enforcement + `AttributeUsage`/`AttributeTargets`** — a
  checked attribute *concept* (resolve.rs validation + base check) is deferred;
  v1 is structural (any resolvable class name). The `corlib-slice/Attribute.bf`
  reference (`AttributeTargets`, `AttributeUsageAttribute`) is the shape to grow
  into; reconcile simple-name dedup (`register_type_struct` skips duplicate
  simple names, `:2514`) before adding a real `Attribute.bf` to the prelude.
- **Member/param-level attributes** — `FieldDef.attributes`/`MethodDef.attributes`
  (`model.rs:189`, `:219`) are collected but unsurfaced; a `FieldInfo`/`MethodInfo`
  attribute sub-table is the natural extension.
- **An `attrs=` column in `format_reflection`** — deferred; it's a golden regen
  with no behavioral coverage gain (§4 golden note).
