# Reflection — Runtime Type Metadata + System.Reflection

## 1. Problem & goal

**What works today.** Attributes are *parsed and recorded* but never evaluated: `model.rs:119`
`TypeDef.attributes: Vec<AttrRef>` and `model.rs:290` `AttrRef { name, arg_count, span }` capture
name + arity only. Two name-string-only attribute readers exist in `lower.rs`
(`has_comptime_attr`, `extern_symbol`). `sizeof(T)` lowers via `Expr::SizeOf { ty }`
(`lower.rs:5569`) → `size_of_ty` → `fb.size_of(id)` for structs. The object header is a single
`Ptr` at field 0 of every `StructKind::Ref` body, set at `new` to the class vtable global *or null*
(`lower.rs:7398-7405`). Vtables are emitted as bare `[N x ptr]` constant globals
(`newbf-llvm/lower.rs:218-238`), and **only for classes with non-empty `vimpls`** (the sema
registration loop gates on `!structs.vimpls[i].is_empty()` at `lower.rs:3482`). Comptime folds
nullary/all-const-int `[Comptime]` calls post-lowering (`fold.rs`). String literals lower to
`Value::str(...)` typed `IrType::Ptr` (a `char8*`; `lower.rs:5561`).

**What doesn't.** `typeof(T)` **does not even retain its type**: the parser eats the keyword and
discards the argument — `parser.rs:1136-1143` matches `Keyword::TypeOf` together with
`AlignOf`/`StrideOf`, parses the type with `let _ty = self.ty();`, throws it away, and yields a bare
`Expr::Ident(span)`. `PrefixKw::Typeof` exists in the AST enum (`ast.rs:174`) but is never produced
or lowered. There is no `obj.GetType()`, no `System.Type`, no metadata tables, no
`[Reflect]`/`[AlwaysInclude]`/strip policy, no attribute-argument evaluation, no `Type.bf` in corlib,
and no `StrEq`.

**Target (v1).** A minimal-but-real runtime `Type` system, strip-policy-designed-in, with an
**observably-green first slice on user-declared class types only** (primitives, generics-as-typeof,
and value-type `GetType` are explicitly deferred — see §10).

```beef
// run-corpus/reflect_typeid_distinct.bf  // expect: 1   (FIRST SLICE, the canonical green)
[Reflect] class Dog { public int32 mAge; }
[Reflect] class Cat { public int32 mLives; }
static class Program {
    public static int32 Main() {
        Type d = typeof(Dog);
        Type d2 = typeof(Dog);
        Type c = typeof(Cat);
        // Same type ⇒ same id; different type ⇒ different id. A pure differential:
        // a null/garbage Type can't satisfy BOTH halves.
        return (d.GetTypeId() == d2.GetTypeId() && d.GetTypeId() != c.GetTypeId()) ? 1 : 0;
    }
}
```

```beef
// run-corpus/reflect_typeof_name.bf  // expect: 1   (FIRST SLICE)
[Reflect] class Dog { public int32 mAge; }
static class Program {
    public static int32 Main() {
        // StrEq is a prelude char8*-vs-char8* NUL-terminated compare (added in Task 4).
        return StrEq(typeof(Dog).GetName(), "Dog") ? 1 : 0;
    }
}
```

```beef
// run-corpus/reflect_strip_vs_marked.bf  // expect: 1   (FIRST SLICE — a DIFFERENTIAL strip test)
[Reflect(.Fields)] class Marked   { public int32 mX; public int32 mY; }
                   class Unmarked { public int32 mX; public int32 mY; }
static class Program {
    public static int32 Main() {
        // 2 (fields emitted) vs 0 (stripped). A differential: 0 alone could be
        // a broken Type, so we pin BOTH sides in one program.
        return (typeof(Marked).GetFieldCount() == 2 && typeof(Unmarked).GetFieldCount() == 0) ? 1 : 0;
    }
}
```

```beef
// run-corpus/reflect_gettype_id_roundtrip.bf  // expect: 1   (Task 5)
[Reflect] class Dog { public int32 mAge; }
static class Program {
    public static int32 Main() {
        Dog p = scope Dog();
        // p.GetType() reads $header → ClassVData.mType → __newbf_type_by_id → Type*.
        return (p.GetType().GetTypeId() == typeof(Dog).GetTypeId()) ? 1 : 0;
    }
}
```

The goal: `typeof(ClassT)` and `obj.GetType()` return a real `Type` whose `Name`, `Size`, `TypeId`
always resolve, and whose **field/method tables are emitted only when `[Reflect]`/`[AlwaysInclude]`
mark the type** (strip policy). v1 is integer/pointer-only metadata (no float-constant attribute
args — MEMORY: the JIT can't resolve `__real@`), **user-declared class (`StructKind::Ref`) types
only** for `typeof`/`GetType`.

## 2. Current state (file:line)

- **IR struct/field**: `newbf-ir/module.rs:13-17` `FieldDef { name, ty }`; `module.rs:23-27`
  `StructDef { name, fields }`; `module.rs:47-60` `Module { structs, funcs, vtables, globals,
  comptime }`. No metadata field. `VtableDef { name, entries }` (`module.rs:32-38`) — no type id.
- **IR types**: `ty.rs` `StructId(pub u32)`; `IrType` (Copy; `Struct(StructId)`, `Ref(StructId)`,
  `Int{bits,..}`, `Ptr`, …). Primitives are *not* StructIds.
- **IR insts**: `inst.rs` `SizeOf { struct_id }`, `GlobalAddr { name }`. No type-id load.
- **Object header**: `lower.rs` every `StructKind::Ref` body pushes `$header: Ptr` at field 0;
  `lower.rs:7398-7405` `new` stores the vtable global **or `Null`** there (Null when vimpls empty).
- **Four `$header` readers (verified)** — ALL must be reconciled with the ABI change:
  1. `type_test` (`lower.rs:7978-8014`) — pointer-**equality** of the loaded `$header` against
     `global_addr(vtable_name(c))` per candidate class. Drives `lower_is`/`lower_as` (8027-8052).
  2. interface dispatch (`lower.rs:8366-8373`) — `load($header)` then `elem_addr(vtbl, Ptr, slot)`.
  3. virtual dispatch (`lower.rs:8526-8531`) — `field_addr($header)` then `elem_addr(vtbl, Ptr, slot)`.
  4. the `new`-site store (`lower.rs:7398-7405`).
- **Vtable registration loop**: `lower.rs:3482-3488` registers a `VtableDef` **only** when
  `!structs.vimpls[i].is_empty()`.
- **Vtable emission**: `newbf-llvm/lower.rs:218-238` `emit_vtables` → bare `[N x ptr]` constant global.
- **typeof parse**: `parser.rs:1136-1143` discards the type → `Expr::Ident`. `Expr::SizeOf { span,
  ty }` (`ast.rs:302-306`) is the node to mirror.
- **sizeof lowering**: `lower.rs:5569-5572` → `size_of_ty(it)` (`lower.rs:7217-7226`).
- **Attributes**: `model.rs:119, 290`; `lower.rs` `extern_symbol`, `[Comptime]` recording,
  `has_comptime_attr`.
- **Comptime**: `fold.rs` `fold_comptime`; `eval.rs` `eval_const_i64`. Driver runs `fold_comptime`
  post-lower. Untouched by v1.
- **Runtime**: `newbf-runtime/lib.rs` lists "Reflection metadata" as future; nothing implemented.
  **NOTE: `newbf-runtime` is NOT a dependency of `newbf-tests`** (its dev-deps are
  lexer/parser/sema/ir/llvm/comptime/winapi — verified in `tests/newbf-tests/Cargo.toml`).
- **Corlib prelude**: `newbf-corlib/src/lib.rs` `prelude()` is a **hardcoded list** of 9
  `(filename, include_str!)` pairs. No `Type.bf`. No `StrEq`.
- **JIT**: `jit.rs:113` `OrcJit::from_ir`; `jit.rs:152` `DynamicLibrarySearchGeneratorForProcess`
  resolves **only symbols already loaded in the host process** (this is why `malloc`/`free` work —
  they are CRT symbols). A custom `#[no_mangle]` Rust symbol with no dep edge is *not* in-process.
- **AOT**: `aot.rs:53-71` `emit_object_to_memory`/`emit_object`; `aot.rs:83+` `link_executable`
  links only kernel32/msvcrt/ucrt/vcruntime + `extra_libs`. Its own comment (`aot.rs:79-82`):
  the runtime staticlib "joins this arg list **when it lands**; for now the CRT alone suffices."
- **Run harness**: `run_corpus.rs` parse→analyze→lower→`OrcJit::from_ir`→call `Program.Main`→check
  i32. JIT-only. Imports lexer/parser/sema/ir/llvm/comptime only.

## 3. Approach

### Chosen design: type-id in the object header (Beef-faithful), per-type metadata structs as constant globals, **LLVM-emitted registry + accessor** (no runtime-crate dependency), sema-evaluated policy.

Five layered pieces, each shippable behind green gates. Two design decisions were forced by the
ground truth above and differ from the initial sketch:

- **The registry lookup `__newbf_type_by_id` is emitted as an LLVM function inside the compiled
  module**, NOT as a Rust shim in `newbf-runtime`. Reason (verified blocker): the JIT resolves
  externals via the *process* symbol generator, and `newbf-tests` has no `newbf-runtime` dep, so a
  Rust shim is simply absent from the JIT process; AOT likewise does not link the runtime staticlib.
  An in-module LLVM function (a bounds-checked index into an in-module table) resolves in **both**
  JIT and AOT with **zero** linking work and keeps `newbf-sema ⊥ newbf-llvm`. **No Rust runtime code
  is introduced by v1 reflection at all** — which also makes the "no stomp/VM interaction" claim
  trivially true.

- **`typeof` requires a parser change** (a new `Expr::TypeOf { span, ty }` node mirroring
  `Expr::SizeOf`), because the parser currently *discards* the type argument. This is Task 0 and is
  gated by the parser corpus.

1. **ABI: type-id joins the header (Beef-faithful, Option A).** `$header` at object field 0 stays an
   `IrType::Ptr`, but now always points at a **ClassVData global** whose layout is the named LLVM
   struct `%ClassVData.<T> = { i32 mType, [N x ptr] vtbl }` — exactly Beef's
   `ClassVData { int mType; [vtable] }`. The single canonical per-class header object is the
   ClassVData global (symbol `classvdata_name(id)`); the bare `vtable_name` global is **retired** (it
   is folded into ClassVData). For classes with no virtuals/interfaces, the vtable array is `[0 x
   ptr]`, so the global is effectively `{ i32 mType, i32 pad }`. **`new` always stores
   `&ClassVData.<T>`** into `$header` (never `Null`), so `GetType()` works on every heap class.
   `typeof(T)` returns a pointer to the per-type **Type global** (a *separate* constant from
   ClassVData). `obj.GetType()` reads `$header` → `ClassVData.mType` (i32) → `__newbf_type_by_id` →
   `Type*`.

   Picked over **Option B (type-info at a second header word)**: B is ABI-divergent from Beef and
   wastes 8 bytes on every object forever. Picked over **Option C (type-id inside the vtable array)**:
   C forces every `GetType` through a vtable load even for non-virtual classes.

2. **All `$header` consumers route through ONE shared helper.** Because the header now leads with
   `{ i32 mType, i32 pad }`, the vtable slot base shifts. There are **three** consumers, not one, so
   we introduce `fn load_vtable_base(&mut self, hdr_ptr: Value) -> Value` (loads `$header`, then a
   **struct-typed GEP into `%ClassVData` field 1** to reach the vtable array base — letting LLVM
   compute the padded offset, never a hand-rolled byte offset) and route virtual + interface dispatch
   through it. `type_test` (is/as) is changed to compare `$header` against `classvdata_name(c)` (the
   same pointer `new` now stores). A companion `fn load_type_id(&mut self, hdr_ptr: Value) -> Value`
   reads `%ClassVData` field 0 as **i32** (matches the registry index width — never i64).

3. **IR: metadata lives on `Module`, not on `IrType`** (respects `IrType: Copy`, `StructTable`
   no-lifetime — owned data only). Add `Module.type_meta: Vec<TypeMeta>` and `VtableDef.type_id:
   u32`. One new instruction `LoadTypeId { obj }` (result `I32`). `typeof` needs **no** new
   instruction — it lowers to `GlobalAddr { name: type_global_name(id) }`.

4. **Sema evaluates *policy*; LLVM *emits*.** Sema computes a `ReflectPolicy` per type from
   `[Reflect(flags)]`/`[AlwaysInclude]` + a module default (enum-flag literals, **no comptime
   callback**), assigns dense type-ids, records `TypeMeta` into the IR Module, and sets
   `VtableDef.type_id`. `newbf-llvm` emits Type globals + ClassVData globals + (policy-gated)
   field/method tables + the registry table + the registry accessor function.

5. **Runtime `Type` = a corlib Beef value struct over the emitted globals; the registry accessor is
   an LLVM function.** `Type.bf` is a slim corlib **`struct`** (deliberately *not* a class — see §4.5)
   whose field order exactly matches the emitted `%struct.Type` aggregate; its accessors read those
   fields. `__newbf_type_by_id(i32) -> ptr` is an **LLVM-emitted** bounds-checked index into the
   emitted `__newbf_type_table`, returning a sentinel "unknown" Type on out-of-range (never null) so
   `GetName` can't deref null.

### Alternatives considered & rejected

- **Rust runtime shim for the registry (`newbf-runtime/metadata.rs`).** Rejected — *blocker*: not in
  the JIT process (`newbf-tests` has no runtime dep; ORC resolves only in-process symbols) and not on
  the AOT link line (`link_executable` doesn't add the runtime staticlib). The LLVM-emitted accessor
  needs neither and is strictly simpler.
- **Pure-comptime metadata.** `obj.GetType()` is inherently runtime (dynamic type unknown at the
  call site); the run-corpus is the authoritative *runtime* gate. Comptime-only tests nothing the
  gate observes.
- **Unified `[i64]` blob metadata (Beef `mMemberDataOffset` style).** Opaque, hard to verify; the
  LLVM verifier can't type-check it. Per-type LLVM struct globals are self-describing and the
  reflection phase report falls out of iterating `Module.type_meta`.
- **`typeof(int32)` in the first slice via `SizeOf`.** Rejected — *blocker*: primitives are
  `IrType::Int{..}` with no StructId, so the StructId-keyed `type_global_name(id)` can't name a
  primitive Type global. Primitive Types are deferred (§10) to a later task that designs a
  primitive→TypeMeta path (synthetic non-StructId entries with reserved low ids).

## 4. Representation / IR / runtime / ABI changes

### 4.1 IR (`newbf-ir`)

`module.rs` — owned metadata (no lifetimes, IrType stays Copy):

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ReflectPolicy(pub u32);
impl ReflectPolicy {
    pub const NONE:    ReflectPolicy = ReflectPolicy(0);
    pub const FIELDS:  ReflectPolicy = ReflectPolicy(1);
    pub const METHODS: ReflectPolicy = ReflectPolicy(2);
    pub const TYPE:    ReflectPolicy = ReflectPolicy(4);  // always-on minimum (name+id+size)
    pub const ALL:     ReflectPolicy = ReflectPolicy(7);
    pub fn has(self, b: ReflectPolicy) -> bool { self.0 & b.0 == b.0 }
}

#[derive(Clone, PartialEq, Debug)]
pub struct FieldMeta  { pub name: String, pub ty: IrType, pub field_index: u32 }
#[derive(Clone, PartialEq, Debug)]
pub struct MethodMeta { pub name: String, pub symbol: String, pub param_count: u32 }

/// One per reflectable type. Emitted (policy-gated) as a constant Type global by newbf-llvm.
#[derive(Clone, PartialEq, Debug)]
pub struct TypeMeta {
    pub type_id: u32,
    pub struct_id: StructId,        // for backend size + field offsets at emit time
    pub name: String,               // simple name, e.g. "Dog"
    pub policy: ReflectPolicy,
    pub is_ref: bool,               // class (heap, has ClassVData) vs value struct
    pub fields: Vec<FieldMeta>,     // empty unless policy.has(FIELDS)
    pub methods: Vec<MethodMeta>,   // empty unless policy.has(METHODS)
}
```

Add `pub type_meta: Vec<TypeMeta>` to `Module` (default empty — programs with no reflectable types
pay nothing). Extend `VtableDef { name, entries, type_id: u32 }`; **all constructors default
`type_id = 0`**, and `print.rs` is updated for the new field. Add `Module::add_type_meta`.

`inst.rs` — one new instruction (update `newbf-ir/print.rs` and every exhaustive `InstKind` match in
`newbf-sema`/`newbf-llvm`):

```rust
/// Load the runtime TypeId from an object's $header:
/// obj.$header -> ClassVData field 0 (i32). Result type is I32. obj is a Ref(_) heap instance.
LoadTypeId { obj: Value },
```

### 4.2 ABI / object layout

**ClassVData global** — the single canonical per-class header object (one global per heap class):

```
%ClassVData.<T> = type { i32, [N x ptr] }                 ; named LLVM struct
@<classvdata_name(id)> = constant %ClassVData.<T> { i32 K, [N x ptr] [ ... ] }
```

- N = number of vtable slots; for a class with empty `vimpls`, `N = 0` ⇒ `{ i32 mType, [0 x ptr] }`
  (LLVM still inserts 4 bytes of trailing pad to 8-align; reach the vtable via a **struct GEP**, not
  a byte offset).
- **`new` always stores `&@classvdata_name(id)`** into `$header` (Task 2 retires the
  empty-vimpls `Null` branch and the bare `vtable_name` global).
- Vtable slot `k` is reached by `load_vtable_base($header)` = `getelementptr %ClassVData.<T>, ptr h,
  0, 1` (field 1), then `elem_addr(base, Ptr, k)`. `mType` is `getelementptr %ClassVData.<T>, ptr h,
  0, 0` loaded as **i32**.

**Type global** (the metatype value `typeof(T)` points at) — headerless aggregate; field order is
pinned and MUST match corlib `Type` (§4.5):

```
%struct.Type = type { i32, i32, i32, i32, i32, ptr, ptr, ptr }
;             mSize  mTypeId mFlags mFieldCount mMethodCount  mName  mFields  mMethods
@<type_global_name(id)> = constant %struct.Type { ... }
%struct.FieldInfo  = type { ptr, i32, i32 }   ; name(char8*), offset, typeId
%struct.MethodInfo = type { ptr, ptr, i32 }   ; name(char8*), symbol(char8*), paramCount
```

`mFields`/`mMethods` are **null** (and their `[k x %FieldInfo]`/`[m x %MethodInfo]` arrays are *not
emitted*) when the policy strips them — so `GetFieldCount()` returns `mFieldCount` (0 when stripped),
and the strip policy is observable in `lower_to_string` as "the array global isn't there." `mSize` is
the **object instance size** (`get_size(struct_id)`), filled by the backend at emit time (not routed
through `SizeOf`).

**Type registry** (for `GetType` by id):

```
@__newbf_type_table = constant [COUNT x ptr] [ ... ]   ; dense over the COMPACT reflectable id-space
@__newbf_type_count = constant i32 COUNT
@__newbf_type_unknown = constant %struct.Type { i32 0, i32 -1, ... ptr @.str.unknown, null, null }
define ptr @__newbf_type_by_id(i32 %id) { ... }        ; LLVM-emitted; see §4.4
```

`COUNT` = number of `TypeMeta` entries (the compact reflectable id-space — **not** the full
`StructTable`), so a program with no reflection emits a `[0 x ptr]` table and pays nothing.

### 4.3 Runtime (`newbf-runtime`)

**No change.** v1 reflection introduces no Rust runtime code. The registry accessor is an LLVM
function (§4.4). All metadata lives in `.rodata` (constants); there is no heap activity, so the stomp
allocator and crash-dump machinery are untouched.

### 4.4 The registry accessor as an LLVM function

Emitted by `emit_metadata` into the same module the JIT/AOT compiles (resolves identically in both,
no external linkage):

```
define ptr @__newbf_type_by_id(i32 %id) {
entry:
  %cnt = load i32, ptr @__newbf_type_count
  %ok  = icmp ult i32 %id, %cnt          ; unsigned: also rejects negative
  br i1 %ok, label %hit, label %miss
hit:
  %slot = getelementptr [COUNT x ptr], ptr @__newbf_type_table, i32 0, i32 %id
  %t    = load ptr, ptr %slot
  ret ptr %t
miss:
  ret ptr @__newbf_type_unknown          ; sentinel, never null
}
```

### 4.5 The corlib `Type` representation (field-index hazard, resolved)

`Type` is a **value `struct`**, not a class. A class instance carries a `$header` at field 0
(`StructKind::Ref`), which would shift every accessor's field index by one relative to the headerless
`%struct.Type` constant the backend emits — an off-by-one miscompile. Making `Type` a `struct` means
its lowered layout has **no `$header`**, so its field order matches the emitted aggregate exactly.
`typeof(T)` returns `Ref(type_struct_id)` (a pointer to the headerless `%struct.Type` constant);
`Type` methods lower as ordinary instance methods that `field_addr` through that pointer.

A **unit test pins the contract**: corlib `Type`'s lowered field order/offsets == the
`%struct.Type` aggregate `emit_metadata` writes (assert field count and each field IR type). If
corlib `Type` is absent (the verify corpus runs *without* corlib), typeof lowering degrades
gracefully (see §5.2).

### 4.6 Mangling

Reuse the per-id prefix (`StructTable.prefixes[id]`):
`type_global_name(id) = format!("{}$type", prefix.trim_end_matches('.'))`,
`classvdata_name(id) = format!("{}$cvdata", …)`. This mirrors the existing `vtable_name` convention,
so monomorphs (`Box$int.` → `Box$int$type` / `Box$int$cvdata`) get distinct metadata automatically.

## 5. Sema / parser / comptime / runtime / codegen changes

### 5.1 Parser (Task 0)
`typeof` currently discards its type. Add `Expr::TypeOf { span, ty }` to the AST (mirroring
`Expr::SizeOf { span, ty }`), re-point `Keyword::TypeOf` to build it (keep the parsed `ty` instead
of `let _ty`), and update `print.rs` + every exhaustive `Expr` match (`span()` arm, sema, parser
print). `AlignOf`/`StrideOf` keep their existing drop-the-type behavior (out of scope). Also extend
attribute capture so `[Reflect(.Fields)]`/`[AlwaysInclude]` retain their **enum-flag identifier
text** (`Fields`/`Methods`/`All`) — `AttrRef` records `arg_count` only today; capture the flag
spans/text into the attribute record reaching sema (no expression evaluation). **Gate: parser corpus
154/154.**

### 5.2 Sema (`newbf-sema`)
- **Policy** (`reflect_policy(attrs, src, module_default) -> ReflectPolicy`, next to
  `has_comptime_attr`): `[Reflect(flags)]` → OR of the flags; `[AlwaysInclude]` → `ALL`; bare
  `[Reflect]` → `TYPE|FIELDS|METHODS`; else module default (v1 default = `TYPE`: name+id+size always,
  fields/methods stripped). Pure string/enum matching — **no LLVM, no comptime**.
- **Dense type-ids**: assign a `u32` per type that actually gets a `TypeMeta` (i.e. reflectable:
  every `StructKind::Ref` declared in the *user* program, plus any used in `typeof`/`GetType`).
  **Order: sort by mangled name** (not raw struct-id order) so type-ids are stable across corlib
  growth (corlib is prepended; raw struct-id order would churn). The registry table is dense over
  this compact id-space.
- **`typeof` lowering** (new `Expr::TypeOf { ty }` arm): resolve `ty` to a `StructId`. Reuse the
  `new_class_id`-style resolution (`structs.ty_of`/`mangle_generic`) — *not* `lower_ty_env` alone —
  so bare class names and generic applications both resolve. If it resolves to `Ref(id)` of a
  registered class with a `TypeMeta`, emit `fb.global_addr(type_global_name(id))` typed
  `Ref(type_struct_id)` (looked up once via `structs.by_name.get("Type")`). **Primitive operands and
  unresolved generic params** → v1 emits the `@__newbf_type_unknown` sentinel global (deferred to
  §10; lowering has no diagnostic sink). If corlib `Type` is unregistered (verify corpus w/o corlib),
  fall back to a null `Ref` — typeof is unreachable there, so no behavior depends on it.
- **`GetType` lowering**: in `lower_method_call`, special-case `recv.GetType()` **only** when
  `recv : Ref(id)` with `kinds[id] == Ref` (a heap class) **and** no user-defined `GetType` overload
  resolves first (Beef makes `GetType` intrinsic/non-overridable — match that, and place the
  special-case before the generic instance `pick_overload`). Emit `LoadTypeId { obj: recv }` then
  `fb.call("__newbf_type_by_id", [id_i32], Ref(type_struct_id))`. **Value-type receivers** (Struct):
  no `$header` to read — return `typeof(static-type)` directly (a v1 simplification vs Beef's runtime
  null; revisit per §10).
- **Metadata recording**: after monomorphization & vtable layout (so field indices and method
  symbols are final), populate `module.type_meta` from `StructTable`
  (`defs[id].fields`/`methods[id]`/`prefixes[id]` + computed policy + `is_ref` from `kinds`). Set
  `VtableDef.type_id` (the dense id) at vtable construction.
- **Crate boundary**: all sema→IR data; **no LLVM import**. SSA-dominance is trivially safe — `typeof`
  is a constant `GlobalAddr` (no deps); `LoadTypeId`/the registry call are emitted inline at the
  receiver's use site (the receiver already dominates).

### 5.3 ABI plumbing (Task 2 — load-bearing, behavior-preserving once complete)
- **Sema registration loop** (`lower.rs:3482`): register a ClassVData entry for **every**
  `StructKind::Ref` id (with `type_id` and `entries` — `entries` empty when `vimpls` empty), not only
  classes with virtuals.
- **`new`-site** (`lower.rs:7398-7405`): always
  `store($header, global_addr(classvdata_name(id)))`; delete the empty-vimpls `Null` branch.
- **`type_test`** (`lower.rs:8006`): compare against `classvdata_name(c)` (the pointer `new` now
  stores), **not** `vtable_name(c)`. (Its `targets.is_empty()→None` short-circuit keys off
  *compile-time* `vimpls` emptiness, not a runtime null header, so it is unaffected by the
  universal-non-null-header change — verified.)
- **Shared helpers**: add `load_vtable_base` (struct-GEP `%ClassVData` field 1) and `load_type_id`
  (`%ClassVData` field 0 as i32). Route virtual dispatch (`lower.rs:8526-8531`) and interface
  dispatch (`lower.rs:8366-8373`) through `load_vtable_base`. **All three sites change atomically.**
- **`$header`-reader audit**: a `Grep` for `$header` before landing confirms the *only* readers are
  type_test, virtual dispatch, iface dispatch, and (new) `LoadTypeId`.

### 5.4 Codegen (`newbf-llvm`) — both JIT and AOT
`emit_module` (`lower.rs:49-67`) inserts `cg.emit_metadata(ir)` after the (renamed) ClassVData
emission. `emit_vtables` becomes `emit_classvdata`: for each `VtableDef`, emit the named
`%ClassVData.<T> = { i32 type_id, [N x ptr] vtbl }` constant (prepending `type_id`); the bare
`vtable_name` global is gone. The two/three dispatch GEPs now index a `%ClassVData`-typed struct GEP
(field 1) emitted by sema as `load_vtable_base`.

`emit_metadata`:
1. Define `%struct.Type`/`%struct.FieldInfo`/`%struct.MethodInfo` once.
2. Per `TypeMeta`: emit the name `[N x i8]` const; policy-gated `[k x %FieldInfo]`/`[m x %MethodInfo]`
   arrays (offsets from `get_size`/field layout; field typeIds from the dense map, or 0 for
   non-reflected field types); then the `%struct.Type` constant (`mSize = get_size(struct_id)`,
   `mFields`/`mMethods` null when stripped).
3. Emit `@__newbf_type_table` (dense `[COUNT x ptr]` over the compact id-space),
   `@__newbf_type_count`, `@__newbf_type_unknown`.
4. Emit the `@__newbf_type_by_id` LLVM function (§4.4).

`LoadTypeId` lowers to: load `$header` (ptr) from `obj` field 0, then struct-GEP `%ClassVData` field
0 and `load i32`.

**JIT vs AOT parity**: both consume the same IR Module → the same ClassVData/Type/registry/accessor
globals. `__newbf_type_by_id` is in-module ⇒ resolves with no external symbol or link change in
either path. Type globals are `constant` → `.rodata`; the registry is `constant` → `.rodata`; AOT
needs zero metadata-specific work.

### 5.5 Comptime
Untouched for v1. The metadata pass runs in sema during lowering; `fold_comptime` runs after and is a
no-op for reflection. The seam is preserved for future `[Comptime] typeof(T).GetFields()` (§10).

### 5.6 Corlib (Task 4)
Add `bf/Type.bf` and **register it in `prelude()`** (`newbf-corlib/src/lib.rs`) — after
`Internal.bf`/`String.bf` (it uses `char8*`), before consumers. `Type` is a `struct` (§4.5) with
fields exactly matching `%struct.Type` and methods `GetName()->char8*`, `GetSize()->int32`,
`GetTypeId()->int32`, `GetFieldCount()->int32`. Add `StrEq(char8*, char8*) -> bool` (a
NUL-terminated byte compare) to `Internal.bf` (or `String.bf`), with its own tiny standalone
run-corpus test so a `reflect_typeof_name` failure is never ambiguous between a metadata bug and a
StrEq bug. (A string literal lowers to a `char8*`, so `char8*`-vs-`char8*` `StrEq` is the natural
comparison — *not* `String.Equals`, which compares `String` objects.)

## 6. Interactions

- **is/as (`type_test`) — the most ABI-fragile consumer.** Folded into Task 2: it compares `$header`
  against `classvdata_name` (the pointer `new` stores). Named green-gate programs below.
- **itables / interface + virtual dispatch.** Prepending `{i32 mType, i32 pad}` shifts every vtable
  slot; mitigated by the single `load_vtable_base` (struct-GEP field 1) routing all dispatch sites.
  **The itable invariant harness (`lower.rs:10059+`) operates on the *logical* slot model
  (`iface_slot_base`/indices/symbols) and does NOT inspect byte offsets — it CANNOT detect the
  physical slot-shift.** The detectors are (a) the run-corpus virtual/iface/abstract/override
  programs and (b) a new `newbf-llvm` emission unit test asserting `%ClassVData = {i32, [N x ptr]}`
  and that dispatch GEPs index field 1. The harness is still updated to stay green, but it is *not*
  the slot-shift detector.
- **generic monomorphs (§106-114).** Each monomorph has a distinct prefix/struct id ⇒
  `type_global_name(id)` auto-produces distinct metadata; `Module.type_meta` indexes by the dense id.
- **$Func / function values (§49).** `MethodMeta.symbol` is a plain mangled name; metadata-only, no
  codegen interaction in v1.
- **two-phase targeted args.** Orthogonal; `typeof`/`GetType` are type-arg/nullary forms.
- **delete/free.** A deleted object's `$header` still points at the `.rodata` ClassVData; a
  `GetType()` after `delete` (UAF) would read a valid Type rather than fault — masking a UAF. Out of
  v1 scope; noted because every object now carries a live type pointer.
- **Diagnostics model.** Lowering has no diagnostic sink (per MEMORY). v1 reflection therefore
  emits the `@__newbf_type_unknown` sentinel for `typeof(unresolved)` rather than hard-erroring; a
  resolve-phase diagnostic is a follow-on. v1 restricts `typeof` to statically-resolvable class types
  in the corpus.

## 7. Risks & mitigations

- **The ClassVData slot-shift / is-as miscompile (verify-clean but wrong).** Highest-probability
  failure. *Mitigations*: (1) Task 2 is behavior-preserving and ships behind a **named** run-corpus
  green list (is/as + virtual + iface + abstract); (2) the `load_vtable_base`/`load_type_id` helpers
  centralize the offset so the three sites can't diverge; (3) a deterministic **non-JIT emission unit
  test** asserts the `%ClassVData` shape and the field-1 dispatch GEP — the only detector independent
  of running the JIT.
- **Registry symbol resolution.** Eliminated by emitting `__newbf_type_by_id` as an in-module LLVM
  function (§4.4) — resolves in JIT and AOT with no dep/link change. (The original Rust-shim plan was
  a verified blocker in both paths.)
- **`Type` field-index off-by-one.** Eliminated by making `Type` a value `struct` (no `$header`) +
  a unit test pinning corlib-`Type` layout == `%struct.Type`.
- **Null/out-of-range Type deref.** `__newbf_type_by_id` returns the `@__newbf_type_unknown`
  sentinel (never null) on out-of-range; a null-Type guard test covers it.
- **`mType` width.** `LoadTypeId` loads exactly **i32** (matches the registry index) — pinned in the
  helper + an IR/codegen comment.
- **type-id determinism vs corlib churn.** Type-ids are assigned by **mangled-name sort**, and no
  run-corpus `expect` value hardcodes a numeric id (tests compare ids *relationally*). The phase
  report (Task 7) keys rows by name.
- **Metadata bloat.** Strip policy: v1 default emits only `{name,id,size}` per type; field/method
  tables require `[Reflect(.Fields)]`/`[AlwaysInclude]`. Registry is dense over the *compact*
  reflectable id-space (not all structs). `format_reflection` reports exactly what each build emitted.
- **Comptime re-entrancy / circular dep.** Avoided — policy comes from enum-flag literals in sema;
  the comptime seam is untouched; `newbf-sema ⊥ newbf-llvm` holds (no Rust runtime code at all).

## 8. Testing strategy

**Gates green throughout**: parser-corpus (154/154), verify-corpus (154/154 LLVM-clean), run-corpus
(~204, authoritative), the LLVM verifier, the itable invariant harness.

**New run-corpus programs** (each `// expect: N`, returns i32; ids compared *relationally*, never
hardcoded):
1. `reflect_typeid_distinct.bf` → `typeof(Dog)==typeof(Dog) && typeof(Dog)!=typeof(Cat)` (ids) → **1**.
   *(canonical first-slice green — a differential a null/garbage Type can't satisfy.)*
2. `reflect_typeof_name.bf` → `StrEq(typeof(Dog).GetName(), "Dog")` → **1**.
3. `streq_basic.bf` → standalone `StrEq` smoke (`StrEq("ab","ab") && !StrEq("ab","ac")`) → **1**
   *(disambiguates StrEq bugs from metadata bugs).*
4. `reflect_strip_vs_marked.bf` → `Marked.GetFieldCount()==2 && Unmarked.GetFieldCount()==0` → **1**
   *(differential strip).*
5. `reflect_typeof_size.bf` → `typeof(TwoInts).GetSize()` → **the backend object instance size**
   (header 8 + two i32 = 16; pin to the actual `get_size` value once layout is confirmed).
6. `reflect_gettype_id_roundtrip.bf` → `(scope Dog()).GetType().GetTypeId()==typeof(Dog).GetTypeId()`
   → **1** (proves `$header.mType` and the registry agree).
7. `reflect_gettype_polymorphic.bf` → base ref at a derived instance →
   `GetType().GetName()`==derived name → **1** (dynamic type via `$header`).
8. `reflect_field_count_marked.bf` → `[Reflect(.Fields)] Point{mX;mY}` → `GetFieldCount()` → **2**.
9. `reflect_field_name.bf` → `typeof(Point).GetField(0).GetName()`=="mX" → **1**.
10. `reflect_mono_distinct.bf` → `typeof(Box<int32>).GetTypeId()!=typeof(Box<int64>).GetTypeId()`
    → **1**.

**Emission / non-JIT unit tests** (the deterministic detectors):
- `%ClassVData` shape is `{i32, [N x ptr]}` and dispatch GEPs index field 1 (the slot-shift detector).
- For an unmarked class, no `%FieldInfo` array global is emitted (`mFields` null in
  `lower_to_string`); for `[Reflect(.Fields)]` it is — strip policy observable in the module string.
- corlib `Type` lowered field order == `%struct.Type` field order (the §4.5 contract).
- `format_reflection` golden phase report — rows keyed by `(name, policy, field_count,
  method_count)`, diff-gated (Task 7).

**AOT smoke** (the run-corpus is JIT-only): one tiny AOT test emits a module with a single Type
global + the accessor and asserts the object links (catches a `.rodata` mis-serialization the JIT
gate would miss). If deferred, §10 records AOT metadata as unverified-in-v1.

**No new harness needed** — `run_corpus.rs` already checks i32 returns; the registry accessor is
in-module so nothing new must link.

**Runtime-safety note** (later phases, not v1): v1 reflection introduces **no heap activity** (all
metadata in `.rodata`, the accessor is in-module), so the stomp allocator / crash-dump path is
untouched and needs none of it.

## 9. Task breakdown (ordered)

**FIRST SLICE = Tasks 0-4** = `typeof(ClassT).GetName()/GetTypeId()` + differential strip
(`GetFieldCount` 2-vs-0), on **user-declared class types only**. Primitives, generics-as-typeof, and
value-type `GetType` are deferred. Each task lands behind all green gates.

**Task 0 — Parser: `Expr::TypeOf { span, ty }` + attribute-flag capture (grammar change).**
Scope: `newbf-parser/ast.rs` (add `Expr::TypeOf`, mirror `Expr::SizeOf`; add to the `span()` match),
`parser.rs:1136-1143` (re-point `Keyword::TypeOf` to keep `ty`), `print.rs`, attribute-arg flag-text
capture for `[Reflect(...)]`/`[AlwaysInclude]`. Deps: none.
Accept: **parser corpus 154/154**; a parser unit test that `typeof(Dog)` produces `TypeOf{ty:Dog}`.

**Task 1 — IR metadata representation (behavior-preserving plumbing).**
Scope: `newbf-ir/module.rs` (`ReflectPolicy`, `FieldMeta`, `MethodMeta`, `TypeMeta`,
`Module.type_meta`, `add_type_meta`, `VtableDef.type_id` default 0), `inst.rs` (`LoadTypeId`),
`print.rs` (print `LoadTypeId` + `type_id`), and every exhaustive `InstKind`/`VtableDef` match in
sema/llvm (compile-only stubs). Deps: T0 (independent in practice).
Accept: workspace compiles; **verify + run corpus unchanged** (no emission yet); IR golden/print
tests updated.

**Task 2 — ClassVData ABI + the three `$header` sites + helpers (load-bearing, behavior-preserving).**
Scope: `newbf-sema/lower.rs` — registration loop `3482` (ClassVData for *every* `Ref` id), `new`-site
`7398` (always store `&classvdata_name`), `type_test` `8006` (compare `classvdata_name`), add
`load_vtable_base`/`load_type_id`, route virtual `8526` + iface `8366` dispatch through
`load_vtable_base`; `newbf-llvm/lower.rs` — `emit_vtables`→`emit_classvdata` emitting `%ClassVData =
{i32 type_id, [N x ptr]}` and retiring the bare `vtable_name` global; update the itable invariant
harness assertions atomically; **add the `%ClassVData`-shape + field-1-GEP emission unit test**.
Deps: T1.
Accept (the regression wall — **named** green list): `is_as.bf`, `virtual_poly.bf`,
`virtual_basic.bf`, `abstract_method.bf`, `base_call.bf`, the three `iface_*` programs, the it_t3
invariant test — **all green**; the new emission unit test green. No reflection behavior yet.

**Task 3 — Sema policy + dense type-ids + metadata recording (populates `type_meta`).**
Scope: `newbf-sema/lower.rs` — `reflect_policy`, name-sorted dense type-id assignment over
reflectable types, populate `module.type_meta`, set `VtableDef.type_id`. Deps: T0, T1, T2.
Accept: verify/run corpus green; a unit test asserts `module.type_meta` has correct
`(name, policy, field_count)` for a marked vs unmarked class.

**Task 4 — LLVM Type-global emission + registry accessor + typeof + corlib Type.bf + StrEq.**
Scope: `newbf-llvm/lower.rs` `emit_metadata` (Type globals, policy-gated FieldInfo/MethodInfo arrays,
`__newbf_type_table`/`__newbf_type_count`/`__newbf_type_unknown`, the `__newbf_type_by_id` LLVM
function); `newbf-sema/lower.rs` `Expr::TypeOf` arm (`GlobalAddr` of `type_global_name`, sentinel for
non-class); `newbf-corlib` new `bf/Type.bf` **registered in `prelude()`** + `StrEq` in
Internal/String; the corlib-`Type`-layout-vs-`%struct.Type` unit test. Deps: T0-T3.
Accept: run-corpus **1, 2, 3, 4, 5** pass; the strip emission unit test (mFields null when unmarked)
passes.

**Task 5 — GetType() runtime lookup.**
Scope: `newbf-sema/lower.rs` `recv.GetType()` → `LoadTypeId` + `__newbf_type_by_id` (gated on heap
`Ref` receiver, no user `GetType` override; value-type → `typeof(static)`); `newbf-llvm/lower.rs`
lower `LoadTypeId`. Deps: T2, T4.
Accept: run-corpus **6, 7** (id roundtrip, polymorphic dynamic type) pass.

**Task 6 — Field metadata + GetFieldCount/GetField/FieldInfo.**
Scope: emit `[k x %FieldInfo]` (name/offset/typeId) under `policy.has(FIELDS)`; corlib
`GetFieldCount`/`GetField(i)->FieldInfo` + `FieldInfo.GetName`. Deps: T4.
Accept: run-corpus **8, 9** pass.

**Task 7 — Method metadata + `System.Reflection` stubs + phase report (stretch).**
Scope: emit `%MethodInfo` arrays under `policy.has(METHODS)`; corlib `System.Reflection` (`MethodInfo`
name-only, `BindingFlags` stub); `format_reflection` diff-gated golden (rows keyed by name).
Deps: T6.
Accept: a `reflect_method_count.bf` passes; the reflection report is captured as a golden file.

Tasks 0-2 are **behavior-preserving** (T0 grammar-only/no new semantics; T1 plumbing; T2 ABI prep
validated by existing programs + the emission unit test). Tasks 3-7 are **behavior-changing**, each
pinned by run-corpus coverage.

## 10. Open questions / decisions deferred

- **`typeof(primitive)`** (`typeof(int32)`): primitives are `IrType::Int{..}` with no StructId, so the
  StructId-keyed representation can't name a primitive Type global. Deferred to a task that designs a
  primitive→TypeMeta path (reserved low type-ids + synthetic `TypeMeta` not keyed by StructId, with
  `IsPrimitive` set). Removed from the first slice.
- **`typeof(generic-T-param)` at runtime** (`typeof(T)` inside `List<T>`): needs a runtime type-arg;
  deferred to v2 (Phase 9+). v1 emits the sentinel for unresolved params.
- **`GetType` on value types**: v1 returns the static Type (simpler than Beef's runtime-null); revisit
  if a corpus program needs Beef-exact null.
- **Interface Type metadata / itable enumeration**: deferred with generic interface methods (Phase 9+).
- **Beef-exact `Type` ABI** (`mMemberDataOffset`, `sizeof(Type)==40`): v1 uses the slimmer 8-field
  layout; full parity deferred (we never share `Type` across an ABI boundary in v1).
- **AOT metadata verification**: the run-corpus is JIT-only; if the Task-8 AOT smoke is deferred, AOT
  metadata serialization is unverified-in-v1 (accepted risk; the accessor is in-module so the symbol
  resolves either way).
- **Per-module vs whole-program registry**: v1 emits one registry per module (single-module
  programs); cross-module merging is an AOT-phase (Phase 11) concern.
- **Comptime reflection** (`[Comptime] typeof(T).GetFields()`): requires the comptime callback to
  reach metadata; deferred to comptime-breadth (§2.5c). Out of v1 scope.
- **Attribute args beyond enum flags** (string/typeof args, general `AttributeInfo` table): deferred —
  v1 evaluates only `[Reflect(flags)]`/`[AlwaysInclude]`.

## 11. Adversarial-review resolution log

- **BLOCKER (correctness/integration): is/as `type_test` ABI break** — accepted. Folded into Task 2:
  compare `$header` against `classvdata_name`; `is_as.bf` named in Task 2's green list.
- **BLOCKER (correctness/integration/planning): Rust runtime shim not resolvable in JIT or AOT**
  (`newbf-tests` has no `newbf-runtime` dep; `link_executable` doesn't link the staticlib) — accepted.
  `__newbf_type_by_id` is now an **in-module LLVM function**; the runtime crate dependency is dropped
  entirely.
- **BLOCKER (planning): typeof discards its type** (`parser.rs:1139`) — accepted. New **Task 0**
  adds `Expr::TypeOf` and is gated by the parser corpus, sequenced before any typeof lowering.
- **BLOCKER (planning): `typeof(int32)` unimplementable in the first slice** (primitives have no
  StructId) — accepted. Removed from the first slice; primitive Types deferred (§10).
- **MAJOR: "slot base shifts in one place" is wrong (three sites)** — accepted. Single
  `load_vtable_base` helper routes virtual + iface dispatch; `type_test` reconciled; all three change
  atomically in Task 2.
- **MAJOR: vtable registration loop skips vtableless classes / new stores Null** — accepted. Task 2
  registers ClassVData for every `Ref` id and always stores `&ClassVData`.
- **MAJOR: itable harness can't detect the slot-shift** — accepted. The slot-shift detector is the
  run-corpus + a new `%ClassVData`-shape/field-1-GEP emission unit test; the harness is updated to
  stay green but is not claimed as the detector.
- **MAJOR: `Type` field-index off-by-one** ($header on a class) — accepted. `Type` is a value
  `struct`; a unit test pins corlib-`Type` layout == `%struct.Type`.
- **MAJOR: typeof site/operand mislocated** (it's `Expr::Prefix`-class, not `Expr::SizeOf`; resolves
  an expr/type, not via `lower_ty_env` alone) — accepted. New `Expr::TypeOf{ty}` arm using the
  `new_class_id`-style resolver.
- **MAJOR: Type.bf not registered in prelude; StrEq absent; char8*-vs-String mismatch** — accepted.
  Task 4 registers `Type.bf` in `prelude()`, adds `StrEq(char8*,char8*)` with a standalone smoke
  test; `GetName` returns `char8*` and tests compare via `StrEq`.
- **MAJOR: primitive size can't route through `SizeOf`** — accepted. `mSize` filled by the backend
  from `get_size(struct_id)` at emit time; the (deferred) primitive path uses the literal width.
- **MAJOR: `mSize` = instance vs ClassVData size** — accepted. `mSize` is the **object instance**
  size (`get_size`), independent of the ClassVData type-id word.
- **MAJOR: strip-default `==0` is non-differential** — accepted. The first-slice strip test is
  `Marked==2 && Unmarked==0` in one program, plus the emission unit test bound to Task 4 acceptance.
- **MINOR: implicit-pad / byte-offset footgun** — accepted. Slot/`mType` access uses struct-typed
  GEPs into the named `%ClassVData`, never a hardcoded byte offset.
- **MINOR: null on out-of-range** — accepted. `@__newbf_type_unknown` sentinel, never null.
- **MINOR: type-id determinism vs corlib churn** — accepted. Name-sorted dense ids; no hardcoded ids
  in tests; report keyed by name.
- **MINOR: `mType` width unstated** — accepted. `LoadTypeId` loads i32, pinned.
- **MINOR: dense `[all-structs]` registry bloat** — accepted. Registry is dense over the **compact
  reflectable id-space** (= `type_meta` count), so non-reflecting programs emit `[0 x ptr]`.
- **MINOR: GetType resolution precedence / value-type receiver** — accepted. Gated on heap `Ref` + no
  user override; value-type → `typeof(static)`, no `LoadTypeId`.
- **MINOR: print.rs / golden updates unowned** — accepted. Folded into Task 0 (Expr) and Task 1 (IR).
- **NOTE (rejected as out-of-v1, recorded): GetType-after-delete masks UAF; AOT metadata
  unverified-if-smoke-deferred** — acknowledged in §6/§10 as accepted risks, not v1 blockers.
