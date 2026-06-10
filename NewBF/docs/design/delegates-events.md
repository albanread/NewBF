# Multicast delegate types + event declarations

> **Status: design (implementation-ready, code-grounded, hardened).** Wave-4 feature.
> Builds directly on the landed WAVE-1 function-value machinery
> ([`fn-values.md`](fn-values.md): the `$Func` two-word fat pointer + the one
> uniform `code(target, args…)` call shape) and the monomorphized prelude
> `List<T>` ([`List.bf`](../../src/newbf-corlib/bf/List.bf)). Every `file:line`
> anchor below was **re-verified** against the live tree at the §95 wave (code lives
> under `NewBF/src/<crate>/...`; re-grep before editing — `lower.rs` (15.6k lines)
> drifts a few lines per commit). Modeled on
> [`iterators.md`](iterators.md) / [`comptime-reflection.md`](comptime-reflection.md).
>
> This revision hardened three load-bearing flaws found in adversarial review
> (recorded inline at the seams): (1) a named delegate type has **no data path** to
> its signature in the lowering pass — the def-graph `DelegateSig` is unreachable —
> so a new **`StructTable` delegate-registration pass** is required (T1, §3.2);
> (2) `event` **cannot** be a reserved keyword (it is already a parameter identifier
> in two corpus-gated `corlib-slice` files) — it must be a **contextual keyword**
> (T2, §3.1); (3) value-struct **field/local destructors are never chained** by the
> current machinery, so the `Multicast` buffer is **not** freed in `~this()` — v1
> uses a `DisposeHook` for `scope`-local events and **documents the buffer leak**
> for embedded-field events (no "exactly-once free" claim; §3.3/§6).

## 1. Overview & the v1 capability

NewBF today has a **single-target** function value (`$Func = {code:Ptr, target:Ptr}`,
registered **first** at `StructId(0)` by `register_func_struct`, `lower.rs:863`, in
`StructTable::build` step 0, `lower.rs:462`) with one uniform call shape — but no way to
hold *more than one* target, no `event` syntax (the keyword does not exist:
`newbf-lexer/src/token.rs` keyword table, lines 96-122, has `Delegate`/`Yield` but
**no** `Event`), and a top-level `delegate` declaration is parsed into both the AST
(`Item::Delegate`, `parser.rs:2631-2639`, carrying `return_ty`/`params` directly) and the
separate def-graph (`build.rs:116-153`, `model.rs:138` `DelegateSig`) but **never lowered**
(`lower.rs:5849` `_ => {}` skips `Item::Delegate`; `ty_of`, `lower.rs:646-656`, only knows
`Value`/`Ref`/`Interface`) and **never registered in `StructTable`** (`register_struct_names`,
`lower.rs:2574-2584`, matches only `Item::Type`). So a named delegate type today resolves to a
bare opaque `Ptr` (via `lower_ty_env`) and is **not even callable**.

**v1 capability (one paragraph).** Introduce a **concrete multicast delegate** as a
corlib value-struct (`Multicast` — a growable, heap-backed list of `$Func` entries) plus
an **`event`** member that declares a backing `Multicast` field with the
subscribe/unsubscribe/raise verbs wired in: `e += f` appends a target (a lambda /
static-method-ref / bound-method-ref, each already a `$Func`-coercible producer,
`lower.rs:8181`/`9962`/`10011`), `e -= f` removes the first structurally-equal target
(comparing **both** `code` and `target` fields), `e.Invoke(args)` / `e(args)` iterates the
list **in subscription order** and `call_indirect`s each entry through the existing uniform
`code(target, args…)` shape (`lower.rs:8365-8392`), `e.Count` tests emptiness, and `e += f`
on an empty event lazily initializes the backing buffer. The backing store is a **bespoke
`$Func*` buffer** whose block is **sized by the real `size_of_ty($Func)` = 16 bytes** (via
`alloc_array`, `lower.rs:10460-10470`, which multiplies `count * size_of_ty(elem)`) and
**indexed with an explicit `Struct(func_struct)` element type** (the 16-byte stride is
enforced at every `elem_addr` use-site, §2.1) — **NOT** a `List<$Func>`, because `List<T>`
hardcodes an 8-byte slot stride (`List.bf:16,140`: `Malloc(cap * 8)`) that would **truncate**
a 16-byte `$Func` (§2.1, the central representation decision). v1 ships a **concrete**
(non-generic) delegate (`delegate void Notify(int32)` shape) + concrete events; **generic**
delegates (`Action<T>`), the full upstream `Event<T> where T : Delegate`, delegate variance,
`is`/`as` on delegate types, and `async` are explicitly deferred (§5).

The marquee proofs (§4): `delegate_concrete_call.bf` (a named-delegate-typed local holds a
single `$Func` and is callable), `mcast_manual.bf` (the bespoke 16-byte buffer holds two
`$Func`s with no aliasing), `event_multicast_two.bf` (two subscribers, one raise, both run in
order → summed side effects), `event_unsubscribe.bf` (`-=` removes one target, the remaining
one still fires).

## 2. Representation / ABI / IR changes

### 2.1 The central representation decision — a bespoke 16-byte `$Func` buffer, NOT `List<$Func>`

A multicast must hold N `$Func` values. The obvious reuse — `List<$Func>` — **does not work**,
and the reason is load-bearing:

- `$Func` is a 16-byte value struct (`StructId(0)`, two `Ptr` fields; `size_of_ty(Struct id)`
  → `self.fb.size_of(id)`, `lower.rs:10362-10364`, which defers to the LLVM DataLayout = 16
  for `$Func`).
- `List<T>` over-allocates a **fixed 8-byte slot stride** independent of `T`: the ctor
  `Malloc(this.mCap * 8)` (`List.bf:16`) and `Grow` `Malloc(nc * 8)` (`List.bf:140`) both
  hardcode `* 8` (documented at `List.bf:5-8` as "over-allocates 8 bytes per slot (the max
  scalar width) pending a sizeof(T) operator"). Indexing `mItems[i]` is then a typed-pointer
  step by `T`'s stride — for a 16-byte `T` this **overruns the 8-byte-strided buffer** and
  the second element's high word aliases the first element's `target`. A `List<$Func>` is a
  silent heap corruption (and the Stomp guard would flag it as an out-of-bounds write — a
  guard fault, not a clean miscompile).

The **two pieces of the fix are already in the tree**, but they are *separate* and the
implementer must use both:

- **Allocation sizing.** `alloc_array` sizes the block by the *real* element size
  (`let esz = self.size_of_ty(elem); bytes = count * esz`, `lower.rs:10461-10462`), so
  `alloc_array(n, Struct(func_struct), …)` allocates `8 + n*16` bytes — correctly sized.
  (Its only internal `elem_addr` is `elem_addr(block, IrType::U8, 8)` at `lower.rs:10468-10469`,
  which skips the 8-byte length header and returns a pointer to the *elements*; that line is
  the header skip, **not** per-element striding — see the citation correction below.)
- **Per-element striding.** The 16-byte stride is enforced **at every use-site** by passing the
  explicit `elem = Struct(func_struct)` to `elem_addr(buf, elem, i)` (the pattern at
  `lower.rs:10499`). **This is the load-bearing invariant:** because the field type
  `function void()*` lowers to a bare `Ptr` at a field position (below), *nothing* forces the
  16-byte stride — the implementer **must** pass `Struct(func_struct)` to `elem_addr` at every
  Add/Get store/load. (A bare `Ptr`/`U8` element there would silently 8-byte-stride.)

So a raw heap array of `$Func` (from `alloc_array(n, Struct(func_struct), …)`, indexed with
`Struct(func_struct)`) is correctly 16-byte-strided and is the right backing store. The corlib
delegate type owns such a buffer directly.

> *Citation correction (was wrong in the draft):* the draft cited `lower.rs:10468-10469` as
> proof that `alloc_array` "steps by the element type's stride." It does **not** — those lines
> are `elem_addr(block, IrType::U8, 8)`, the header skip. Block *sizing* by the real element
> size is `lower.rs:10461-10462`; per-element *striding* is the separate use-site `elem_addr`
> call with `elem = Struct(func_struct)`.

**Decision: a corlib value-struct `Multicast`** with the shape

```beef
// newbf-corlib/bf/Delegate.bf  (new file, added to prelude())
// A concrete multicast delegate backing store: a growable buffer of $Func
// entries, 16-byte-strided (NOT a List<T> — that hardcodes an 8-byte stride,
// List.bf:16/140, which truncates a 16-byte $Func). The element type at every
// index site is the $Func value-struct; `function void()` is its v1 surface
// signature. Add/Get/Grow are HAND-EMITTED in sema (§3.3) so the 16-byte stride
// is explicit — the body below is illustrative, not the lowered source.
struct Multicast {
    function void()* mItems;   // an opaque Ptr-to-buffer at the field position (§2.1)
    int mCount;
    int mCap;

    // `+= f` — append a target, lazily allocating/growing the buffer. A
    // zero-initialized (default) Multicast has mItems == null / mCount == 0, so
    // the first Add allocates. (Equivalent to upstream Event's lazy mData.)
    public void Add(function void() f) mut { … grow if needed; mItems[mCount] = f; mCount += 1; }
    // `-= f` — remove the FIRST structurally-equal entry (both code+target).
    public void Remove(function void() f) mut { … shift down; mCount -= 1; }
    public int Count() { return this.mCount; }
    public function void() Get(int i) { return this.mItems[i]; }
    // Dispose() frees the buffer; invoked via a DisposeHook for `scope`-local
    // events (§3.3). NOT a ~this() — value-struct field/local dtors are never
    // chained by emit_destroy/scope-cleanup today (§3.3, the hardened finding).
    public void Dispose() mut { if (this.mItems != null) { Internal.Free(this.mItems); this.mItems = null; } }
}
```

> **Why a `function void()` element, not a generic `T`.** v1 is a *concrete* delegate. The
> field type `function void()*` lowers — at a **field/cast position**, where the `$Func`
> position-gating does **not** apply (`lower_value_ty` is threaded only into param/local/return
> sites, NOT `fill_fields_at`; `lower_ty_env(Function)` is bare `Ptr`, `lower.rs:13186`/§5.0 of
> fn-values.md) — to a bare `Ptr`. **This is why hand-emit is mandatory** (§3.3): the buffer
> must be 16-byte-strided, but the field-position lowering gives no `$Func` element type, so T1
> plumbs the explicit `Struct(func_struct)` at the `Add`/`Get`/`alloc_array`/`elem_addr` seams.
> The alternative — widening `function`-typed fields to `$Func` — is rejected (it is the exact
> `BfRtCallbacks` C-ABI-layout regression fn-values.md §3/§5.5 forbids).

### 2.2 The `event` member → a synthesized `Multicast` field + verb dispatch

An `event Notify N;` member (Notify a concrete delegate type, §3.1) lowers to:
- a backing **field** `N : Multicast` (a value-struct field, in-place mutable through
  `field_addr`, `lower.rs:12544` shows the `field_addr`/`store` shape), zero-initialized
  (`mItems = null, mCount = 0, mCap = 0`) by the existing field default-init path, and
- a member-name binding so `e.N += f` / `e.N -= f` / `e.N.Invoke(args)` resolve `N` to that
  field and dispatch the verb (§3.4/§3.5), **plus** an entry in a synthesized-event set so the
  dispatch can distinguish an `event` field from a plain user `Multicast` field (§3.5).

No new `IrType`, no new `InstKind`. `Multicast` is an ordinary value struct (`IrType::Struct`),
the buffer is an ordinary `alloc_array`'d `Ptr`, and invocation reuses the existing
`call_indirect` (`inst.rs` `CallIndirect{callee, args}`, the one indirect shape) — so
**newbf-ir and newbf-llvm need ZERO changes** (mirrors fn-values.md §4 / iterators.md §2.1).

### 2.3 The sema ⊥ llvm contract (what sema emits by-name)

The HARD INVARIANT (sema must not depend on newbf-llvm) is preserved trivially: every new
operation is a **direct `self.fb.call(symbol, args, ret)`** against a corlib `Multicast` method
symbol sema itself mangled (`Multicast.Add`, `Multicast.Remove`, `Multicast.Count`,
`Multicast.Get`, `Multicast.Dispose`), plus the existing `field_addr`/`load`/`call_indirect`
primitives. The contract table:

| Operation | Emission | Symbol / shape |
|---|---|---|
| `e += f` | `field_addr(owner, MulticastField)` (the in-place field address from `lvalue`) → `call("Multicast.Add", [field_ptr, coerce(f → $Func)], Void)` | `Multicast.Add` mangled symbol |
| `e -= f` | `field_addr` → `call("Multicast.Remove", [field_ptr, coerce(f → $Func)], Void)` | `Multicast.Remove` |
| `e.Invoke(args)` / `e(args)` | loop `i in 0..Count`: `entry = call("Multicast.Get",[field_ptr,i],$Func)`; **spill `entry` to a fresh alloca**; load `entry.code`/`entry.target`; `call_indirect(code,[target,args…])` (the §3.5 invoke-all) | reuses `lower.rs:8368-8392` shape |
| `e.Count` (emptiness) | `call("Multicast.Count",[field_ptr],i32)` → compare `== 0` where needed | `Multicast.Count` |
| structural `$Func` equality (for `-=`) | `func_eq(a,b)` = `a.code==b.code && a.target==b.target` (new helper — **NOT** a trivial extension of `func_code_field`, `lower.rs:12554`, which reads field 0 ONLY; §3.4) | two `field_addr`+`load`+`cmp` pairs `and`-ed |

`Multicast`'s layout is ABI-pinned by a unit test (the same posture as the `Type`/`FieldInfo`
layout pins): `{ function void()* mItems; int mCount; int mCap; }` = `{Ptr, i64, i64}`.

> *Semantics correction (was loose in the draft):* there is **no** `null` value for a value-struct
> `Multicast` (it is never a pointer), so `e == null` as a surface form is **dropped from v1**;
> emptiness is `e.Count == 0` only.

### 2.4 No metadata, no mangling change

`Multicast` is an ordinary monomorph-free corlib value struct; its methods mangle like any
`List` method. The `$mref$`/`$mrefb$` thunks subscribers already produce are unchanged
(`lower.rs:9978`/`10039`). No new monomorph key, no new symbol namespace, no generic
mangling change (the concrete delegate signature `void(int32)` does **not** parametrize
`Multicast` in v1 — the element is the signature-agnostic `$Func`; §5 generic-delegate is the
deferred lift).

## 3. Concrete changes (parser + sema + llvm + runtime), with seams

### 3.1 Parser — `event` as a CONTEXTUAL keyword + member (parser, `parser.rs`)

**`event` MUST NOT become a reserved `Keyword`.** It is already used as a plain **identifier**
in two parser-corpus-gated `corlib-slice` files: `Platform.bf` (parameter name `BfpEvent* event`,
plus `event.mSet`/`delete event`/`&event`, lines 391-433) and `Event.bf` (`public this(ref
Event<T> event)`, lines 232-234). Both feed the parser corpus (`newbf-parser/tests/corpus.rs:40`
collects `corlib-slice`; the 100%-clean floor is asserted at `:79-84`), the lexer corpus, the
sema verify corpus (`newbf-sema/tests/corpus.rs`), and the constraints corpus. Reserving `event`
globally would turn every `event` identifier into a syntax error → **the parser ratchet breaks
on day one.** (The draft's §6 claim "no corpus file uses `event`" checked only for the *keyword*,
not for `event`-as-*identifier*, and was false.)

`event` is therefore a **contextual keyword**, recognized **only** as a member-declaration leader
inside `member()`, exactly as `get`/`set` (property accessors, `parser.rs:3760-3762`) and `not`
(`parser.rs:278`) already are via the existing `at_ident_text`/`eat_ident_text` helpers
(`parser.rs:114-119`). Changes:

1. **Lexer:** **none.** `event` stays a normal `Ident`. (No `Keyword::Event`.)
2. **AST:** a new `Member::Event` variant (modeled on `Member::Field`, `ast.rs:864-876`):
   ```rust
   // ast.rs, alongside Member::Field:
   /// `event DelegateType Name;` — a multicast event member. Lowers to a backing
   /// `Multicast` field + the += / -= / invoke verbs (delegates-events.md §3.4).
   Event { span: Span, attributes: Vec<Attribute>, modifiers: Vec<(Modifier, Span)>,
           delegate_ty: Type, name: Span },
   ```
3. **Parser:** in `member()` (`parser.rs:3102`), **before** the `let ty = self.ty()` field/method
   fall-through (`parser.rs:3290`), add a **disambiguated** contextual arm: when
   `self.at_ident_text("event")` **and** the lookahead is a type-then-name-then-`;` shape (so a
   bare local/expression named `event` is not misread — `member()` only runs at member position,
   but guard the lookahead anyway), `bump()` the `event` ident, parse a type (`self.ty()`),
   parse the name `Ident`, `expect(Semicolon)`, emit `Member::Event`. This mirrors the field
   shape but is keyed by the leading contextual `event`.

**Walker audit (the iterators.md §3.3 safety net — REAL only for the exhaustive walks).**
Adding a `Member` variant forces an arm only where the `match` is exhaustive. Verify per-walk
and **hand-edit** the wildcard ones (the compiler will NOT flag a missed wildcard — a silent
skip):
- `Member::span()` and `print.rs::member` — exhaustive (no `_`), **forced** (compile error
  until added). The `print.rs` arm must round-trip `event T N;` for the parser corpus.
- every sema member-walk (member registration in `register_type_struct`, `build.rs`'s member
  loop) that currently matches `Member::Field` — hand-edit to also synthesize the backing
  `Multicast` field for `Member::Event` (§3.3) **and** record the (owner, name) in the
  synthesized-event set (§3.5). **T2 ships a focused test** that an `event`-bearing class
  registers the backing field (the wildcard-skip would otherwise drop the event silently).

> The `delegate` *item* parser already exists (`parser.rs:2605-2640`, `delegate_item`) for
> top-level `delegate void Notify(int32);` type declarations — **no parser change there**.

### 3.2 Registering a concrete delegate type into `StructTable` so a delegate local is callable (sema)

**This is the hardened load-bearing fix.** The draft framed named-delegate callability as a small
extension to `lower_value_ty`/`fn_sigs` that would "resolve the name to its `DelegateSig`." That
is **not possible as written**: `DelegateSig` lives only in the **def-graph** (`model.rs:138`,
built by `build.rs:116-153`), and the def-graph is **structurally unreachable** from the lowering
pass. Concretely:
- `lower_program(files, _program)` (`lower.rs:5466`) **underscores/ignores** the def-graph
  `_program`; lowering is driven entirely off `StructTable::build(&all)` (`lower.rs:5541`), built
  from the **raw source AST** (`&[SourceFile]`).
- `lower_value_ty` (`lower.rs:13179`) is a **free function** with signature
  `(ty, src, &StructTable, env)` — it has the `StructTable` and the `TyEnv`, and **nothing else**.
- the `fn_sigs` registration sites (param `lower.rs:6710-6723`, local `lower.rs:7255-7268`) see
  only `self.structs`/`self.env`.
- a delegate name **never enters `StructTable`** today (`register_struct_names`, `lower.rs:2574`,
  matches only `Item::Type`; `lower_items` skips `Item::Delegate`, `lower.rs:5849`), so
  `ty_of("Op")` returns `None`.

So there is **no data source** for `lower_value_ty`/`fn_sigs` to consult. **T1 adds a new
`StructTable` delegate-registration pass** (the signature is right there in the AST — no def-graph
needed):

- **New `StructTable` state + pass.** Add `delegate_sigs: HashMap<String,(IrType,Vec<IrType>)>` to
  `StructTable` (owned `String` keys → owned IR types; satisfies "StructTable owns all its data",
  no lifetimes). In `StructTable::build`, add a pass mirroring `register_struct_names` that walks
  `Item::Delegate` nodes with `generic_params.is_empty()` (arity 0), lowers `(return_ty, params)`
  to `(IrType, Vec<IrType>)` via `lower_ty_env` (the `Item::Delegate` AST carries them directly,
  `parser.rs:2635-2638`), and inserts into `delegate_sigs`. (Generic delegates, `arity > 0`,
  are **not** registered → deferred §5.) Optionally key a `delegate_names: HashSet<String>` off
  the same set for cheap path-is-delegate checks.
- **`lower_value_ty` consults it.** In `lower_value_ty` (`lower.rs:13180-13184`), after the
  inline-`function` arm, when `ty` is a single-segment path whose name is in
  `structs.delegate_sigs`, return `Struct(structs.func_struct)` (the named concrete delegate
  aliases to `$Func`).
- **`fn_sigs` registration consults it.** At both sites (`lower.rs:6710-6723`/`7255-7268`),
  when the declared type is a path resolving to `structs.delegate_sigs`, insert that `(ret, ptys)`
  into `fn_sigs` — making `f(args)` on a delegate-typed local lower through the **same** uniform
  indirect-call path (`lower.rs:8365-8392`) an inline `function` local already uses.

This is additive: the existing inline-`function` path is untouched; the new path is a
*named-concrete-delegate ⇒ same `$Func` treatment* alias, fed by the new `delegate_sigs` map.
(No `event` is needed for this slice — `delegate_concrete_call.bf` proves a bare delegate local,
the smaller half.)

### 3.3 The corlib `Multicast` value-struct + the 16-byte-buffer plumbing (corlib + sema)

Add `newbf-corlib/bf/Delegate.bf` (the §2.1 shape) and register it in `prelude()`
(`newbf-corlib/src/lib.rs`, the flat list `List.bf` is in). The **load-bearing subtlety** is the
buffer element type: `function void()*` at a **field position** lowers to a bare `Ptr`-to-buffer
(field positions never use `lower_value_ty`, §2.1), and `mItems[i]` inside `Multicast` must step
by **`sizeof($Func)` = 16**, not by the field's `Ptr` stride. The chosen path:

- **(chosen) hand-emit `Multicast.Add`/`Get`/`Grow` in sema** (like the auto-property getter,
  `lower.rs:6036-6047`, and `try_property_get`, `lower.rs:9395-9405`, which build bodies with
  `field_addr`/`load`/`call` directly): emit `Add(this, f:$Func)` as `alloc_array`/grow with
  `elem = Struct(func_struct)` (correctly **sizes** the block, `lower.rs:10461-10462`) and store
  via `elem_addr(buf, Struct(func_struct), count)` (correctly **strides** by 16, §2.1); `Get` as
  `elem_addr(buf, Struct(func_struct), i)` + `load` `$Func`. **The explicit `Struct(func_struct)`
  at every `elem_addr` is the invariant** — without it the field-`Ptr` element silently 8-byte-
  strides (§2.1).
- **(rejected) write `Multicast` in `.bf` source.** A `new (function void())[n]` array element
  cannot route to `$Func`: `array_elem_ty` (`lower.rs:10377-10389`) matches only `Expr::Ident`/
  `Expr::Paren`, so a `function void()` *type-expression* element falls to its `_ => None` arm —
  the array-new **can't even size** the element (it returns `None`, it does **not** "give `Ptr`",
  as the draft mis-stated). There is no source surface to express a `$Func`-strided array element,
  so hand-emit is mandatory and is the acceptance gate.

**Buffer-free lifetime (hardened — the draft's "exactly-once free" claim was unsupported).**
The current machinery does **not** chain value-struct destructors:
- `emit_destroy` (`lower.rs:11067-11089`) walks ONLY the class's own dtor chain (`dtor_of(cid)` +
  `bases[cid]`) then `newbf_free` — it **never recurses into value-struct fields**. So a
  `Multicast` *field* embedded in a class would **never** get `~this()` called on the owner's
  `delete`.
- scope cleanup registers an alloc only for `IrType::Ref` (`lower.rs:8520`), so a value-struct
  *local* `Multicast` is **never** scope-tracked, and `free_scope_alloc` (`lower.rs:6983-7017`)
  runs dtors only for `Ref` allocs + foreach `DisposeHook`s.

Therefore v1 does **not** claim auto-free via `~this()` (that `~this()` would be dead code).
Instead:
- **`scope`-local events** are freed via the existing **`ScopeAlloc::DisposeHook`** seam
  (`lower.rs:7003-7015`, the same hook `ListEnumerator.Dispose` uses for foreach): when an
  event-bearing `scope` local is created, register a `DisposeHook` calling `Multicast.Dispose`
  (which frees `mItems`) on scope exit. This is the **exactly-once-free** path and the only one
  with a guard-relevant free.
- **`event` fields embedded in a heap class leak their buffer** (documented — benign under the
  Stomp guard, a skipped free, consistent with fn-values §10's env-leak posture). Adding
  value-struct *field*-dtor chaining to `emit_destroy` is a real, separable mechanism and is
  **deferred** (§5); v1 does not depend on it.

**Value-copy of a `Multicast` is forbidden.** All `Multicast` methods take `this` **by address**
(`mut this` = pointer); an `event` field is never loaded by value. A by-value copy that later ran
`Dispose`/free would double-free the shared `mItems` → a Stomp **double-free abort** (not a benign
leak). The §3.4 verbs pass the field **address** precisely to avoid this; the invariant is pinned
in §6 and by a scoped-event guard test (§4 ex 5).

### 3.4 The `event` verb dispatch — `+=` / `-=` (sema, `assign`)

`e += f` and `e -= f` reach `assign` (`lower.rs:12374`) as `AssignOp::Add`/`AssignOp::Sub` over a
`Member`/`Ident` target (`AssignOp::Add`/`Sub` confirmed at `ast.rs:145-146`; `compound_op` maps
them at `lower.rs:12410`+). The flow in `assign` is: RHS lowered first (`self.expr(value)`,
`lower.rs:12388`), then `self.lvalue(target)` (`lower.rs:12391`) which **already returns the field
address** `(slot, ty)`, then `coerce(rhs, rhs_ty, ty)` **to the lvalue type** (`lower.rs:12392`),
then the generic `compound_op` block (`lower.rs:12410`) which for a `Struct` op= with no
`operator+` falls to `arith` (`lower.rs:12422`) — **ill-typed on a struct.** So the event
special-case **must intercept after the `lvalue` at `:12391` and BEFORE the `:12392` coerce**
(coercing a lambda to `Multicast` is meaningless), reusing the `slot` already in hand:

```text
// in assign(), right after `lvalue` (lower.rs:12391) returns (slot, ty),
// and BEFORE the `:12392` coerce: when op ∈ {Add, Sub} and `ty` is the
// `Multicast` struct AND the target is a synthesized `event` field
// (the (owner, name) is in the synthesized-event set, §3.5 — a plain user
// `Multicast` field does NOT auto-dispatch):
let field_ptr = slot;                                    // the field_addr lvalue ALREADY produced; do NOT recompute
let fv = coerce(rhs, rhs_ty, Struct(func_struct));       // lambda/mref → $Func (NOT to Multicast)
let sym = if op == Add { "Multicast.Add" } else { "Multicast.Remove" };
call(sym, vec![field_ptr, fv], Void);                    // mutates through the field ptr
return (fv, Struct(func_struct));
```

The `slot` from `lvalue` (a `field_addr` of the owning struct/`this`, `lower.rs:12544` shape) is
the **address** of the backing field, so `Add`/`Remove`'s `mut this` mutates the event **in place**
(not a by-value copy — the value-struct `mut`-receiver hazard that made bound method-refs decline
for value receivers, `lower.rs:10028`, is avoided here because we pass the field address directly,
never a loaded copy). **Do not recompute `field_addr`** — the draft's pseudo-code recomputed it,
which would double-emit; reuse the `:12391` `slot`. `coerce(rhs → $Func)` reuses the existing
`Ptr → $Func` auto-wrap (`lower.rs:12586-12588`) for a non-capturing lambda / static-method-ref
thunk, and is a no-op for a capturing lambda / bound-method-ref (already `$Func`). **`+=` on an
empty event** just works: the zero-initialized `Multicast` has `mItems == null`, and `Add`'s
grow-if-null lazily allocates.

`-=` removal is **by structural `$Func` equality** (both `code` and `target`): `Multicast.Remove`
scans for the first entry where `func_eq(entry, f)` and `RemoveAt`-shifts it out. `func_eq` is a
**new helper** that spills **both** `$Func`s to allocas and compares field 0 (`code`) **and**
field 1 (`target`) with an `and`. It is **NOT** a trivial extension of `func_code_field`
(`lower.rs:12554`), which reads field 0 **only** — a `code`-only compare would wrongly conflate two
bound refs of the same method on different receivers (the same hazard as the `code`-only `f == null`
at `lower.rs:8741`). This is **NOT** Beef's heap-delegate-identity removal (NewBF has no GC; `$Func`
is a value, §5) — it removes a target whose code+target both match. A lambda and a method-ref
produce distinct `code`, so `-=` matches the same producer expression.

### 3.5 Invoke-all — `e.Invoke(args)` / `e(args)` (sema)

**Dispatch seam (hardened — the draft pointed at the wrong site).** A qualified `e.OnTick(args)`
is an `Expr::Call` whose callee is an `Expr::Member`, which routes **unconditionally** to
`self.lower_method_call(base, name, args, src)` at `lower.rs:8326` (after the emit/enum special-
cases). `lower_method_call` resolves the name against the owner's **method** table, not its
**fields** — an event is a field, so it would emit a "no such method" path and **never reach an
invoke-all.** So T3 adds an explicit interception **before** `lower.rs:8326`, alongside
`try_enum_construct`: a **`try_lower_event_invoke(base, name, args, src)`** that checks whether
`name` is a synthesized `event` field (its `(owner, name)` is in the synthesized-event set, §3.2)
of `Multicast` type on `base`'s owner, and if so emits the loop below; otherwise returns `None`
and the normal method-call path runs. **v1 supports only the qualified `e.Name(args)` /
`e.Name.Invoke(args)` form** (the corpus examples all use it); a bare unqualified `Name(args)`
inside the class (callee `Expr::Ident`, `lower.rs:8347`) checks `local_fns`/`fn_sigs`+`lookup`, in
none of which an event field appears, so it would fall through — supporting it is deferred (§5).

The loop is modeled on the foreach-over-List Count/Get loop (`lower.rs:7567-7603`, the indexed
`i < Count(); entry = Get(i)` skeleton):

```text
// e.Invoke(a0, a1, …):  field_ptr = field_addr(owner, MulticastField)
// n = call("Multicast.Count", [field_ptr], i32)
// entry_slot = alloca($Func)                                          // ONE scratch slot, HOISTED before the loop
// for i in 0..n {
//     entry = call("Multicast.Get", [field_ptr, i], Struct(func_struct))  // a $Func BY VALUE (SSA aggregate)
//     store(entry_slot, entry)                                        // SPILL the $Func — REQUIRED to take field_addr
//     code   = load(field_addr(entry_slot, func_struct, 0), Ptr)      // §1.1 of fn-values
//     target = load(field_addr(entry_slot, func_struct, 1), Ptr)
//     call_args = [target, coerced-args…]
//     debug_assert_eq!(call_args.len(), ptys.len() + 1)               // arity guard (§6)
//     call_indirect(code, call_args, ret)                             // ret discarded (void v1)
// }
```

**The `$Func` spill is the new bit (the draft omitted it).** `Multicast.Get(i)` returns a `$Func`
**by value** (an SSA aggregate); you **cannot** `field_addr` a non-pointer. Spill it to a scratch
alloca first (the `func_code_field` pattern, `lower.rs:12554-12559`: `alloca; store; field_addr;
load`). The scratch slot is hoisted once before the loop (allocas don't violate SSA; re-storing
each iteration is fine). The single-target template `lower.rs:8368-8392` reads `code`/`target`
from a *named local slot* via `self.lookup(name)` — here that slot is the per-iteration spill, so
the spill is mandatory, not optional. The block skeleton (`head`/`body`/`cont`/`exit`) and the
`Count()`/`Get(i)` calls are copied from `lower.rs:7567-7603`; the per-entry `code`/`target` load +
`call_indirect` is copied from `lower.rs:8368-8392` **including its arity assert** (`:8385`).
**Order is subscription order** (ascending index). The result of each target is discarded in v1
(concrete delegate is `void`); a value-returning multicast (last-result semantics) is deferred
(§5). The arity assert is mandatory — LLVM builds the indirect-call type from the *args*, so a
drift is verify-clean (fn-values.md §1) and only the run-corpus catches it.

### 3.6 llvm + runtime

**newbf-llvm:** no change (no new instruction; `call`/`call_indirect`/`alloca`/`field_addr`/
blocks all exist). **newbf-runtime:** no change — the `Multicast` buffer uses the existing
guard-routed `newbf_alloc`/`Internal.Free` path (`alloc_array` → `heap_alloc`, `lower.rs:10466`;
`Internal.Free`, `List.bf:19`) already covered by the Stomp guard.

## 4. Worked examples (the run-corpus programs that prove it)

All under `e:/NewBF/beef-tests/run-corpus/`, `Program.Main -> int32`, `// expect: N`, JIT-run
full-i32 value checks under the Stomp guard (the authoritative gate; AOT truncates to 8 bits per
MEMORY, so keep N ≤ 255 *or* rely on the JIT harness). The existing function-value programs
(`function_pointer.bf → 12`, `fn_null.bf → 7`, `closure_arg.bf → 36`, `closure_basic`,
`list_hof`, `lambda_*`, `mref_*`) must stay green (no change to the single-target path).

0. **`delegate_concrete_call.bf` — `expect: 12`** (T1's proof, event-independent). A top-level
   `delegate int32 Op(int32);` and a static `Twice`; `Op f = Math2.Twice; return f(6);` → 12.
   Mirrors `function_pointer.bf → 12` but through a **named delegate type** (proving §3.2: the
   delegate is registered in `delegate_sigs`, lowers to `$Func`, and is callable via `fn_sigs`).
   The smallest slice; does not need `event` or `Multicast`.

1. **`mcast_manual.bf` — `expect: 30`** (T0's proof, the representation core, T1-independent).
   Build a `Multicast` **local** (a `scope`/owned value struct), `Add` two `$Func`s (two
   **existing** `function void()`-typed values — pre-feature callable, so the proof needs no
   named-delegate machinery), then manually `Get(0)`/`Get(1)`, spill each, and `call_indirect`
   both → summed side effects → 30. Pins the **bespoke 16-byte buffer**: two entries, the second
   does **not** alias the first's `target` (the `List<$Func>` truncation that the design exists to
   avoid), under the Stomp guard. (Subscribers are existing `function void()` locals/refs, so this
   gate does **not** depend on T1's named-delegate callability — the dependency the draft created.)

2. **`event_multicast_two.bf` — `expect: 30`** (the marquee — T3). A `delegate void Notify();`
   and a class holding `event Notify OnTick;`. Two static method-refs (or two lambdas) that each
   add to a static accumulator (`+10` and `+20`); `e.OnTick += A; e.OnTick += B; e.OnTick.Invoke();`
   then `return acc;` → both ran, in order → **30**. Pins: contextual `event` parsing, the
   synthesized `Multicast` field, `+=` → `Add`, invoke-all iterating + per-entry spill +
   `call_indirect`, the 16-byte buffer stride (two entries, no aliasing) under the guard.

3. **`event_unsubscribe.bf` — `expect: 10`** (the `-=` edge — T3b). Same setup; subscribe `A`(+10)
   and `B`(+20), then `e.OnTick -= B;` then raise → only `A` runs → **10**. Pins structural `$Func`
   equality removal (`func_eq` matching **both** fields) + `RemoveAt`-shift, and that the survivor
   still fires after a removal (the in-place field mutation persisting to the raise).

4. **`event_empty_raise.bf` — `expect: 0`** (the empty edge — T3a). Declare `event Notify OnTick;`,
   raise it with **no** subscribers → the invoke-all loop runs zero times → `return acc;` → **0**.
   Pins: zero-initialized `Multicast` (`mItems == null, mCount == 0`) is safely invokable (no
   null-buffer deref because `Count == 0` short-circuits the loop).

5. **`event_scope_dispose.bf` — `expect: 0`** (the buffer-free guard test — T0/T3). Construct an
   event-bearing `scope` value (or a `scope`-local `Multicast`), `Add` two targets, then let it
   fall out of scope — the `DisposeHook` (§3.3) frees `mItems` **exactly once** under the Stomp
   guard (no double-free abort, no use-after-free). Returns 0 (a clean run is the assertion). Pins
   the §3.3 `DisposeHook` free path and the "value-copy forbidden / no double-free" invariant.

6. **`event_add_then_invoke_arg.bf` — `expect: 25`** (an argument-carrying concrete delegate — T3).
   `delegate void Notify(int32);`, `event Notify OnVal;`; subscribers add their `int32` arg to an
   accumulator; `e.OnVal += A; e.OnVal += B; e.OnVal.Invoke(5);` with two subscribers (`acc += x`
   and `acc += x*4`) → `5 + 20 = 25`. Pins arg coercion + the arity assert (`ptys.len()+1` with one
   real param + `$self`) for a non-void-arg delegate raised over the list.

Each `.bf` is self-contained (inline `delegate`/class decls; corlib `Delegate.bf` in the
prelude). All `// expect:` values fit in i32 and are ≤ 255 (AOT-safe; they run under the JIT
harness anyway).

## 5. v1 scope vs explicitly deferred

**In v1 (ship this — NO generic-interface dependency, §7):**
- A **concrete** multicast delegate as the corlib `Multicast` value-struct holding a bespoke
  **16-byte-strided `$Func` buffer** (NOT `List<$Func>`, §2.1) with `Add`/`Remove`/`Count`/`Get`
  + a `Dispose()` that frees the buffer once (via a `DisposeHook` for `scope`-local events).
- A new **`StructTable` delegate-registration pass** (`delegate_sigs`) so a named concrete
  delegate type is reachable from lowering (§3.2).
- An **`event DelegateType Name;`** member (concrete delegate type) via a **contextual** `event`
  keyword (§3.1), lowering to a synthesized backing `Multicast` field + the verbs, with the
  (owner, name) recorded in a synthesized-event set.
- `e += f` / `e -= f` **special-cased in `assign`** (NOT a general `operator+=`): `Add`/`Remove`
  through the field address (in-place mutation), `+=` on an empty event lazily allocates, `-=`
  removes by **structural `$Func` equality** (both `code` + `target`).
- Invoke-all `e.Invoke(args)` / `e(args)` (qualified form only): iterate the buffer in subscription
  order, spill each `$Func`, `call_indirect`, with the arity assert.
- Subscribers: a **lambda**, a **static method-ref** (`$mref$` thunk), or a **bound method-ref**
  (`$mrefb$`, class receiver) — all already `$Func`-coercible (`lower.rs:8181`/`9962`/`10011`).
- A **named concrete delegate-typed local** (`Op f = …; f(x)`) callable via the `delegate_sigs` +
  `fn_sigs` extension (§3.2).
- Emptiness via `e.Count` (`Count() == 0`); **no** `e == null` surface (a value-struct has no null).

**Deferred (honest — the genuinely hard parts):**
- **Value-struct field-dtor chaining in `emit_destroy`** (`lower.rs:11067-11089` walks only the
  inheritance chain). Until it lands, an `event` field embedded in a heap class **leaks its
  buffer** (benign under the guard; `scope`-local events are freed via the `DisposeHook`, §3.3).
  This is its own separable mechanism touching the ownership/dtor path.
- **Unqualified `Name(args)` invoke** inside the class (bare `Expr::Ident` callee). v1 requires the
  qualified `e.Name.Invoke(args)` / `e.Name(args)` form (§3.5).
- **Generic delegates** (`delegate void Action<T>(T x)`, `Predicate<T>`, `Func<T,R>`). These need
  a *delegate monomorph path* — delegates are not registered as structs at all today
  (`register_struct_names` ignores `Item::Delegate`, `lower.rs:2574`; `ty_of` doesn't know
  delegates, `lower.rs:646-656`). A generic delegate could monomorphize like a generic *class*
  (mirror `record_inst`/`register_mono`/`mangle_generic`) **without** generic-interfaces, since
  `$Func` is signature-agnostic (one layout) — but `Multicast` would then need to be generic over
  the element signature (or stay `$Func`-agnostic with a per-instantiation `ptys`). This is its own
  tractable slice; see §7.
- **The full upstream `Event<T> where T : Delegate`** — blocked on **generic-interfaces**: its
  `Enumerator : IEnumerator<T>` (an interface-typed generic enumerator) is the exact construct
  excluded by the monomorph index (`lower.rs:737`, the `td.kind != TypeKind::Interface` guard;
  `collect_iface_own_type` filters generic interface methods). Also needs `rettype(T)`, `params T`
  (params over a *delegate*, not `params T[]`, `lower.rs:1926`), runtime `as List<T>`, and a
  bit-packed single-vs-list `mData`. Each is its own feature.
- **Delegate as a heap GC object with identity + `delete`-able lifetime** (Beef's
  `Delegate{mFuncPtr,mTarget}`). NewBF has no GC; `$Func` is a value with no identity. Removal uses
  structural equality (§3.4), not reference identity.
- **A general `operator+=` instance-operator form** (void, mutating, single-arg). v1 special-cases
  events in `assign` instead (§3.4); the broad `assign` change is deferred.
- **Value-returning multicast** (a non-void delegate whose invoke yields the last target's result).
  v1's concrete delegate is `void`; the loop discards each result.
- **Delegate variance**, `is`/`as`/cast on delegate types, `Delegate.Equals`/`GetHashCode`/
  `operator==` as language ops, and **`async`** / `IAsyncResult` / `AsyncCallback`.
- **Enumeration-safe add/remove during invoke** (upstream's `sIsEnumerating`/`sHadEnumRemoves`).
  v1 **forbids** mutating an event from inside its own raise (or snapshots the count at loop entry,
  the simpler guard) — documented.
- **Bound method-ref of a value-struct / `mut` / `ref` receiver** as a subscriber — inherits the
  fn-values.md Risk 7.9 deferral (`try_bound_method_ref` declines non-class receivers,
  `lower.rs:10028`); class receivers only.

## 6. Load-bearing risks + mitigations

- **`List<$Func>` truncation (the headline representation risk).** `List<T>` hardcodes an 8-byte
  slot stride (`List.bf:16,140`), so a 16-byte `$Func` element would alias/overrun. *Mitigation:*
  the backing store is a **bespoke `$Func*` buffer via `alloc_array`** (block **sized** by the real
  `size_of_ty($Func)` = 16, `lower.rs:10461-10462`) **indexed with an explicit `Struct(func_struct)`
  element** at every `elem_addr` (the 16-byte stride, §2.1) — never `List<$Func>`, and never a
  bare-`Ptr` element. The `Multicast` layout unit test + `mcast_manual.bf`/`event_multicast_two.bf`
  (two entries, no aliasing) under the Stomp guard pin it.
- **No `StructTable`→delegate-sig data path (the named-delegate-callability risk).** The def-graph
  `DelegateSig` is unreachable from lowering (`lower_program` ignores `_program`, `lower.rs:5466`;
  `lower_value_ty`/`fn_sigs` see only `&StructTable`). *Mitigation:* T1 adds a `delegate_sigs`
  `StructTable` map populated from `Item::Delegate` AST in `StructTable::build`; `lower_value_ty`
  and both `fn_sigs` sites consult it (§3.2). `delegate_concrete_call.bf` pins it.
- **`event`-as-keyword breaks the parser ratchet.** `event` is a parameter identifier in
  `corlib-slice/Platform.bf` (lines 391-433) and `Event.bf` (line 232), both in the 100%-clean
  parser corpus (`newbf-parser/tests/corpus.rs:40,79-84`). *Mitigation:* `event` is a **contextual**
  keyword recognized only in `member()` via `at_ident_text` (the `get`/`set`/`not` precedent,
  `parser.rs:114-119,278,3760`); it stays a normal `Ident` everywhere else. No `Keyword::Event`.
- **Invoke-all dispatches into the method table, never the event field.** A qualified
  `e.OnTick(args)` routes to `lower_method_call` (`lower.rs:8326`), which resolves methods, not
  fields. *Mitigation:* a `try_lower_event_invoke` interception **before** `:8326` (alongside
  `try_enum_construct`) recognizes a synthesized `event` field and emits the loop (§3.5);
  `event_multicast_two.bf` pins it.
- **Invoke-all `$Func` spill (the SSA hazard).** `Multicast.Get(i)` returns a `$Func` **by value**;
  `field_addr` on a non-pointer is ill-typed. *Mitigation:* hoist one `alloca($Func)` scratch slot
  before the loop, `store` each `Get(i)` result, then `field_addr`/`load` `code`/`target` from it
  (the `func_code_field` pattern, `lower.rs:12554`). §3.5 spells it out.
- **Verify-clean invoke-all miscompile (the dominant `$Func` failure mode).** LLVM builds the
  indirect-call type from the *args*, so an arity/type drift in the invoke loop is verify-clean and
  only the run-corpus catches it (fn-values.md §1). *Mitigation:* the per-entry `call_indirect`
  copies the proven single-target shape (`lower.rs:8368-8392`) **including** its
  `debug_assert_eq!(call_args.len(), ptys.len()+1)` arity guard (`lower.rs:8385`). The run-corpus is
  the authoritative gate.
- **`assign` special-case insertion point + double-emit.** The event `+=`/`-=` must intercept
  **after** `lvalue` (`lower.rs:12391`, which already returns the field address) but **before** the
  `:12392` coerce-to-lvalue (coercing a lambda to `Multicast` is wrong) and before the `:12410`
  `compound_op` struct path (which falls to `arith` on a struct — ill-typed). *Mitigation:* branch
  on `ty == Struct(multicast_id)` ∧ `(owner,name) ∈ event-set` ∧ `op ∈ {Add,Sub}` right after
  `:12391`, **reuse** the `slot` (do **not** recompute `field_addr` — double-emit), coerce rhs to
  `$Func`, call `Add`/`Remove` (§3.4).
- **In-place mutation of the event field (value-struct `mut`-receiver hazard).** A `Multicast` is a
  value struct; `+=`/`-=` must mutate the **backing field in place**, not a by-value copy (a copy
  would lose the subscription — the same hazard that made bound method-refs decline value receivers,
  `lower.rs:10028`). *Mitigation:* pass the `lvalue` `slot` (the field **address**) to
  `Add`/`Remove`, never a loaded copy (§3.4); `event_unsubscribe.bf` (a removal that must persist to
  the raise) pins it.
- **Buffer-free lifetime + value-copy double-free.** Value-struct field/local dtors are **not**
  chained (`emit_destroy`, `lower.rs:11067`, walks only inheritance; scope cleanup is `Ref`-only,
  `lower.rs:8520`). A by-value copy of a `Multicast` that later freed `mItems` would **double-free**
  → a Stomp abort. *Mitigation:* (a) `scope`-local events free via a `DisposeHook`
  (`lower.rs:7003-7015`) — exactly once; (b) `event` **fields** of heap classes **leak** the buffer
  (documented, benign under the guard, §3.3/§5); (c) `Multicast` methods take `this` by address and
  an event is never loaded by value → no copy → no double-free. `event_scope_dispose.bf` pins the
  free-once path under the guard. **No "exactly-once free via `~this()`" claim** (that would be dead
  code).
- **`+=` on an empty / null event.** A zero-initialized `Multicast` has `mItems == null`.
  *Mitigation:* `Add` grows-if-null (lazy alloc); the invoke loop short-circuits on `Count == 0`
  (no null deref). `event_empty_raise.bf` pins the empty-raise path.
- **`-=` removal semantics (structural, not identity).** NewBF has no delegate identity; `-=`
  removes by `code+target` equality. *Mitigation:* the new `func_eq` helper compares **both**
  fields (a `code`-only compare, like `f == null` at `lower.rs:8741`, would wrongly conflate two
  bound refs of the same method on different receivers); it is **not** a copy of `func_code_field`
  (`lower.rs:12554`, field-0-only). `event_unsubscribe.bf` pins it.
- **Ratchet breakage (corrected claim).** verify-corpus (162/162) + parser-corpus are 100%-clean
  ratchets. The new surface is confined to new test files **only because `event` is contextual**
  (a reserved keyword would break `Platform.bf`/`Event.bf` — see above). The new `Member::Event`
  variant forces the two exhaustive walks (`Member::span()`/`print.rs::member`); the print.rs
  round-trip for `event T N;` must be added (parser-corpus). *Mitigation:* contextual keyword +
  confined new surface + untouched single-target `$Func` path; run-corpus is authoritative.
- **sema ⊥ llvm (HARD INVARIANT).** Everything is in newbf-sema emitting IR + named `Multicast.*`
  symbols + the existing `field_addr`/`call_indirect`. *Mitigation:* no new IR instruction →
  newbf-llvm untouched (mirrors fn-values.md §4 / iterators.md §2.1).
- **Walker audit (compiler does NOT enforce wildcard member-walks).** Only `Member::span()` /
  `print.rs::member` are exhaustive. *Mitigation:* T2 hand-edits the member-registration walks to
  synthesize the backing field + record the event for `Member::Event`, and ships a focused "event
  registers a field" test.
- **JIT FP constant pool (MEMORY).** No float constants in any delegate/event path → the JIT-FP-
  constant-pool caveat does not apply; the JIT run-corpus harness is the gate.

## 7. Cross-feature dependency (generic-interfaces)

**A v1 concrete multicast + event does NOT need generic-interfaces.** The precise split:

| Feature slice | Needs generic-interfaces? | Why |
|---|---|---|
| Concrete `Multicast` (`$Func` buffer) + `event` + `+=`/`-=`/invoke (concrete delegate) | **No** | `$Func` is monomorph-clean (one layout, fn-values.md §6); the buffer is a single concrete `alloc_array`'d block (sized by real `size_of_ty`, 16-byte stride at each `elem_addr`, §2.1); invoke reuses the existing `call_indirect`. No interface dispatch anywhere. |
| Named concrete delegate-typed local callable (`Op f; f(x)`) | **No** | A delegate `TypeDef` with `arity == 0` is registered in the new `delegate_sigs` `StructTable` map, aliases to `$Func`, and feeds `fn_sigs` (§3.2). |
| `-=` structural removal | **No** | `func_eq` compares both `$Func` fields; new but local. |
| **Generic** delegate (`Action<T>`) as a first-class type | **No** (but needs a delegate **monomorph** path) | `$Func` is signature-agnostic, so a generic delegate can monomorphize like a generic *class* (mirror `record_inst`/`register_mono`) **without** interfaces. The blocker is that delegates aren't registered as structs at all (`register_struct_names` `:2574`, `ty_of` `:646`) — a tractable own-slice, not an interface dependency. |
| Full upstream `Event<T> where T : Delegate` (`IEnumerator<T>` enumerator, `as List<T>`, `rettype(T)`, `params T`, bit-packed `mData`) | **Yes** | The `Enumerator : IEnumerator<T>` is an interface-typed generic enumerator — the exact construct excluded by the monomorph index (`lower.rs:737`, the `kind != TypeKind::Interface` guard) and `collect_iface_own_type`'s generic-interface filter. Same deferral that blocks iterators-lazy's `IEnumerable<T>`. |

**What this feature PROVIDES to / shares with other wave-4 features:** the **bespoke-buffer
pattern** (a `Struct`-element heap array via `alloc_array`'s real-`size_of_ty` block sizing +
an explicit `Struct(...)` element at each `elem_addr`, `lower.rs:10461-10462`/`:10499`, *instead
of* the 8-byte-strided `List<T>`) is the reusable answer whenever a collection must hold a >8-byte
value struct — directly relevant to any wave-4 feature storing `$Func`/`Type`/tuple values in bulk.
The `func_eq` structural-equality helper is the groundwork for fn-values.md's deferred
`Func$.Equals` / Delegate-bridge (T8). **What it needs from generic-interfaces:** nothing for v1;
only the deferred **full `Event<T>`** consumes the monomorphized-generic-interface itable (the
iface id + itable that generic-interfaces, feature #1, provides), on the same footing as
iterators-lazy.

## 8. Task breakdown

Each task is agent-assignable with a one-line seed + a concrete acceptance gate. Gates green at
**every** boundary: verify corpus 162/162, parser corpus, run-corpus (authoritative). A task lands
only when its own test + all prior gates are green.

**Dependency order:** `T0 → {T1 ∥ T2} → {T3a → T3b} → T4`. T0 is the representation foundation
(the 16-byte buffer, proven with **existing** `function`-typed subscribers so it's T1-independent);
T1 (named-delegate callability, the `delegate_sigs` pass) and T2 (`event` parsing + field synthesis)
are independent of each other; T3a/T3b are the behavioral core (split per the smallest-diff rule);
T4 ties off.

**T0 — `Multicast` corlib value-struct + the 16-byte `$Func` buffer (the representation core).**
*Seed:* add `newbf-corlib/bf/Delegate.bf` with `struct Multicast { function void()* mItems; int
mCount; int mCap; … }` (§2.1) and register it in `prelude()` (`newbf-corlib/src/lib.rs`). **Hand-
emit** `Add`/`Get`/`Grow` in sema (like the auto-property getter, `lower.rs:6036-6047`), plumbing
the buffer element as the explicit `Struct(func_struct)` at the `alloc_array`/`elem_addr` seams so
the buffer is **16-byte-strided** (§3.3). Wire a `Multicast.Dispose()` free + the `DisposeHook` for
a `scope`-local `Multicast` (`lower.rs:7003-7015`). Add a `Multicast` layout unit test
(`{Ptr,i64,i64}`) and the manual-iteration corpus proof. *Accept:* `mcast_manual.bf → 30` passes
under JIT/Stomp using **existing `function void()` subscribers** (two 16-byte entries, no aliasing —
**T1-independent**); `event_scope_dispose.bf → 0` (free-once via the `DisposeHook`, no double-free
under the guard); the layout test pins `{Ptr,i64,i64}`; verify 162/162. **Proves the representation
decision in isolation, before events/named-delegates layer on top.** Additive — no parser/event
change.

**T1 — Named concrete delegate type → callable `$Func` local (event-independent).**
*Seed:* add `delegate_sigs: HashMap<String,(IrType,Vec<IrType>)>` to `StructTable` and a
`StructTable::build` pass that registers arity-0 `Item::Delegate` nodes (lowering `return_ty`/
`params` from the AST via `lower_ty_env`, `parser.rs:2635-2638`); have `lower_value_ty`
(`lower.rs:13180`) return `Struct(func_struct)` for a path in `delegate_sigs`, and extend both
`fn_sigs` sites (`lower.rs:6710-6723`/`7255-7268`) to pull `(ret,ptys)` from `delegate_sigs` for
such a path (§3.2). *Accept:* `delegate_concrete_call.bf → 12` (§4 ex 0) passes — a `delegate int32
Op(int32)` local holds `Math2.Twice` and `f(6) == 12`; the inline-`function` path
(`function_pointer.bf → 12`) unchanged; verify 162/162. Deps: T0 (`Multicast`/`$Func` machinery
in place). Independent of T2.

**T2 — Contextual `event` keyword + `Member::Event` parsing + backing-field synthesis.**
*Seed:* **no lexer change** — recognize `event` contextually in `member()` via `at_ident_text`
(`parser.rs:114`, the `get`/`set` precedent); add `Member::Event` to `ast.rs` (§3.1) + the forced
`Member::span()` / `print.rs::member` arms (round-trip `event T N;`); hand-edit the member-
registration walks (`register_type_struct` / `build.rs` member loop) to synthesize a backing
`Multicast` field **and** record `(owner, name)` in a synthesized-event set; add the contextual
`event` arm to `member()` (`parser.rs:3102`, before the field fall-through `:3290`). *Accept:* an
`event`-bearing class parses, round-trips in the parser corpus, registers a `Multicast` backing
field (a focused "event-registers-a-field" test — the wildcard-walk skip would otherwise drop it
silently), AND the existing `Platform.bf`/`Event.bf` `event` identifiers still parse clean; parser
corpus 100%; verify 162/162. Deps: T0 (`Multicast` must exist). No verbs yet (the field is inert).

**T3a — Invoke-all `e.Invoke(args)` / `e(args)` (the call-shape risk).**
*Seed:* add `try_lower_event_invoke(base, name, args, src)` intercepting **before**
`lower_method_call` (`lower.rs:8326`, alongside `try_enum_construct`) for a synthesized `event`
field; emit the invoke-all loop (§3.5 — copy the foreach-List skeleton `lower.rs:7567-7603` + the
per-entry **spill** + `code`/`target` load + `call_indirect` `lower.rs:8368-8392`, **with the arity
assert**). *Accept:* `event_multicast_two.bf → 30`, `event_empty_raise.bf → 0`,
`event_add_then_invoke_arg.bf → 25` pass under JIT/Stomp; the single-target `$Func` programs
(`function_pointer`, `fn_null`, `closure_arg`, `list_hof`, `lambda_*`, `mref_*`) unchanged; verify
162/162. **The verify-clean arity-drift surface (fn-values §1) — the smallest possible diff for
the call shape.** Deps: T0, T2. (Subscribe via `Add` is needed to populate the list — fold the
minimal `+=`→`Add` special-case here too, or seed the test via a `scope` `Multicast` direct-`Add`;
prefer the former so `event_multicast_two.bf` is end-to-end.)

**T3b — `-=` unsubscribe + `func_eq` structural removal (the mutation/removal risk).**
*Seed:* complete the `assign` event special-case (`lower.rs:12391`, **after** `lvalue`, **before**
the `:12392` coerce; reuse the `slot`, do not recompute `field_addr`) for `AssignOp::Sub` →
`Multicast.Remove`; add the `func_eq` helper (compares **both** `$Func` fields, **not** a copy of
`func_code_field` `:12554`) used by `Remove`'s scan + shift (§3.4). *Accept:* `event_unsubscribe.bf
→ 10` passes under JIT/Stomp (the survivor fires after a removal that persists to the raise — the
in-place field mutation); all T3a gates still green; verify 162/162. Deps: T3a.

**T4 — Journal + doc cross-link + verify pin.**
*Seed:* add a numbered journal entry (design + outcome) to `docs/journals/`; add a focused
verify-corpus fixture exercising an `event` + `+=`/invoke (pin the IR shape); cross-link this design
doc. *Accept:* journal entry present; verify corpus count incremented and green; commit pairs with
the entry (conventional style + the `Co-Authored-By` trailer). Deps: T3b.

**Final task count: 6** (T0, T1, T2, T3a, T3b, T4).

**Riskiest task: T3a** — the invoke-all loop is the verify-clean arity/type-drift surface
(fn-values.md §1: LLVM builds the indirect-call type from the *args*, so a drift passes verify and
only the run-corpus catches it), and it converges the new `try_lower_event_invoke` dispatch seam,
the per-entry `$Func` spill (the SSA hazard), and the 16-byte-buffer striding under the Stomp guard.
