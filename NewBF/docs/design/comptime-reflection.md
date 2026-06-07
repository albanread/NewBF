# Comptime Reflection — `typeof(T).GetFields()` at compile time → reflection-driven codegen

> Status: design (implementation-ready, **hardened** after three adversarial
> reviews). Wave-3 feature. Composes [`comptime-breadth.md`](comptime-breadth.md)
> (the emission fixpoint loop) with [`reflection.md`](reflection.md) (the
> `%struct.Type` / `FieldInfo` metadata + the in-module `__newbf_type_by_id`
> accessor). All `file:line` anchors below were **re-verified against the live tree**
> at the §95 wave (the code lives under `NewBF/src/<crate>/...`; the prose research
> map used a different path shorthand — ignore it). Re-grep before editing: the
> EmitTypeBody wall and neighbours drift by a few lines per commit.

## 1. Overview & the v1 capability

Today comptime is **primitives-only**: a `[Comptime, EmitGenerator]` generator can
emit Beef *source text*, but that text must be a **string literal**
(`newbf-sema/src/lower.rs:9891` — `let [Expr::Str(s)] = args else { return None };`),
so a generator cannot emit text it *computed* by reflecting over a type
(comptime-breadth.md §1.2 lists `typeof`/`Type`/reflection as an explicit v1
non-goal; reflection.md §10 defers "Comptime reflection" symmetrically). This is the
documented primitives-only boundary.

**v1 capability (one paragraph).** Lift that boundary just enough that a
`[Comptime, EmitGenerator]` generator can call `typeof(T)`, `T.GetFieldCount()`,
`T.GetField(i).GetName()` (and `.GetTypeId()` / `.GetOffset()`) **at compile time**,
build a `String` from the results, and pass that runtime-computed `String` to
`Compiler.EmitTypeBody(...)` — which now emits the *runtime* bytes instead of
demanding a literal. The canonical use is auto-generating a member that iterates a
type's fields (a field-count accessor, a hand-rolled `ToString`/`Equals` skeleton).
The enabling insight (verified, §3.1): the emission **sandbox JIT already contains
the full reflection metadata**, because the *same* `emit_module` builds it for every
module including the sandbox clone (`newbf-comptime/src/emit.rs:499` →
`newbf-llvm/src/jit.rs:123` → `newbf-llvm/src/lower.rs:80` calls `emit_metadata`
unconditionally). So the sandbox can already *run* `typeof(T)` and the
`Type`/`FieldInfo` query methods — the only thing blocking reflection-driven codegen
is the literal-only EmitTypeBody wall. **v1 = relax that one wall + plumb the runtime-
`String` operand correctly + prove a generator that reflects to emit.** Fields only;
methods/attributes/generic-T deferred (§5).

The v1 marquee (the run-corpus proof, §4): a `[Reflect(.Fields)]` class with a
`[Comptime, EmitGenerator]` that emits `int32 FieldCount() { return <N>; }` where `N`
is read at generator-runtime from `typeof(Self).GetFieldCount()` — a value computable
only if the generator saw the reflected field count and emitted it into a member that
re-resolves and runs.

## 2. Representation / ABI / IR changes — and the `sema ⊥ llvm` contract

**The headline: almost nothing new is *represented*.** This feature adds no new IR
instruction, no new ABI, no new metadata. But the load-bearing engineering is *not*
"one trivial edit": it is the **lowering-path plumbing** that turns a runtime `String`
operand into the shim's `(ptr, i32 len)` without double-evaluating the argument and
without tripping the value-struct-method-chain trap (§2.2, §3.2). Two of three reviews
independently flagged that the *plumbing*, not the wall, is where the bugs live.

### 2.1 No new IR, no new instruction, no new metadata

- **`typeof(T)` already lowers to a constant `GlobalAddr`** (`lower.rs:9644-9659`):
  for a user class it returns `self.fb.global_addr(type_global_name(prefix))` typed
  `Ref(Type)` (`:9653-9655`); non-class/unresolved → `GlobalAddr(__newbf_type_unknown)`
  (`:9659`). Note the result type is `Ref(Type)` **only when corlib is present**
  (`type_id.map_or(IrType::Ptr, IrType::Ref)`, `:9648`); standalone (no corlib) it is
  `Ptr`, but `typeof` is unreachable there — irrelevant to run-corpus, where corlib is
  always linked. A constant `GlobalAddr` has no operands ⇒ it **trivially
  SSA-dominates** every use, including inside a comptime generator body. No new
  instruction.
- **The metatype query methods are ordinary Beef** — `Type.GetFieldCount()`,
  `Type.GetField(i)`, `FieldInfo.GetName()` are plain field-reading methods in
  `newbf-corlib/bf/Type.bf:47,57` and `FieldInfo.bf:27`, byte-identical to the emitted
  `%struct.Type` / `%struct.FieldInfo` aggregates (`newbf-llvm/src/lower.rs:407-426`).
  They lower and execute like any other call, in the app JIT **and the sandbox** — no
  sema special-case to read fields, no new intrinsic. **Receiver-shape caveat (§3.2,
  Risk 2):** `typeof(T)` is a `Ref(Type)` rvalue, which `struct_base`'s non-lvalue arm
  accepts (`lower.rs:9598-9606`, the `IrType::Ref(id)` arm). But `GetField(i)` returns a
  **value-struct `FieldInfo` by value** (`Type.bf:57`, `FieldInfo` is `struct`,
  `FieldInfo.bf:20`), i.e. an `IrType::Struct(id)` rvalue, which `struct_base`
  **rejects** (only `Ref` rvalues flow through). So you may **not** chain
  `typeof(T).GetField(0).GetName()`; you must bind a local `FieldInfo f` first. The
  existing `reflect_field_name.bf` does exactly this (`FieldInfo f = typeof(Point).GetField(0); f.GetName()`),
  and §4.2's *emitted runtime text* must too.
- **`Module.emit_jobs` / `EmitJob` / `__newbf_ct_emit` / `EMIT_SINK`** are all already
  in place (comptime-breadth §4.1-4.2; `emit.rs:99,129`). The shim ABI is **length-
  based** (`emit.rs:129-140` copies `len` bytes), so it does not care whether the text
  was literal — only the *sema rewrite* (`lower.rs:9891`) currently restricts the
  source to a literal. Relaxing that rewrite reuses the shim verbatim.

### 2.2 The one representational decision: runtime-`String` operand at the EmitTypeBody seam

The single change is what `try_lower_emit_type_body` (`lower.rs:9874-9907`) accepts as
its text argument:

- **Today (literal-only):** `[Expr::Str(s)]` → `decode_string_literal` → a static
  `.rodata` `Value::str` + a compile-time `text.len()` literal (`:9891-9894`), passed
  as `__newbf_ct_emit(<owner_id>, ptr, len)`.
- **v1 (relaxed):** accept **two explicit cases** for the single argument — a literal
  (the fast path) or an expression that lowers to `Ref(String)` — and **report a sema
  diagnostic for anything else** (do *not* silently decline; see the next bullet for
  why declining is unsafe). For the `Ref(String)` case, lower the arg **exactly once**
  to a `Value`, then obtain its byte pointer and length by calling its `Ptr()` and
  `Length()` methods **via the methods-table-lookup helper** (the same pattern
  `append_to_string` uses, `lower.rs:10133-10141`), narrowing the length to `i32`.

> **Why a value-struct *and* a class subtlety bites here.** `String` is a **class**
> (`String.bf:4` `class String`), and a class body's **field 0 is the ClassVData
> header** (`construct_string` stores it at `field_addr(p, id, 0)`, `lower.rs:10056-10059`),
> so its user fields are `mPtr`@1 / `mLength`@2 / `mCapacity`@3. **Therefore the
> tempting "just `field_addr(body, string_id, 0)` for `mPtr`" shortcut is OFF BY ONE
> and layout-fragile.** Use the **methods** `Ptr()` (→ `char8*`, `String.bf:29`) and
> `Length()` (→ `int` = i64, `String.bf:24`), which already encode the layout. The
> length must be narrowed from i64 to i32 for the shim's `int32 len` parameter — and
> the narrowing seam is `self.coerce(len64, IrType::I64, IrType::I32)`
> (`lower.rs:11769-11783`, the int→int arm picks `CastKind::Trunc` by width). **There
> is no `self.fb.trunc`** — only `self.fb.cast(CastKind::Trunc, …)` (`func.rs:130`);
> `coerce` is the higher-level seam and is what to call. (The prose research map's
> "`String` has `.Ptr`/`.Len`" and the earlier `fb.trunc` sketch are both wrong for
> this tree.)

> **Why "decline → fall through to the stub" is UNSAFE and must not be used.** The
> caller (`lower.rs:7651`) falls through on `None` to `try_enum_construct` and then
> `lower_method_call` (`lower.rs:7657-7659`), both of which **re-lower the args**
> (`lower_method_call` takes `base: &Expr` and re-evaluates the receiver and
> arguments). Two distinct hazards follow, both verified:
> 1. **Double-emit.** If `try_lower_emit_type_body` first lowers the arg via
>    `self.expr(other, src)` and *then* `return None`, the fall-through re-lowers the
>    same arg. For an arg with side effects (`new String(...)`, a builder call) this
>    emits the allocation **twice** — one leaked, two different pointers. The current
>    literal-only code is safe precisely because it declines *before* emitting anything.
>    The fix: **decide the branch from the arg's type WITHOUT first committing to
>    emission** — peek/typecheck (`Expr::Str` literal? otherwise lower once and inspect
>    `IrType`), and once you have lowered a `Ref(String)`, **always consume it** (never
>    `return None` after lowering).
> 2. **Silent miscompile.** `Compiler.EmitTypeBody` is declared **`(String text)`**
>    with an **empty `[Comptime]` body** (`Compiler.bf:38-39`). If a non-literal,
>    non-`String` arg declined into the fall-through, it would resolve against that
>    single `String` overload. A `char8*` literal would `coerce`-auto-wrap into a fresh
>    `String` (`lower.rs:11743-11748`) and hit the **empty stub** — the emission is
>    *silently dropped*. So the relaxed seam must handle exactly `Expr::Str` (fast path)
>    and `Ref(String)`, and emit a real diagnostic ("`EmitTypeBody` expects a string
>    literal or a `String`") for anything else.

### 2.3 The `sema ⊥ llvm` contract (what sema emits by-name vs what llvm defines)

The HARD INVARIANT (newbf-sema must not depend on newbf-llvm) is **preserved
unchanged** — this feature adds no new cross-crate coupling. The edit names only
`__newbf_ct_emit`, the `String` struct (by `by_name`), and `Ptr`/`Length` (via the
methods table by id) — all by name/id.

| Symbol | Defined by | Referenced by sema (by name only) |
| --- | --- | --- |
| `%struct.Type` / `FieldInfo` / `MethodInfo` aggregates | **llvm** `emit_metadata` (`lower.rs:407-426`) | never named in sema; sema only knows the corlib `Type` struct id via `by_name.get("Type")` (`lower.rs:9648`) |
| per-type `Type` global `type_global_name(prefix)` | **llvm** `emit_metadata` (`newbf-llvm/src/lower.rs:548`) | sema emits `GlobalAddr(type_global_name(prefix))` **by name** (`lower.rs:9654`); the two `type_global_name` impls (sema `:1078`, llvm `:357`) agree by convention, already pinned |
| `__newbf_type_by_id(i32)->ptr` accessor | **llvm** in-module fn (`newbf-llvm/src/lower.rs:588-627`) | sema emits a `call "__newbf_type_by_id"` by name (`lower.rs:11197`) |
| `__newbf_ct_emit(i32, ptr, i32)` host shim | **newbf-comptime** Rust `extern "C"` (`emit.rs:129`), bound via `add_absolute_symbol` (`emit.rs:503`) | sema emits a `call "__newbf_ct_emit"` by name (`lower.rs:9897`) |
| `newbf_alloc` (the String **object body** in the sandbox) | **newbf-runtime**, bound absolute (`jit.rs:186`) | sema emits `newbf_alloc(...)` by name (`lower.rs:9710`+) — see §3.5 for what is and isn't guard-tracked |

Reflection-at-comptime is **generator code running on the backend JIT, driven from
`newbf-comptime`** (which legally depends on both sema and llvm). Sema's only job stays
"recognize + record + a local body rewrite," exactly as comptime-breadth §3 framed it.
**No metadata-reading Rust code is added to sema, and no host shim reads
`module.type_meta`** — the generator reads reflection *through the emitted corlib `.bf`
API in the sandbox*, not through a new Rust view. (This is the §3.0 decision, made
explicit below.)

## 3. The concrete changes, with file:line anchors for every seam

### 3.0 The design decision the research posed: emitted API vs Rust host view

> *Does the generator call the SAME emitted reflection API (Type globals in the
> sandbox module), or does newbf-comptime expose a Rust-side reflection view of
> `module.type_meta` to the generator via a host shim (like `__newbf_ct_emit`)?*

**Decision: the generator calls the SAME emitted reflection API in the sandbox.** No
new host shim, no Rust-side reflection view. Justification (all verified):

1. The sandbox clone is built by `emit_module` (`run_generators` → `OrcJit::from_ir`
   → `emit_module`, `emit.rs:499`, `jit.rs:123`), and `emit_module` calls
   `emit_metadata` **unconditionally** (`lower.rs:80`). So the sandbox already holds
   every `Type` global, the `__newbf_type_table`, `__newbf_type_count`, the
   `__newbf_type_unknown` sentinel, and the in-module `__newbf_type_by_id` accessor —
   the accessor is a **pure in-module LLVM function** (`newbf-llvm/src/lower.rs:588`),
   so there is **no host symbol to bind** for reflection. The worry "the comptime
   sandbox can't see reflection" is **false**. (T1 still hard-proves this in the
   *sandbox-shaped* `from_ir` build, not just the app JIT — see §7.)
2. `typeof(T)` is a constant `GlobalAddr` (`lower.rs:9655`) and the query methods are
   plain Beef (`Type.bf`) — both already JIT-resolve in the sandbox with zero new
   machinery.
3. A Rust host view of `module.type_meta` would duplicate the metadata model on the
   host, re-introduce a marshalling ABI, and (worse) make the generator see a
   *different* reflection surface than ordinary runtime code — a divergence bug
   waiting to happen. The emitted-API path makes comptime reflection and runtime
   reflection the **same** surface by construction.

The only thing the host shim (`__newbf_ct_emit`) still does is **carry the emitted
text bytes out** — unchanged from comptime-breadth.

### 3.1 Parser

**No grammar change.** `typeof` already parses to `Expr::TypeOf { ty }` (the reflection
RF-T0 work; dispatched at `lower.rs:7507`). `[Comptime]`/`[EmitGenerator]` parse with
the existing attribute grammar (`comptime_emitter_of`, `lower.rs:12065`). The generator
body is ordinary Beef calling ordinary methods. Parser corpus stays at its current
ratchet (160/160).

### 3.2 Sema (`newbf-sema/src/lower.rs`) — the ONE edit (plumbing-heavy)

**Relax `try_lower_emit_type_body` (`lower.rs:9874-9907`).** Today the body is:

```rust
let owner_id = self.emit_owner?;                       // :9881  (in an emit generator)
if name != "EmitTypeBody" { return None; }             // :9882
let Expr::Ident(b) = base else { return None };        // :9886  (receiver is `Compiler`)
if b.text(src) != "Compiler" { return None; }          // :9887
let [Expr::Str(s)] = args else { return None };        // :9891  ← THE WALL (literal only)
let text = decode_string_literal(s.text(src));         // :9892
let len  = text.len() as i128;                         // :9893
let ptr  = Value::str(text);                           // :9894
self.fb.call("__newbf_ct_emit", vec![ /* i32 owner, ptr, i32 len */ ], IrType::Void);  // :9897
```

Replace the `:9891` arm. The **two safe cases** are a literal and a `Ref(String)`;
everything else is a **diagnostic**, never a silent `return None` after lowering
(§2.2). Sketch (the `/* ... */` are spelled out below — they are the real work):

```rust
let [arg] = args else { return None };                 // exactly one text arg (else decline early)

// Fast path (back-compat): a literal stays a static .rodata str + const len.
// Decided from the AST BEFORE any emission, so no double-evaluation is possible.
if let Expr::Str(s) = arg {
    let text = decode_string_literal(s.text(src));
    let len  = text.len() as i128;
    let ptr  = Value::str(text);
    self.fb.call("__newbf_ct_emit",
        vec![Value::int(owner_id.0 as i128, IrType::I32), ptr, Value::int(len, IrType::I32)],
        IrType::Void);
    return Some((Value::int(0, IrType::I32), IrType::Void));
}

// v1: any expression that lowers to Ref(String). Lower it EXACTLY ONCE.
let (sval, sty) = self.expr(arg, src);
let string_id = self.structs.by_name.get("String").copied();
if Some(()) != string_id.map(|id| ()).filter(|_| sty == IrType::Ref(string_id.unwrap())) {
    // NOT a String and NOT a literal → a real user error, NOT a silent decline.
    self.error(src_span_of(arg), "EmitTypeBody expects a string literal or a String");
    return Some((Value::int(0, IrType::I32), IrType::Void)); // recover, don't fall through
}
let string_id = string_id.unwrap();

// Read text.Ptr() -> char8* and text.Length() -> int via the methods table
// (the append_to_string pattern, lower.rs:10133-10141): NO direct field_addr
// (class field 0 is the ClassVData header — see §2.2), NO re-lowering of `arg`.
let ptr_sig = self.structs.methods[string_id.0 as usize]["Ptr"]
    .iter().find(|m| m.is_instance && m.params.len() == 1).cloned().unwrap();
let ptr = self.fb.call(ptr_sig.full_name, vec![sval.clone()], ptr_sig.ret); // char8* (Ptr)

let len_sig = self.structs.methods[string_id.0 as usize]["Length"]
    .iter().find(|m| m.is_instance && m.params.len() == 1).cloned().unwrap();
let len64 = self.fb.call(len_sig.full_name, vec![sval], len_sig.ret);       // int (I64)
let len   = self.coerce(len64, IrType::I64, IrType::I32);                    // shim wants i32

self.fb.call("__newbf_ct_emit",
    vec![Value::int(owner_id.0 as i128, IrType::I32), ptr, len], IrType::Void);
Some((Value::int(0, IrType::I32), IrType::Void))
```

Notes on the spelled-out plumbing (the bug-prone bits the prior draft hid behind
`/* ... */`):
- **Single evaluation.** `arg` is lowered to `sval` once; `Ptr()`/`Length()` are
  emitted as direct `fb.call`s on the **already-lowered `Value`**, never by re-passing
  the `Expr` through `lower_method_call` (which re-lowers the receiver). This is the
  exact double-eval hazard all three reviews flagged.
- **Methods-table lookup, not `field_addr`.** `String` is a class; field 0 is the
  ClassVData header, so reading `mPtr`/`mLength` by index is off-by-one and fragile.
  The `Ptr`/`Length` method sigs (`MethodSig { full_name, ret, params, is_instance }`,
  `lower.rs:5400`) already encode the layout. (`params.len() == 1` ⇒ only the leading
  `this`, no explicit params.) Treat the `methods[...][name]` index / `.find(...)` as
  expectations that hold whenever corlib is linked; if you prefer, fall back to the
  same diagnostic when absent rather than `unwrap`.
- **Narrowing.** `self.coerce(len64, IrType::I64, IrType::I32)` — NOT a nonexistent
  `fb.trunc`.

**Everything else in the function is unchanged**, including the owner-id literal
injection the loop resolves per-round (comptime-breadth §3.4 / `emit.rs:450`).
`comptime_emitter_of` (`:12065`), the `emit_owner` threading (`:5858,6135,6179`), and
the strip/fold scoping are untouched. The generic-comptime guard stays. **`sema ⊥ llvm`
holds** (this edit names only `__newbf_ct_emit`, `String`, and `Ptr`/`Length`).

### 3.3 Comptime (`newbf-comptime/src/emit.rs`) — no code change, two confirmations

The fixpoint loop, sandbox JIT, dedup/normalize, round/byte caps, and analyze-abort all
work **unchanged** for a reflection-driven generator (`emit.rs:317` analyze-abort,
`:393` dedup, `:351` round cap, `:476` sandbox). Two properties must be *confirmed by
test* (not changed):

1. **`run_generators` sandbox runs reflection as-is** (`emit.rs:476-516`). The wrapper
   `$ct_emit_run` is nullary `void` (`:494-505`) and the generator returns `void`,
   communicating only via the shim — so there is **no struct-return / FFI-marshalling
   problem** and the `eval_const` struct-return gate (`eval.rs:107`, on the *value-fold*
   path) is **not** on this path. Confirm: a generator that calls
   `typeof(T).GetFieldCount()` and binds a `FieldInfo` local for `GetField(i).GetName()`
   JITs and runs in the **sandbox** (T1) — note the existing `reflect_field_*.bf` prove
   this in the *app* JIT, so T1 is the only thing that pins struct-by-value reflection
   returns inside the `$ct_emit_run` wrapper.
2. **`strip_emitter_and_shim` keeps the shared corlib reflection/String methods**
   (`emit.rs:530-571`). The strip is scoped to `module.comptime` ∧ transitively-reaches-
   `__newbf_ct_emit` (`:522`, `:566-571`). The corlib `Type`/`FieldInfo`/`String`
   methods are **not** `module.comptime`, so they survive; the generator's own body
   (comptime, reaches the shim) is dropped — correct. The matching `fold_comptime` drop
   (`fold.rs` `reachable_from_ordinary`) agrees by the same scoping (the comment at
   `emit.rs:527`). Confirm once a generator pulls in `Type.GetField` + `String.Append`.

### 3.4 LLVM / codegen (`newbf-llvm`) — no change

`emit_metadata` (`newbf-llvm/src/lower.rs:399`) is already called for every module by
`emit_module` (`lower.rs:80`), including the sandbox clone. **Every class gets a `Type`
global regardless of `[Reflect]` policy** — the policy gate (`:456-457`,
`policy.has(FIELDS) && !fields.is_empty()`) only governs whether the *FieldInfo array*
is emitted, not whether the `Type` global exists; so an unmarked type's
`GetFieldCount()` reads a real `Type` with `mFieldCount == 0` (the §4.3 differential).
The `Type`/`FieldInfo` globals, the registry table, and the `__newbf_type_by_id`
accessor (`:588`) are emitted identically in JIT and AOT — **no backend change**. The
strip (§3.3) ensures `__newbf_ct_emit` is gone before the final app/AOT codegen, so the
shipped binary never references it (comptime-breadth §5.4).

### 3.5 Runtime / memory-guard interaction (load-bearing — see §6 Risk 4)

A reflection-driven generator builds its text with corlib `String` (`Append`,
`String.bf:203-236`). **Two distinct allocations, only ONE of which the guard sees:**

- The **`String` object body** (`new String(...)` → `construct_string` →
  `heap_alloc(size, AllocKind::Object(id), …)` → `newbf_alloc`, `lower.rs:10048-10055`)
  **is** ledger-tracked. `newbf_alloc` is bound absolute in the sandbox JIT
  (`jit.rs:186`), so it resolves; `delete s` runs `emit_destroy` (`lower.rs:10236-10257`,
  which walks the dtor chain **then** `newbf_free`), balancing the ledger.
- The String's **`char8*` buffer** (`mPtr`) is allocated/grown via `Internal.Malloc` /
  `Internal.Free` (`String.bf:11,18,22; ~249`), which are `[LinkName("malloc")]` /
  `[LinkName("free")]` externs (`Internal.bf:5-6`) resolving to **CRT `malloc`/`free`
  through the JIT process-search generator — NOT routed through the Stomp ledger.** So
  the buffer is invisible to the guard.

> Correction vs the prior draft: the old text cited `String.bf:11,18,249` as
> `newbf_alloc` sites. Those are `Internal.Malloc` (C malloc), not `newbf_alloc`. The
> only ledgered allocation is the **object body** from `new String`.

The run-corpus harness runs the whole pipeline — including `run_emission`'s sandbox JIT
— under `GuardMode::Stomp` (`run_corpus.rs:89`, process-global). **What actually faults
the compiler is a double-free / use-after-free / wild-free**, which the Stomp allocator
detects synchronously (quarantine + poison). A **pure leak does NOT abort**: the
run-corpus harness deliberately does **not** call `report_leaks` and tolerates leaks
(`run_corpus.rs:114-119` — "a leak here must NOT fail the harness"), `guard_reset()`ing
between programs. So:

- The correct generator invariant is **`delete s` exactly once** so the object body is
  neither leaked-but-mainly **not double-freed**, and the buffer is freed by the dtor.
  (`emit_destroy` runs the dtor despite the **stale** `lower.rs:10144` comment that says
  "destructor is deferred" — dtors *do* run; don't trust that comment.)
- The guard hazard to test for is a **double-free** (e.g. `delete s` twice, or freeing a
  `scope`-bound String the automatic frame cleanup also frees — MS-T4 de-registers
  exactly to prevent this). A pure leak would pass run-corpus silently and is not the
  acceptance property.

Acceptance therefore pins **no double-free under Stomp**, not "allocations balance."

## 4. Worked examples (run-corpus programs that prove it)

Each is a self-contained `Program.Main() -> int32` with `// expect: N`, dropped in
`beef-tests/run-corpus/` (resolved relative to the test manifest as
`../../../beef-tests/run-corpus`, i.e. `e:/NewBF/beef-tests/run-corpus/`), run by the
JIT full-i32 harness under the Stomp guard (`run_corpus.rs:35-122`). All compose with
the existing `reflect_field_*.bf` and `comptime_emit_*.bf` corpus.

### 4.1 `comptime_reflect_field_count.bf` — the true minimal slice (**expect: 2**)

The generator reads the reflected field count at compile time and emits a member
returning it. `2` is computable only if the generator saw the two reflected fields and
emitted a member that re-resolves and runs. **This slice needs no corlib addition**: it
uses only `Append(int)` (an `int32 n` argument matches `Append(int)` by `type_affinity`
score 1 — both integers, no exact overload, so `Append(int)` is selected;
`lower.rs:5647-5658`), and `Append("; }")` on a string literal auto-wraps into a
`String` via `coerce` (`lower.rs:11743-11748`). It does **not** depend on T2.

```beef
// expect: 2
[Reflect(.Fields)]
class Pair {
    public int32 mA;
    public int32 mB;

    [Comptime, EmitGenerator]
    public static void Generate() {
        // Reflect at COMPILE TIME: count this type's fields.
        // typeof(Pair) is a Ref(Type) rvalue → GetFieldCount() resolves directly
        // (no value-struct chain). The Type global lives in the sandbox.
        int32 n = typeof(Pair).GetFieldCount();        // 2
        // Build the member source from the reflected count.
        String s = new String("public int32 FieldCount() { return ");
        s.Append(n);                                   // "...return 2"  (Append(int))
        s.Append("; }");                               // literal auto-wraps to String
        Compiler.EmitTypeBody(s);                      // runtime String, NOT a literal
        delete s;                                      // exactly once → no double-free
    }
}
class Program {
    public static int32 Main() {
        Pair p = new Pair();
        int32 r = p.FieldCount();                      // the emitted member returns 2
        delete p;
        return r;
    }
}
```

### 4.2 `comptime_reflect_field_name.bf` — name-driven emission (**expect: 1**)

The generator reads the first field's *name* and emits a predicate that re-derives the
same name at runtime and compares — proving `GetField(i).GetName()` flows through the
sandbox into emitted code. **Both the generator code AND the emitted runtime text bind
a `FieldInfo` local before `.GetName()`** (never chain off the value-struct rvalue —
§2.2, Risk 2). Depends on **T2** (`Append(char8*)`) because `FieldInfo.GetName()`
returns `char8*` (`FieldInfo.bf:27`) and there is no `Append(char8*)` overload today
(§5/T2). The emitted text must also be careful with quoting; the predicate is built so
that what it emits is itself valid Beef that binds a local.

```beef
// expect: 1
[Reflect(.Fields)]
class Tagged {
    public int32 mX;

    [Comptime, EmitGenerator]
    public static void Generate() {
        // The emitted method binds a FieldInfo LOCAL (not a chained rvalue),
        // re-derives the field name at RUNTIME, and compares to the literal the
        // generator read at COMPILE TIME — both must be "mX".
        String s = new String(
            "public bool FirstFieldIsMX() { FieldInfo f = typeof(Tagged).GetField(0); return Internal.StrEq(f.GetName(), \"");
        FieldInfo gf = typeof(Tagged).GetField(0);     // generator-side: bind a local too
        s.Append(gf.GetName());                        // Append(char8*) — needs T2
        s.Append("\"); }");
        Compiler.EmitTypeBody(s);
        delete s;
    }
}
class Program {
    public static int32 Main() {
        Tagged t = new Tagged();
        bool ok = t.FirstFieldIsMX();
        delete t;
        return ok ? 1 : 0;
    }
}
```

> **Note (acceptance-scoping).** `String.Append` overloads cover
> `char8`/`String`/`int`/`bool` (`String.bf:203-236`) but **not `char8*`**.
> `f.GetName()` returns `char8*` (`FieldInfo.bf:27`). So §4.2 needs the
> `String.Append(char8*)` overload from **T2** (a tiny corlib addition — a
> NUL-terminated copy loop, mirroring the `String(char8*)` ctor at `String.bf:14-21`).
> The alternative (`new String(f.GetName())` + `Append(String)`) would also work but
> leaks/needs an extra `delete`; T2 is cleaner. **T4 therefore depends on T2.**

### 4.3 `comptime_reflect_count_zero.bf` — the strip differential (**expect: 7**)

An **unmarked** type reflects `GetFieldCount() == 0` (strip policy, reflection.md §5.2;
the `Type` global still exists, only the FieldInfo array is gated — §3.4); the generator
emits a member returning `0 + 7`. Proves the generator observes the *policy-gated*
metadata (a marked type would emit a different constant). The existing
`reflect_field_count_marked.bf` / `reflect_strip_vs_marked.bf` already prove the
read-side at runtime; this adds the *emit*-side at comptime. Like §4.1, needs no corlib
add (`Append(int)` + literal auto-wrap).

```beef
// expect: 7
class Plain {                                          // NOT [Reflect(.Fields)] → fields stripped
    public int32 mA;
    public int32 mB;

    [Comptime, EmitGenerator]
    public static void Generate() {
        int32 n = typeof(Plain).GetFieldCount();       // 0 (stripped; Type global still present)
        String s = new String("public int32 Code() { return ");
        s.Append(n + 7);                               // (i32) 0 + 7 = 7 → Append(int)
        s.Append("; }");
        Compiler.EmitTypeBody(s);
        delete s;
    }
}
class Program {
    public static int32 Main() {
        Plain p = new Plain();
        int32 r = p.Code();
        delete p;
        return r;
    }
}
```

These three pin: (1) reflection reaches the sandbox (count, no corlib add), (2) field
*names* flow into emitted text (needs T2), (3) the strip policy is observed at comptime.
Plus the existing `comptime_emit_member.bf` (literal path, **expect: 42**) must stay
green — the back-compat gate.

## 5. v1 scope vs explicitly-deferred

**v1 (this design):**
- Relax `try_lower_emit_type_body` (`lower.rs:9874-9907`) to accept a runtime
  `Ref(String)` text arg (keep the literal fast-path; diagnose anything else — §2.2).
- A generator may call `typeof(UserClass)`, `Type.GetFieldCount()`,
  `Type.GetField(i)` (**binding a `FieldInfo` local**, not chaining) +
  `FieldInfo.GetName()/GetOffset()/GetTypeId()`, and build text with `String`
  (`Append(int/String/char8/bool)`, plus a new `Append(char8*)` from T2 for §4.2).
- Fields only. Target type must be `[Reflect(.Fields)]` to see field metadata
  (`emit_metadata` gates the FieldInfo array at `newbf-llvm/src/lower.rs:456`); an
  unmarked type still has a `Type` global and reflects count 0 (the strip differential,
  §4.3).
- Emitted text names members **syntactically** (`this.mX`, `typeof(T).GetField(0)`),
  i.e. it emits Beef *source* that re-resolves — never raw field reads by computed
  offset.

**Deferred (honest):**
- **Methods/attributes at comptime** — `typeof(T).GetMethod(i)` (`Type.bf:74`) and
  querying user-defined attributes. `GetMethods()`-driven dispatch generation is the
  obvious next slice but needs `[Reflect(.Methods)]` plumbing through a generator and
  is out of v1.
- **Generic-T params** — a generator inside `List<T>` reflecting `typeof(T)`. The
  generic-comptime guard stays (comptime-breadth §1.2); `typeof(generic-T)` is itself
  deferred in reflection.md §10.
- **Reading field *values* by offset** — `FieldInfo.mOffset` is a real DataLayout
  offset (`newbf-llvm/src/lower.rs:466-472`), but a generated serializer doing pointer
  arithmetic + typed reads at runtime is a hazard (research Risk #5). v1 emits text
  that *names* fields, not offset-indexed reads.
- **Reflecting over emitted-this-round members** — a field added by emission in round
  *k* is not reflected until round *k+1* (`emit_metadata` reads `type_meta` populated
  by that round's `lower_program`, `emit.rs:337`). Fixpoint-ordering hazard; v1
  generators reflect only pre-existing fields.
- **`typeof`/reflection on the value-fold path** — `eval_const` rejects struct/ptr
  returns (`eval.rs:107`); reflection-driven emission stays strictly on the
  `run_generators` void+shim path, never on `fold_comptime`.
- **Any new sema metadata-reading Rust code** — generators read reflection only via the
  emitted corlib `.bf` API (§3.0).

## 6. Load-bearing risks & mitigations

1. **SSA dominance.** `typeof(T)` is a constant `GlobalAddr` (`lower.rs:9655`, no
   operands) ⇒ dominates trivially; the query-method calls and the new
   `Ptr()`/`Length()`/`coerce` are emitted inline at their use sites (the receiver
   dominates). No new block, no phi. *Mitigation:* structural — the rewrite emits only
   straight-line IR, like the existing literal path. The verify corpus (LLVM-clean
   ratchet) is the gate.
2. **Value-struct method-chain trap (highest-probability bug).** `GetField(i)` returns
   a value-struct `FieldInfo` **by value** (`Type.bf:57`, `FieldInfo.bf:20`); chaining
   `typeof(T).GetField(0).GetName()` fails because `struct_base` rejects a `Struct(id)`
   rvalue (only `Ref` rvalues flow, `lower.rs:9598-9606`). The String ABI is also a
   class with `Ptr()`/`Length()` *methods* (not `.Ptr`/`.Len`), `Length()` is i64, and
   field 0 is the ClassVData header. *Mitigation:* (a) **bind a `FieldInfo` local**
   before `.GetName()` in BOTH generator code and emitted runtime text (§4.2); (b) read
   `Ptr`/`Length` via the **methods-table lookup**, never `field_addr` (§3.2);
   (c) narrow length with `coerce(I64,I32)`, never a nonexistent `fb.trunc`. A sema unit
   test asserts the rewritten generator body contains `call __newbf_ct_emit(i32, ptr,
   i32)` with no residual `EmitTypeBody` (mirroring the existing test at
   `lower.rs:14120`).
3. **Single-evaluation / no silent decline.** The relaxed seam must NOT lower the arg
   and then `return None` (the caller re-lowers it at `lower.rs:7657-7659` → double
   emit), and must NOT decline a non-String/non-literal into the empty
   `Compiler.EmitTypeBody(String)` stub (`Compiler.bf:38-39`, which would
   auto-wrap+silently drop the emission). *Mitigation:* §3.2 lowers the arg exactly once
   and emits a real diagnostic for anything that is neither a literal nor `Ref(String)`.
   A unit test feeds a non-String, non-literal arg and asserts a diagnostic (not a
   silent no-op) and asserts a side-effecting `String` builder arg is evaluated once.
4. **Memory-safety under the guard (sandbox String allocation).** The generator's
   **`new String` object body** routes through `newbf_alloc` → the Stomp ledger
   *during compilation* (`run_corpus.rs:89`); the **char buffer** uses CRT
   malloc/free and is NOT guard-tracked (§3.5). A **double-free / UAF** in a generator
   (e.g. `delete s` twice, or a `scope` String freed twice) faults the **compiler**
   (Stomp quarantine, no SEH recovery; comptime-breadth §7). A **pure leak does NOT
   abort** — run-corpus tolerates leaks and never calls `report_leaks`
   (`run_corpus.rs:114-119`). *Mitigation:* §4 generators `delete` their String
   **exactly once**; an integration test asserts a reflection generator runs **with no
   double-free** under Stomp (NOT "balance"). (The emitted *member* runs in the app JIT
   later, also under Stomp — covered by the normal run-corpus gate.)
5. **Sandbox completeness.** Relies on `emit_metadata` running in the sandbox
   (`emit.rs:499` → `jit.rs:123` → `lower.rs:80`). The existing `reflect_field_*.bf`
   prove reflection in the *app* JIT, not the `$ct_emit_run` sandbox wrapper.
   *Mitigation:* T1 JITs a **sandbox-shaped** `from_ir` module and looks up
   `__newbf_type_by_id` + a `Type` global, **and** runs a generator that binds a
   `FieldInfo` local + reads `GetName()` inside the wrapper — pinning struct-by-value
   reflection returns there. If a future change ever gates `emit_metadata`, this test
   fails loudly.
6. **Monomorph keying / determinism.** `GetFieldCount`/iteration order is declaration
   order, stable across rounds (`emit_metadata` sorts `metas` by type-id,
   `newbf-llvm/src/lower.rs:438`; field order is declaration order). Emitted text must
   be byte-stable round-to-round or the `seen` dedup (`emit.rs:393`) never converges
   and trips the round cap (`:351`). *Mitigation:* generators build text from a stable
   reflection iteration; §4 emits a *single* idempotent member. A monomorph generator
   is deferred (§5).
7. **Strip/fold agreement.** A generator now pulls in corlib `Type`/`FieldInfo`/`String`
   methods; the strip (`emit.rs:530`) and `fold_comptime`'s `reachable_from_ordinary`
   must both keep them (they're not `module.comptime`) and both drop the generator
   (it is). *Mitigation:* the existing scoping already does this (comment at
   `emit.rs:527`); T3's acceptance asserts the corlib methods survive and the generator
   + `__newbf_ct_emit` are gone in the final module (a `dump-ir`-style assertion,
   mirroring the JIT-and-run strip test at `emit.rs:679-683`).

## 7. Task breakdown

Ordered; each task is agent-assignable with a one-line seed and a concrete acceptance
gate. **T0-T1 are behavior-preserving** (the relaxation is a no-op until a generator
uses it; all existing corpora stay green via the literal fast-path). **T3-T4 are
behavior-changing**, each pinned by a run-corpus `// expect:` program. **T2 is a
prerequisite of T4 only** (the count-only marquee §4.1/§4.3 do NOT need it).

**T0 — Relax `try_lower_emit_type_body` to accept a runtime `Ref(String)` arg
(plumbing-heavy; the keystone).**
Seed: in `newbf-sema/src/lower.rs:9874-9907`, replace the `[Expr::Str(s)]` arm
(`:9891`) with the two-case shape of §3.2 — literal fast-path decided from the AST
(unchanged); else lower the arg **exactly once**, require `Ref(String)`, read
`Ptr()`/`Length()` via the methods-table lookup (`lower.rs:10133` pattern), narrow with
`coerce(I64,I32)`, and emit `__newbf_ct_emit(<owner>, ptr, i32 len)`; **diagnose**
(don't silently decline into the stub) anything that is neither a literal nor a
`String`.
Accept: (a) a sema unit test (mirroring `lower.rs:14120`+) shows a generator passing a
**non-literal** `String` lowers to `call __newbf_ct_emit(i32, ptr, i32)` with no
residual `Compiler.EmitTypeBody`; (b) a unit test shows a side-effecting `String`
builder arg is evaluated **once** (no duplicate alloc in the IR) and a non-String,
non-literal arg yields a **diagnostic** (not a no-op); (c) **all existing corpora
unchanged** (literal path untouched), including `comptime_emit_member.bf`
(**expect: 42**).

**T1 — Sandbox-reflection confirmation test (`newbf-comptime`) — a HARD gate, not a
confirmation.**
Seed: in `emit.rs` tests, add an integration test that drives `run_emission` over a
program whose `[Comptime, EmitGenerator]` calls `typeof(T).GetFieldCount()` **and binds
a `FieldInfo` local for `GetField(0).GetName()`** inside the generator, and emits a
member; assert the final module JIT-links clean and the corlib `Type`/`FieldInfo`
methods survive the strip while the generator + `__newbf_ct_emit` are gone.
Accept: the test passes; a companion unit test JITs a `from_ir` **sandbox-shaped**
module and (i) looks up `__newbf_type_by_id` + a `Type` global and (ii) runs a wrapper
that exercises a value-struct `FieldInfo` return inside `$ct_emit_run` (pins Risk 5 —
struct-by-value reflection present and callable in the sandbox, not just the app JIT).

**T2 — Corlib `String.Append(char8*)` overload (prerequisite of T4 only).**
Seed: add `public void Append(char8* s)` to `newbf-corlib/bf/String.bf` (a
NUL-terminated copy loop, mirroring the `String(char8*)` ctor at `:14-21`); selected by
arg type over the existing `char8`/`String`/`int`/`bool` overloads.
Accept: a standalone run-corpus smoke (`string_append_cstr.bf` — specifically the
`char8*` overload, e.g. `new String(); s.Append(somePtr); StrEq(...)`) → **expect: 1**;
the existing `append_overload.bf` (**expect: 5427**, covers `Append(String)/char8`) and
`string_append_int.bf` (**expect: 1591**) stay green; verify corpus stays clean.

**T3 — The count marquee: reflection-driven field-count emission (run-corpus).**
Seed: land `comptime_reflect_field_count.bf` (§4.1, **expect: 2**) and
`comptime_reflect_count_zero.bf` (§4.3, **expect: 7**) in `beef-tests/run-corpus/`.
Both use only `Append(int)` + literal auto-wrap, so **no T2 dependency**.
Deps: T0, T1.
Accept: both pass under the JIT full-i32 Stomp harness; the final module JIT-links and
AOT-links clean (the strip property); an integration test asserts the generator runs
under Stomp with **no double-free** (Risk 4 — not "balance").

**T4 — Name-driven emission (run-corpus).**
Seed: land `comptime_reflect_field_name.bf` (§4.2, **expect: 1**) using the T2
`Append(char8*)` overload; the emitted runtime text **binds a `FieldInfo` local**
before `.GetName()` (Risk 2). Deps: **T0, T1, T2**.
Accept: passes under the Stomp harness; a `dump-ir` golden shows the emitted predicate
member present and the generator + `__newbf_ct_emit` absent.

**T5 — Docs + journal (behavior-preserving).**
Seed: cross-link this doc from `docs/COMPTIME.md` and `reflection.md` §10 (resolve the
"Comptime reflection deferred" note to "v1 landed, fields"); add a journal entry
pairing the feature commits. Deps: T0-T4.
Accept: docs build; journal entry references the T3/T4 corpus values.

**Critical path:** T0 (the single-eval methods-table rewrite + diagnostic) → T1 (the
sandbox struct-by-value reflection pin) → T3 → T4. T2 sits on the T4 branch only.

**Staged beyond v1 (recorded, §5):** comptime *method*/attribute reflection +
`GetMethods()`-driven dispatch generation; generic-T reflection (lift the
generic-comptime guard); field-*value* serialization by offset; reflecting
emitted-this-round members; bounded/recoverable generator execution.
