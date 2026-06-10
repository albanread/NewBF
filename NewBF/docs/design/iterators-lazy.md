# Lazy Coroutine `yield` (state machine) + interface-typed `IEnumerator<T>` — Design

> **Status: DESIGNED (wave 4), NOT landed.** Builds directly on the wave-3 eager-`yield`
> + 5th-`foreach`-branch substrate (`docs/design/iterators.md`, LANDED). v1 here is the
> **lazy / coroutine state-machine** `yield` — a compiler-synthesized *resumable* concrete
> enumerator (state field + `MoveNext` resume switch + cross-yield local spill) that the
> existing 5th `foreach` branch drives **unchanged**. The interface-typed
> `IEnumerator<T>`/`IEnumerable<T>` half is a **separate** work item, hard-blocked on
> generic-interface monomorphization (feature #1) — it is **decoupled** from lazy-yield and
> deferred behind a clear API contract (§7). Every load-bearing claim is anchored to a
> re-verified `file:line`.

## 1. Overview

Wave-3 `yield` is **eager**: `rewrite_generators` (`lower.rs:5444`) re-emits a
`List<E>`-returning generator as a `List<E>`-builder — `yield return e` → `__yield.Add(e)`,
`yield break` → `return __yield`, prologue `List<E> __yield = new List<E>();`, epilogue
`return __yield;` — via **disjoint `SrcEdit` span-insertions** into the *original* source
(`collect_generator_edits`, `lower.rs:5354-5376`; `apply_edits`, `lower.rs:5382-5399`),
re-parsed with a fresh `FileId` (`lower.rs:5455-5457`). The whole loop runs at call time;
every value is materialized into an owned heap `List<E>`; there is **no resume, no state
field, no `MoveNext` re-entry** (the method runs once, start to finish). Consequences
(documented divergence, iterators.md §5): no laziness, **no infinite/large sequences** (the
entire sequence is held in heap memory at once).

**v1 capability (one paragraph).** Add a **lazy** generator lowering, *gated* so the eager
path stays the default for the three pinned eager tests. For a generator whose body is a
**feasible shape** — straight-line (`yield return a; yield return b; yield return c;`) **or a
single loop containing yields** (a range `for (i in 1...n)`, a `while`, or a C-style/infinite
`for`) — the lazy transform synthesizes, **as `SrcEdit`-spliced source text**, a **top-level
concrete generic enumerator** `__GenN<…>` carrying an `int mState`, one field per captured
argument, one field per **cross-yield-live local**, one field per **synthesized loop-induction
slot** (range `lo`/`cur`/`hi`), and a `mCurrent` field; **rewrites the generator method body
AND its return type** from `List<E>` to `__GenN<E>` (§2.4 — the return-type change is what
routes the consumer to the right `foreach` branch); and synthesizes a `MoveNext()` whose body
is a **`switch (mState)` resume dispatch** that jumps past the last yield, runs to the next
`yield return` (storing the value into `mCurrent`, recording the next state, `return true`),
and `return false` on completion. `Current` reads `mCurrent`; `Dispose()` is a no-op;
`GetEnumerator()` returns a reset copy of `this`. The synthesized enumerator is a **concrete
monomorphized generic value struct** — **resolved statically by name** on the concrete type by
the **unchanged** 5th `foreach` branch (`lower.rs:7619-7747`), with **zero loop-lowering
changes** and **zero interface type**. The single genuinely-hard part is the **cross-yield
local spill + induction-state synthesis + resume-switch synthesis** (the liveness transform
the codebase lacks), which the alloca-everything codegen tames for the v1 shapes (§6.1).
Lazy-yield is **independent of** generic-interface monomorphization (§7); the interface-typed
`IEnumerator<T>`/`IEnumerable<T>`-`foreach` half is hard-blocked on it and deferred.

This document is modeled on `docs/design/iterators.md` and `docs/design/generic-constraints.md`
(the two wave-3 designs whose rigor and task structure this mirrors).

## 2. Representation / ABI / IR changes

### 2.1 No new IR instruction, no new `IrType`, no ABI change

The lazy path is — exactly like the eager path — a **source-text `SrcEdit` splice + re-parse in
newbf-sema** (`rewrite_generators`, `lower.rs:5444`) that produces ordinary Beef AST (a
generic struct + a `switch`-bodied method), lowered by the existing path. The synthesized
`switch (mState)` resume dispatch lowers to the **chained `cmp`/`cond_br`** that `Stmt::Switch`
already emits (`Stmt::Switch` arm, `lower.rs:7773-7877`; the scalar `cmp`/`cond_br` chain at
`:7792-7861`) — there is **no native `Switch` IR instruction** (`InstKind`, `inst.rs:207`, has
`CallIndirect`/`Phi` at `:266`/`:271` but no switch/jump-table). Crucially, `mState` is an
`int` scrutinee, so it takes the **scalar value-equality chain**, NOT the payload-enum match
guarded at `lower.rs:7786-7791` (`Struct(eid)|Struct(eid)` with `enum_cases`). `IrType` stays
`Copy` (`newbf-ir/src/ty.rs`); the enumerator is an ordinary `Struct(eid)` value. **No new IR
instruction; newbf-llvm needs zero changes; newbf-runtime needs zero changes** (a value-struct
enumerator allocates nothing; the eager substrate's `Internal.Malloc`/`Internal.Free` path is
untouched).

### 2.2 The sema ⊥ llvm contract (what sema emits by-name)

The HARD INVARIANT (sema must not depend on newbf-llvm; they agree via the IR contract +
named symbols) is preserved trivially: the lazy transform is a **pure-sema source rewrite**
that emits **the same IR shapes the eager path + the 5th `foreach` branch + ordinary generic
value structs already emit today**. Every call the synthesized enumerator makes is a direct
`fb.call(symbol, args, ret)` (or, on the consumer side, `call_instance_on_ptr`,
`lower.rs:10068-10086`) against a method symbol sema itself mangled.

| Sema emits (by name / by shape) | llvm defines / lowers |
|---|---|
| The synthesized `__GenN<…>` struct's `MoveNext`/`get_Current`/`Dispose`/`GetEnumerator` mangled symbols (ordinary monomorphized generic methods) | the already-emitted mono symbols |
| `switch (mState)` → chained `cmp`/`cond_br` (`lower.rs:7792-7861`, existing) | the existing `cmp`/`cond_br` insts |
| The 5th `foreach` branch's `call_instance_on_ptr` against `MoveNext`/`get_Current`/`Dispose` (`lower.rs:7712/7723/7003-7015`, existing) | the same mono symbols |

**No new global, no new extern, no widened struct, no new mangling key.** newbf-llvm needs
**zero** change. newbf-sema gains **no** dependency on newbf-llvm.

### 2.3 The synthesized enumerator is a TOP-LEVEL CONCRETE generic struct (forced, not stylistic)

The synthesized `__GenN<…>` **must be a top-level generic type**, spliced as a
`CompUnit`-level `Item::Type` (an end-of-file append edit, §3.1 step 5) — **not** nested inside
the generator's owner. **Why this is forced:** `index_generic_decls` (`lower.rs:716-753`)
iterates only `Item::Type` and `Item::Namespace` bodies — it is **member-blind** (zero `Member`
descent) and additionally **excludes generic interfaces** (`td.kind != TypeKind::Interface`,
`lower.rs:735-737`). A generic type declared as a `Member::Nested` is **never inserted** into
the `(name, arity)` `generics` map, so `record_inst` hits its `else { return; }`
(`lower.rs:1759-1761`) and the type **never monomorphizes** — it would lower to `Ptr`, and the
5th branch's `match ge.ret { Struct(e)|Ref(e) => e, _ => bail }` (`lower.rs:7625`) would bail.
`ListEnumerator<T>` was made top-level in corlib for **exactly** this reason (`List.bf:192-204`,
the same member-blindness rationale). *(NOTE: the `List.bf:195` comment still cites the stale
`lower.rs:655-692` for `index_generic_decls`; the live site is `lower.rs:716-753` — do not chase
the stale span. MEMORY/CLAUDE.md's `655` cite is likewise stale.)* The lazy transform splices
`__GenN<…>` at top level so it travels the **already-working generic-value-struct path**
(`index_generic_decls` → `record_inst` → `register_mono`, `lower.rs:716/1745/907`), the same
path `ListEnumerator<T>` proves under JIT/Stomp (`enum_manual.bf → 6`). **It is a CONCRETE
monomorphized type — never an interface — so it does NOT need generic-interface
monomorphization** (§7).

**KEY REPRESENTATION DECISION — value struct only (v1).** The synthesized enumerator is a
**value struct** (`StructKind::Value`). It inherits the **tested** generic-value-struct ABI
(`ListEnumerator<T>`, `List.bf:205-222`; the value-struct `this`-aliasing discipline,
`lower.rs:7706-7723`) and **sidesteps heap ownership entirely** — nothing to `delete`, the
5th branch copies it into `e_slot` (`lower.rs:7666`) and reuses that address as `this` (§6.3).
**A direct consequence (do not miss): a `[Coroutine]` generator's result is a value struct and
must NOT be `delete`d** — `var ns = LazyGen(); delete ns;` is a user error (a `delete` on a
value struct), and the v1 corpus programs must iterate the result **inline**
(`foreach (x in LazyGen())`), NOT bind-then-`delete` (the eager idiom, §4). Because the captured
state is copied by value into `e_slot`, v1 also caps captures to **scalar / small-aggregate**
shapes (the feasible-shape gate already implies this; a closure over a large struct arg bloats
the `e_slot` copy — deferred with the `Ref` enumerator, §5). A `Ref` (heap) enumerator is
**deferred** (it needs the auto-`delete` ownership work, §5/§6.4). **v1 ships value-struct lazy
enumerators only.**

### 2.4 What the generator method returns — the return type changes to `__GenN<E>` (LOAD-BEARING)

The 5th `foreach` branch (`lower.rs:7619-7639`) is reached **only when the Count/Get probe
above it yields `None`** (the comment at `lower.rs:7607-7609` pins this). That Count/Get probe
(the **4th** branch, `lower.rs:7542-7606`) fires when `coll_ty` is `Ref(id)` for an `id` that
defines `Count()`/`Get(int)` — **which `List<E>` does**. The consumer evaluates
`let (coll, coll_ty) = self.expr(iter, src)` (`lower.rs:7542`), so `coll_ty` is the generator
method's **declared return type**. Therefore:

**The generator's return type MUST be rewritten from `List<E>` to `__GenN<E>`.** If it stayed
`List<E>`, `coll_ty = Ref(list_id)`, the 4th (Count/Get) branch would fire at `lower.rs:7556`,
and the consumer would call `List.Count()`/`List.Get()` on a value that is actually a
`__GenN<E>` — a guaranteed miscompile / Stomp fault, and the 5th branch would **never** be
entered. By rewriting the return type to `__GenN<E>` (a value struct with **no** `Count`/`Get`,
**with** `GetEnumerator`), `coll_ty = Struct(genN_id)`, the 4th branch's `if let IrType::Ref(id)`
guard (`lower.rs:7543`) fails (it is a `Struct`, not a `Ref`), the Count/Get probe yields `None`,
and the **5th branch fires** — exactly the decoupling thesis. The return-type rewrite is **the
mechanism**, not an incidental detail; §3.1 step 4 specifies the exact source edit (the
return-type `AstType` span is replaced; the `List<E>` text is sliced for `E` *before* the
replacement).

`E` is extracted syntactically by the existing `generator_list_ty_text` (`lower.rs:5262-5271`,
matches a `List<…>` path with one type arg) — v1 keeps the explicit `List<E>` *surface*
declaration (no inference); the transform reads `E` from it, then **replaces** the whole return
type with `__GenN<E>`. (A future `IEnumerable<E>` surface return type is the
generic-interface-dependent variant, §7.)

**Self-enumerator shape (v1).** The synthesized `__GenN<…>` *is* the enumerator **and** carries
a `GetEnumerator()` that returns a copy of `this` reset to `mState = 0` (the corlib precedent
`List<T>.GetEnumerator()` returns a value enumerator by value, `List.bf:31-37`). So
`foreach (x in Gen(args))` resolves `GetEnumerator` on `__GenN`, gets a fresh `__GenN`
enumerator, and drives it. The **enumerable + separate enumerator** BCL split (two synthesized
types) is strictly more machinery for no v1 benefit (foreach calls `GetEnumerator` once) and is
**deferred**.

### 2.5 No metadata change, no new mangling

`__GenN<int32>` mangles under the standard mono prefix exactly like `ListEnumerator<int32>` /
`List<int32>` do today (the top-level generic *type* mangle via `mangle_generic`,
`lower.rs:13126`). No new monomorph key, no new symbol namespace, no reflection-metadata change
(the synthesized type is an ordinary monomorphized generic struct).

## 3. Concrete changes (sema only), with seams

**Parser / AST: NO change.** The `yield` surface is fully landed in wave-3:
`Stmt::YieldReturn { span, value }` / `Stmt::YieldBreak { span }` (`ast.rs:497,500`), exhaustive
in `Stmt::span()`; `yield_stmt()` (`parser.rs:2079-2099`, dispatched from `stmt()`) requiring
`return`/`break` after `yield` (no bare `yield e;` — which **helps**: §5 yield-in-expression is
syntactically excluded). The walker audit is complete — the `YieldReturn`/`YieldBreak` arms
already exist (from the eager landing): `collect_insts_stmt` `lower.rs:2229`, `for_each_stmt_expr`
`:3825`, `collect_lambdas_stmt` `:3720`, `register_tuples_in_stmt` `:1040`, `caps_stmt` `:8063`,
and the lowering diagnostic arm `:7922`. **newbf-llvm: NO change** (§2.1). **newbf-runtime: NO
change** (value-struct enumerator allocates nothing). The entire feature is a **newbf-sema
source-synthesis transform**.

### 3.1 The lazy generator transform — seam: a NEW gated edit set inside `rewrite_generators`

The transform hooks the **exact** pre-lowering site the eager path uses. `rewrite_generators`
(`lower.rs:5444-5464`) already: walks each input file's members (`collect_file_generator_edits`,
`lower.rs:5406` → `collect_type_generator_edits`, `:5418`), detects a generator
(`stmt_contains_yield`, `lower.rs:5277` + `generator_list_ty_text` returns `Some`,
`lower.rs:5262`), collects `SrcEdit`s (`lower.rs:5251`), applies them (`apply_edits`,
`lower.rs:5382`), and **re-parses the whole rewritten file with a fresh `FileId`**
(`GENERATOR_FILE_BASE = 50_000 + i`, `lower.rs:5447,5456-5457`). The owned `(idx, String,
CompUnit)` triple is kept alive in the returned `Vec` for the rest of `lower_program`
(`lower.rs:5461`) and substituted into the source set so every downstream walk
(`StructTable::build` `:5541`, lambda/tuple/mono collection, `lower_items`, ownership) sees only
the desugared source. **This is the only sanctioned synthesis mechanism** (the comptime
`emit.rs` precedent): identifiers in this AST are source-slicing `Span`s (`token.rs:33`,
`&src[lo..hi]`) that **cannot be fabricated** — the transform must emit the whole enumerator as
**source text**, never AST mutation.

**The actual edit machinery (corrected — it is `SrcEdit`, not "re-emit the file").** `apply_edits`
(`lower.rs:5382-5399`) sorts disjoint `SrcEdit{lo,hi,text}`s by `(lo,hi)`, splices each into the
original `src`, then appends the untouched `src[cursor..]` tail; it **assumes non-overlapping
edits** and silently drops any edit whose `lo < cursor` (`lower.rs:5388`, the `if e.lo >= cursor`
guard with no else). The lazy path therefore CANNOT reuse `collect_generator_edits`'s edit set
(which inserts a prologue/epilogue inside the *same* method body); it builds its **own** edit set
of exactly two kinds:

- **(i) a whole-body REPLACEMENT** `SrcEdit{ lo: bspan.lo, hi: bspan.hi, text: "{ … construct
  + seed + return __GenN … }" }` over the generator method's `{ … }` block span (the body is a
  `MethodBody::Block(Stmt::Block { .. })`, span via `block.span()`, exactly the handle
  `collect_generator_edits` uses at `lower.rs:5358`);
- **(ii) a RETURN-TYPE replacement** `SrcEdit{ lo: ret_ty.span().lo, hi: ret_ty.span().hi,
  text: "__GenN<E>" }` (§2.4 — the load-bearing edit);
- **(iii) an END-OF-FILE APPEND** `SrcEdit{ lo: src.len(), hi: src.len(), text: "<the whole
  __GenN<E> struct decl>" }` (a top-level `Item::Type`, §2.3). `apply_edits`'s final
  `out.push_str(&src[cursor..])` makes an append at `src.len()` land correctly. This is a
  **first-of-kind edit** (the eager path only does *in-place* splices within an existing method;
  it never grows the item list) — its failure mode is that the appended text must be a
  **syntactically complete top-level decl** or the whole re-parse derails. LT-T2a pins a unit
  test that the appended `__GenN` source re-parses to an `Item::Type` and lands in
  `index_generic_decls`.

All three are disjoint from each other and (because `[Coroutine]` takes a separate code path)
never coexist with the eager prologue/epilogue/yield edits for the same method, so the
non-overlap invariant `apply_edits` relies on holds.

**The lazy transform adds this NEW edit-collection path, gated, leaving the eager path intact:**

1. **Gate (LT-T0).** A generator opts into the lazy path via a marker attribute on the method,
   **`[Coroutine]`** (NOT `[Lazy]` — `[Lazy]` collides with the real Beef `System.Lazy<T>`
   class, `beef-tests/corlib-slice/Lazy.bf:12`, a lazy-init wrapper; reusing the spelling would
   mislead anyone porting Beef). The marker is recognized by a new `has_coroutine_attr(attrs,
   src)` helper (modeled exactly on `has_comptime_attr`, `lower.rs:12869`, / `attr_simple_name`,
   `lower.rs:12912`), and registered in `ATTR_BUILTIN_MARKERS` (`lower.rs:5086-5096`) so the
   custom-attribute resolver does not treat it as an unresolved user attribute class. **The seam
   requires a one-line pattern change:** `collect_type_generator_edits` (`lower.rs:5418`)
   currently matches `Member::Method { return_ty, body, .. }` (`:5421`) and **drops
   `attributes`** via the `..`; widen the arm to `Member::Method { attributes, return_ty, body,
   .. }` (the field exists, `ast.rs:880`) and thread the already-in-scope `src`. **Default =
   eager** (a non-`[Coroutine]` method calls the **unchanged** `collect_generator_edits`
   verbatim in the else, so the three pinned eager tests stay byte-identical — LT-T0's
   acceptance diff-checks the eager `SrcEdit` output is byte-identical, not merely that the
   run-corpus values match). A `[Coroutine]` generator whose body is **not** a feasible shape
   (§5) emits a stderr diagnostic and falls back to eager (never a panic).
2. **Feasibility classify (LT-T1).** A new `classify_generator_shape(body) -> {StraightLine,
   SingleLoop(LoopForm), Unsupported}` — a structural match mirroring `stmt_contains_yield`'s
   recursion (`lower.rs:5277-5296`). `StraightLine` = top-level block of statements with
   `yield return`s and **no** loop containing a yield. `SingleLoop` = **exactly one** loop whose
   body contains yields and no nested loop-containing-yield; the classifier must enumerate the
   **three distinct loop ASTs** (because each desugars differently and the resume `MoveNext`
   must re-implement each):
   - `LoopForm::Range` — `Stmt::ForEach { iter: Expr::Binary { op: Range | ClosedRange, .. } }`
     (this is what `for (var i in 1...n)` parses to — a **`ForEach`, NOT a `For`** — lowered
     today by the range branch at `lower.rs:7425-7483`, which synthesizes the counter/bound as
     fresh allocas at `:7447-7450` and picks `Sle` for `...` / `Slt` for `..<` at `:7460-7464`);
   - `LoopForm::While` — `Stmt::While { cond, body, .. }`, incl. `while (true)`;
   - `LoopForm::CFor` — `Stmt::For { .. }`, incl. the infinite `for (;;)`.
   Anything else (nested loops, list-`foreach` over a collection, `try`/`finally`,
   `yield` in `defer`/`switch`) = `Unsupported`.
3. **Liveness + induction-state (LT-T1 — THE hard sub-task), partitioned into three disjoint
   sets** (the previous flat `{i, n}` framing double-counted a captured arg as both a param and
   a spill — keep them separate):
   - **(a) Captured args.** Every method parameter referenced anywhere in the body after the
     first `yield` → one typed `__GenN` field per param, seeded in the rewritten body (step 4),
     and every in-body reference to the param rewritten to `this.m<Param>` (the
     identifier→field rewrite, step 4). For `Upto(int32 n)`, `n` is here (a *captured arg*), NOT
     a cross-yield local.
   - **(b) Synthesized loop-induction state.** For `LoopForm::Range`, the desugared counter/bound
     are **compiler inventions** (no user-visible local — `lower.rs:7447-7450` makes them fresh
     allocas): synthesize `int mCur` (the counter, init `lo`), and `int mHi` (the bound, init
     `hi`); pick the predicate from the operator (`...`→`<=`, `..<`→`<`, `lower.rs:7460-7464`)
     and the step `mCur = mCur + 1`. For `While`/`CFor`, re-emit the user's test/update verbatim
     (field-qualified) — there is no synthesized counter.
   - **(c) Cross-yield-live user locals.** The general rule (covers pre-loop, pre-yield, and
     between-yield locals — NOT just "loop-header" locals): **any local whose declaration
     lexically precedes a `yield` and which is read on any path after that `yield`.** This
     captures the `lazy_take_infinite.bf` headline case `var i = 0; for(;;){ i = i+1; yield
     return i; }` where `i` is declared *before* the loop and read across the yield (the prior
     "header locals only" rule **lost** it). Each → one typed `__GenN` field, the in-body
     identifier rewritten to `this.m<Local>`.

   LT-T1's unit-test oracle asserts the **partition** (a, b, c) as a span-keyed map, not a flat
   name set, so a captured arg cannot be double-counted as a spill (which would emit two `mN`
   fields / a name collision).
4. **Synthesize the enumerator source + the identifier→field rewrite (LT-T2a/b).** Emit, as the
   end-of-file append edit (iii), the top-level `struct __GenN<E…> { int mState; <field per
   captured arg (a)>; <field per induction slot (b)>; <field per cross-yield local (c)>; E
   mCurrent; bool MoveNext() { switch (mState) { … } } E Current { get { return mCurrent; } }
   void Dispose() {} __GenN<E…> GetEnumerator() { var c = this; c.mState = 0; return c; } }`.
   The `MoveNext` body is the resume switch (§3.2). **The identifier→field rewrite** (turning
   each captured-arg / cross-yield-local identifier in the yielded expressions and loop test into
   `this.m<Name>`) is performed by **regenerating each sub-expression from its AST span** with
   field-qualified references — NOT a blind textual `n`→`this.mN` replace (which would corrupt
   substrings like `n` inside `count`). Each yielded expression and each loop-test/update
   sub-expression is re-emitted span-by-span with its leaf identifiers substituted.
5. **Rewrite the generator method body + return type (LT-T2a).** Edit (i) replaces the body with
   `{ __GenN<E…> g = ?; g.mState = 0; g.<argField> = <arg>; …; <init induction fields>; return
   g; }` (construct + seed captured args + init induction slots + initial state). Edit (ii)
   replaces the return type with `__GenN<E>` (§2.4). **No `delete`** — the result is a value
   struct (§2.3).
6. **Re-parse + substitute.** Exactly as today (`lower.rs:5455-5457` re-parse; the owned
   `String` kept alive in the returned `Vec`). Because the spliced body + appended struct are
   ordinary source, the existing `collect_insts_stmt` walk (`lower.rs:2229`) sees `__GenN<E>` /
   member accesses and monomorphizes them normally; `lower_method` (`lower.rs:6594`) lowers the
   synthesized `MoveNext` with locals → entry-block allocas (`:6661-6666` spill `this`;
   params/locals spill likewise), so **the alloca-everything codegen handles SSA dominance**
   (§6.1).

### 3.2 The `MoveNext` resume switch (the heart) — synthesized source, lowered by the existing switch

The synthesized `MoveNext` body for the straight-line `[Coroutine] List<E> Gen()
{ yield return a; yield return b; }` (no cross-yield locals):

```beef
public bool MoveNext() {
    switch (this.mState) {
    case 0: { this.mCurrent = <a>;  this.mState = 1; return true; }
    case 1: { this.mCurrent = <b>;  this.mState = 2; return true; }
    default: { return false; }
    }
}
```

For a range single loop `[Coroutine] List<int32> Upto(int32 n) { for (var i in 1...n) yield
return i; }` (captured arg `n` → `mN`; synthesized induction `mCur`/`mHi`; predicate `<=` from
`...`):

```beef
public bool MoveNext() {
    switch (this.mState) {
    case 0:  { this.mCur = 1; this.mHi = this.mN; this.mState = 1; }  // loop-entry: init, fall to test
    case 1:  {                                                     }  // resume: re-enter at the test
    default: { return false; }
    }
    // shared loop test+body, straight-line AFTER the switch (reached from case 0 and case 1):
    if (this.mCur <= this.mHi) {
        this.mCurrent = this.mCur;
        this.mCur = this.mCur + 1;
        this.mState = 1;
        return true;
    }
    this.mState = 2;
    return false;
}
```

This **lowers entirely through existing machinery**: `switch (this.mState)` → the chained
`cmp`/`cond_br` (`lower.rs:7792-7861`, **no IR change**, and `int` scrutinee so NOT the
enum-match path at `:7786-7791`); each field read/write → the ordinary `this`-relative
`field_addr`/`load`/`store`; `return true/false` → the ordinary return arm.

**Pinned control-flow invariant (load-bearing, previously undocumented).** The resume switch's
non-returning cases (`case 0`, `case 1`) **fall through to straight-line post-switch code** (the
shared `if` test), relying on the existing `Stmt::Switch` lowering: each non-terminated case body
branches to `switch.exit` (`lower.rs:7872`) and `self.switch(exit)` (`:7876`) continues from
`exit` into the next statement. This is **only** safe because the shared loop test after the
switch is emitted as **straight-line code (an `if`, NEVER a loop)** — a loop after the switch
would rebind `cont` (the switch pushes a loop frame with `cont = self.loops.last()…unwrap_or(exit)`,
`lower.rs:7803/7862`). **Therefore the synthesized `MoveNext` case bodies must contain NO
`break`/`continue`** (only assignments + `return`), so the switch's loop frame is never targeted.
LT-T1's acceptance pins a tiny **hand-written** concrete enumerator with this exact
`switch(mState){…} <shared if>` shape (no synthesis) so the IR pattern is proven *before* LT-T2b
depends on it.

**No value crosses a block edge except through `this`'s fields** — every cross-yield value is a
**field of the value-struct enumerator body** loaded fresh on each `MoveNext`, so the SSA
"instruction does not dominate all uses" trap (MEMORY) **cannot arise** (§6.1). The transform's
job is purely to **emit this source** with the right field set and state numbering — the
liveness/induction analysis (§3.1 step 3) decides which locals/inductions become fields.

### 3.3 The consumer side — the 5th `foreach` branch is UNCHANGED (the decoupling proof)

`foreach (x in Gen(args))` already works with **zero changes** to `Stmt::ForEach` — **given the
return-type rewrite** (§2.4) so `coll_ty = Struct(genN_id)` skips the 4th (Count/Get) branch and
reaches the 5th. The 5th branch (`lower.rs:7619-7747`): probes `coll_ty` (`Ref(cid)|Struct(cid)`,
`:7619`) for `GetEnumerator` (`:7620-7623`, `.cloned()` mandatory), takes `eid` from `ge.ret`
(`:7625`), probes `MoveNext`/`get_Current`/optional `Dispose` (`:7627-7639`), materializes a
value-struct receiver into an alloca for `GetEnumerator`'s `this` (`:7658-7664`), evaluates the
enumerator once into `e_slot` via `call_instance_on_ptr` (`:7666-7669`), and drives the
head/body/cont/exit loop reusing the **same `e_slot` address as `this`** for MoveNext/Current
across all iterations (`:7706-7723` — the load-bearing value-struct aliasing). `Dispose` runs
exactly once on every exit edge via the `ScopeAlloc::DisposeHook` registered at `:7688-7697`
(`free_scope_alloc` emits it at `:7003-7015`; `free_all_scopes` on `return`, `lower.rs:7299-7301`;
`free_scopes_down_to` on `break`, `:7758`). **The lazy enumerator plugs straight in** — it
exposes `GetEnumerator`/`MoveNext`/`get_Current`/`Dispose` by name on a concrete value struct,
exactly the protocol the branch resolves. **No loop-lowering change; this is the entire reason
lazy-yield ships without generic interfaces** (§7).

### 3.4 Ordering (pinned)

The lazy `rewrite_generators` path runs in `lower_program` **before** `StructTable::build`
(`lower.rs:5541`)/`collect_insts` **and** ownership — identical to the eager path's pinned
ordering (iterators.md §3.4). So every downstream walk — monomorph collection (`record_inst`),
tuple/lambda collection, AND `ownership.rs`'s wildcard-terminated `Stmt` walks — only ever sees
the **desugared** `__GenN<…>` struct + the `switch`-bodied `MoveNext`, never a raw `YieldReturn`.
The synthesized `__GenN<E>` instantiation is collected by `collect_insts_stmt` (`lower.rs:2229`)
like any `new List<E>()`, and monomorphized by the **existing** generic-value-struct path
(`index_generic_decls` `:716` → `record_inst` `:1745` → `register_mono` `:907`).

## 4. Worked examples (the run-corpus programs that prove it)

All under `e:/NewBF/beef-tests/run-corpus/`, `Program.Main -> int32`, `// expect: N`, JIT-run
full-i32 value checks under the Stomp guard (the **authoritative** gate, `run_corpus.rs`). The
three pinned **eager** programs (`yield_eager_basic.bf → 6`, `yield_break.bf → 3`,
`yield_empty.bf → 0`) and the four enumerator programs (`foreach_getenumerator.bf → 60`,
`foreach_enum_break.bf → 30`, `foreach_dispose_once.bf → 1`, `foreach_dispose_return.bf → 1`)
plus `enum_manual.bf → 6` must stay green (verified present). **Every lazy program iterates the
generator INLINE** (`foreach (x in Gen())`) — NOT `var ns = Gen(); … delete ns;` — because a
`[Coroutine]` result is a value struct that must not be `delete`d (§2.3); this deliberately
diverges from the eager corpus's `delete ns` idiom (`yield_eager_basic.bf:21-23`,
`yield_break.bf:25-27`).

0. **`lazy_straightline.bf` — `expect: 6`** (the minimal lazy proof, LT-T2a). `[Coroutine]
   List<int32> Nums() { yield return 1; yield return 2; yield return 3; }`; `foreach (x in
   Nums()) sum += x` → 6. Pins: the return type rewrites to `__GenN<int32>`; the synthesized
   `__GenN` struct monomorphizes; the resume switch advances `mState` 0→1→2→3 across three
   `MoveNext` calls; `Current` reads `mCurrent`; the 5th branch (NOT the Count/Get branch) drives
   it unchanged. Same `expect` as the eager `yield_eager_basic.bf` — proving the lazy path is
   *value-equivalent to eager for the inline-`foreach` shape* (NOT `delete`-equivalent: §2.3).

1. **`lazy_loop.bf` — `expect: 6`** (range single-loop resume + induction synthesis + cross-yield
   spill, LT-T2b). `[Coroutine] List<int32> Upto(int32 n) { for (var i in 1...n) yield return i;
   }`; `foreach (x in Upto(3)) sum += x` → 1+2+3 = 6. Pins: the captured arg `n` → `mN`; the
   range `1...n` desugar is re-implemented as `mCur`/`mHi` with the `<=` predicate from `...`
   (§3.1 step 3b / `lower.rs:7460-7464`); the resume switch re-enters at the shared loop test;
   the spilled state survives across the `MoveNext` boundary. **The riskiest single behavior** — a
   miscompiled spill/induction would loop forever / read stale state under the Stomp guard.

2. **`lazy_take_infinite.bf` — `expect: 10`** (THE proof that laziness *exists*, LT-T2b). The
   canonical unbounded shape (pinned, no "substitute if needed" hedge): `[Coroutine] List<int32>
   Naturals() { var i = 0; while (true) { i = i + 1; yield return i; } }` — `i` is a **pre-loop
   body local** (cross-yield-live by §3.1 step 3c, the rule the old "header-only" framing
   missed), and `while (true)` is `LoopForm::While` (the classifier explicitly accepts it). An
   **unbounded** generator the eager path could **never run** (it would fill `List` until OOM /
   hang). `Main`: `int sum = 0; int taken = 0; foreach (x in Naturals()) { sum += x; taken += 1;
   if (taken == 4) break; } return sum;` → 1+2+3+4 = 10. Provable only if the generator is
   **resumable**. Pins the lazy semantics + the `break` edge + exactly-once `Dispose` on break
   (the `ScopeAlloc::DisposeHook`, `lower.rs:7688-7697`). **To kill the off-by-one aliasing risk**
   (an off-by-state-number that still sums to 10), `Main` ALSO asserts `taken == 4` exactly
   (folds into the return only on success), so a resume that yields `2+3+4+5` or `0+1+2+3` cannot
   alias to the expected value.

Each `.bf` is self-contained (corlib `List<T>` is in the prelude for the element-type
convention). The `expect` values fit in i32 (well under the 8-bit AOT-probe caveat; these run
under the JIT harness anyway, per MEMORY).

*(The eager substrate's `Dispose`-on-`return`-edge discipline is already pinned by the landed
`foreach_dispose_return.bf`; the lazy `Dispose` is a synthesized no-op (§3.2), so a separate
`lazy_dispose_return.bf` would either retread that eager test or require synthesis customization
v1 does not have — it is NOT a v1 corpus program. The lazy `Dispose`-on-`break` edge IS exercised
by `lazy_take_infinite.bf` above.)*

## 5. v1 scope vs explicitly deferred (HONEST)

**In v1 (lazy half):**
- **Lazy / coroutine `yield`** for **feasible generator shapes** — **straight-line**
  (`yield return a; …`) and a **single loop** (range `ForEach`, `While`, or C-style/infinite
  `For`) — via a compiler-synthesized **top-level concrete generic value-struct enumerator**
  (`__GenN<E>`) with `int mState` + a resume `switch` + per-captured-arg fields + synthesized
  range-induction fields + per-cross-yield-live-local fields + `mCurrent`, gated behind
  `[Coroutine]` so the eager path stays default. **The generator's return type is rewritten to
  `__GenN<E>`** so the consumer reaches the 5th `foreach` branch (§2.4). Driven by the
  **unchanged** 5th `foreach` branch. **Independent of generic-interface monomorphization** (§7).
- Value-equivalent to eager **for the inline-`foreach` shape** for finite sequences (NOT
  `delete`-equivalent — the result is a value struct, §2.3); **genuinely lazy** for unbounded
  sources (`lazy_take_infinite.bf`, the eager path cannot run).

**Deferred (honest — the genuinely hard cases):**
- **Nested loops** (the resume point is a *tuple* of counters; the dispatch must restore an
  entire loop nest), **`try`/`finally` around a `yield`** (the finally must run on `Dispose`
  mid-iteration — interacts with the `DisposeHook` + the Stomp guard), **`yield` inside a
  `defer`/`switch` arm with complex fall-through**, and **list-`foreach`** as the loop (iterating
  another collection while yielding). These need a **general CFG-to-state-machine transform +
  real dataflow liveness**, not a structural source rewrite — the codebase has no liveness pass
  today (§6.1). The v1 classifier (`classify_generator_shape`, §3.1) emits a diagnostic + falls
  back to eager for these.
- **`yield` in an expression / sub-expression position** — already **syntactically excluded**:
  `yield_stmt` (`parser.rs:2079-2099`) requires statement-form `yield return`/`yield break`;
  there is no bare `yield e;`. Leave it that way.
- **Heap (`Ref`) lazy enumerator + auto-`delete` ownership + large-aggregate captures.** v1's
  synthesized enumerator is a **value struct** (fixed, small state shape) precisely to sidestep
  heap ownership (§2.3/§6.4) — which also caps v1 captures to scalar/small-aggregate (a value
  enumerator is copied into `e_slot`, `lower.rs:7666`, so a large captured struct bloats the
  copy). A heap enumerator (unbounded / large captured state) needs the deferred *"wire the heap
  body into a `ScopeAlloc` for exactly-once free"* work (the §5-deferred item in iterators.md; the
  `DisposeHook`, `lower.rs:7003-7015`, calls `Dispose` but **not** `delete`). The Stomp guard
  catches double-free / use-after-free but **not a missed free (leak)** — so a heap lazy
  enumerator is unsafe to ship before that work. **Deferred.**
- **Generator return-type inference** — v1 keeps the explicit `List<E>` *surface* declaration
  (`generator_list_ty_text`, `lower.rs:5262`) to extract `E`, then rewrites it to `__GenN<E>`;
  no inference of the element type. Inferring an `IEnumerable<E>` surface return re-enters the
  generic-interface dependency (§7) and is **not** a small follow-on.
- **Interface-typed `IEnumerator<T>` / `IEnumerable<T>` enumerators + `foreach` over an
  `IEnumerable<T>`-typed value + dynamic dispatch.** Hard-blocked on **generic-interface
  monomorphization** (§7) — a **separate** work item. There are **no `IEnumerator`/`IEnumerable`
  interfaces in `newbf-corlib` today** (verified: zero matches across `newbf-corlib`; the
  `beef-tests/feature-suite`/`corlib-slice` copies are out of this scope). Do **NOT** couple this
  to lazy-yield.

## 6. Load-bearing risks + mitigations

- **The cross-yield spill + induction synthesis (the SSA-dominance trap) — THE hard part, the
  headline risk.** A lazy `MoveNext` resumes *after* the last yield, so every value **live across
  a yield** must survive the `MoveNext` return. An SSA register defined before a yield and used
  after is the classic "instruction does not dominate all uses" trap (MEMORY). *Mitigation:* the
  transform **spills every captured arg / cross-yield-live local / synthesized induction slot
  into a `__GenN` field** and the synthesized `MoveNext` body reads/writes those fields — so the
  value lives in the **value-struct enumerator body**, loaded fresh on each `MoveNext` via
  `field_addr`/`load`, and **never stays in an SSA register across the resume boundary**. The
  synthesized body is **ordinary source**, so `lower_method`'s alloca-everything codegen
  (`lower.rs:6661-6666` spills `this`; locals → entry-block allocas) handles the in-method SSA
  discipline, exactly as the eager/5th-branch precedent (which *never* lets a non-trivial SSA
  value cross a block edge, `lower.rs:7706-7723`, everything through allocas). The **genuine
  difficulty is the liveness/induction analysis** (§3.1 step 3) — bounded to the feasible shapes
  (straight-line has ~none; single-loop = captured args + synthesized range induction + pre-loop
  cross-yield locals). `lazy_loop.bf` + `lazy_take_infinite.bf` (§4) pin it.
- **The consumer takes the WRONG `foreach` branch if the return type is not rewritten.** If
  `Gen()` still declared `List<E>`, `coll_ty = Ref(list_id)` → the 4th (Count/Get) branch fires
  (`lower.rs:7542-7606`), calling `List.Count/Get` on a `__GenN` value (miscompile/Stomp fault),
  and the 5th branch is never entered. *Mitigation:* §2.4/§3.1 step 4 rewrite the return type to
  `__GenN<E>` (a `Struct`, no Count/Get) so the 4th branch's `if let IrType::Ref` guard
  (`:7543`) fails and the 5th branch fires. `lazy_straightline.bf` (the smallest case) catches a
  regression here under the guard.
- **Ratchet breakage (eager path must stay byte-identical).** The 3 eager-yield programs + 5
  pinned foreach programs + verify corpus + parser corpus are behavior-pinned. *Mitigation:* the
  lazy path is **gated behind `[Coroutine]`** (§3.1 step 1); a non-`[Coroutine]` generator takes
  the **unchanged** eager edit-collection (`collect_generator_edits`, `lower.rs:5354`), so all 3
  eager tests stay byte-for-byte (LT-T0 diff-checks the eager `SrcEdit` output). The 5th `foreach`
  branch is **reused unchanged** (`lower.rs:7619-7747`). Acceptance names every pinned file.
- **First-of-kind: a SYNTHESIZED generic value struct with a non-trivial mutated state set + an
  EOF-append edit.** `ListEnumerator<T>` proves a 3-field generic value struct with
  state-mutating methods runs under JIT/Stomp (`enum_manual.bf → 6`, `List.bf:205-222`), but a
  **synthesized** `__GenN` has a *larger* mixed-type field set (state + captured args + induction
  + spilled locals + `mCurrent`), AND the EOF-append (edit iii) grows the item list — a new code
  path. *Mitigation:* the value-struct `this`-aliasing discipline (`lower.rs:7706-7723`) must hold
  across the larger state — pinned by `lazy_loop.bf` (multiple mutated fields) under the guard;
  the ABI is inherited (not new), only the field count grows. LT-T2a pins a unit test that the
  appended `__GenN` source re-parses to an `Item::Type` and lands in `index_generic_decls`.
- **Monomorph keying / the (name,arity) fixpoint.** The synthesized `__GenN<E>` must be collected
  by `index_generic_decls` (`lower.rs:716-753`) — *top-level only* (§2.3). *Mitigation:* append it
  as a top-level `Item::Type` (the `ListEnumerator<T>` precedent); it then travels `record_inst`
  (`lower.rs:1745`) like any generic value struct. A nested emit would silently lower to `Ptr` and
  bail at `lower.rs:7625` — the acceptance JIT-run catches it.
- **Synthetic names + immutable borrowed AST.** The transform cannot fabricate `Span`-backed
  identifiers (`token.rs:33`) and cannot mutate the borrowed AST. *Mitigation:* emit the whole
  enumerator + rewritten method/return-type as owned source via three disjoint `SrcEdit`s (§3.1)
  + re-parse with a fresh `FileId` (`lower.rs:5455-5457`), kept alive in the returned `Vec` — the
  same `apply_edits` mechanism the eager path uses, plus the EOF-append (edit iii).
- **Identifier→field rewrite corruption.** A blind textual `n`→`this.mN` replace would corrupt
  substrings (`n` in `count`). *Mitigation:* re-emit each yielded expression / loop test
  span-by-span from the AST, substituting only leaf identifiers (§3.1 step 4).
- **Exactly-once `Dispose` for the lazy enumerator (incl. `return`/`break`).** *Mitigation:* the
  5th branch already registers `Dispose` as a `ScopeAlloc::DisposeHook` (`lower.rs:7688-7697`)
  that fires on every exit edge (`free_scope_alloc`, `:7003-7015`; `free_all_scopes` on return,
  `:7299-7301`; `free_scopes_down_to` on break, `:7758`). The lazy value enumerator's `Dispose`
  is a no-op, but the hook path is exercised. `lazy_take_infinite.bf` (§4 ex 2) pins the `break`
  edge under the Stomp guard.
- **sema ⊥ llvm boundary (HARD INVARIANT).** Everything is a newbf-sema source rewrite emitting
  IR shapes the eager path + 5th branch + generic value structs already emit. *Mitigation:* no
  new IR instruction → newbf-llvm untouched (§2.1).
- **Comptime sandbox — NOT involved.** The lazy transform uses the comptime *parser/FileId*
  mechanism (splice/reparse) but is **never** routed through `newbf-comptime` evaluation — it is
  a pure-sema source rewrite. No JIT-FP-constant-pool concern (MEMORY); no float constants.

## 7. Cross-feature dependency (generic-interfaces #1)

**Lazy-yield is INDEPENDENT of generic-interface monomorphization — this is the decoupling
decision.** The synthesized enumerator is a **concrete monomorphized generic value struct**
resolved **statically by name** on the concrete type by the 5th `foreach` branch
(`lower.rs:7619-7639`) — **no interface type, no dynamic dispatch**. The generic-value-struct
monomorph path it rides (`index_generic_decls` → `record_inst` → `register_mono`,
`lower.rs:716/1745/907`) **works today for generic value structs and classes** — just **not for
generic interfaces**, which are explicitly excluded at `lower.rs:735-737`
(`td.kind != TypeKind::Interface`). The lazy enumerator is a `struct` (`StructKind::Value`), so
the exclusion **does not touch it**.

**What v1 ships WITHOUT generic-interfaces:** the entire lazy half (§5 v1) — `[Coroutine]`
generators of feasible shapes → synthesized concrete enumerators → driven by the unchanged 5th
branch.

**What the interface-typed half NEEDS from generic-interfaces (deferred, separate work item):**
`foreach` over an `IEnumerable<T>`-typed value, or an `IEnumerator<T>`-typed enumerator, needs:
- generic-interface registration/monomorphization — `index_generic_decls` must stop excluding
  generic interfaces (lift `lower.rs:735-737`) so `IEnumerator<int32>` resolves to
  `Ref(mono_iface_id)` not `Ptr`;
- `collect_iface_own_type` (`lower.rs:1445`) / `collect_iface_bases_type` (`lower.rs:1649`) to
  handle **generic** interface methods — both are gated on `generic_params.is_empty()` today
  (`lower.rs:1453`/`:1653`, the non-generic-only path — itables.md §5/§6/§10);
- an `IEnumerator<T>`/`IEnumerable<T>` interface pair **in corlib** (none exist today: verified
  zero `IEnumerator|IEnumerable` matches across `newbf-corlib`);
- the 5th branch to dispatch `MoveNext`/`Current` through `emit_iface_dispatch` (the itable
  load-vtable + slot-index path) when `ge.ret` / the collection is an interface type, instead of
  the by-name direct call.

Note that even the **value-struct** lazy path re-enters this dependency the moment an
`IEnumerable<E>` surface return type or return-type inference is wanted (§2.4/§5) — so the
interface-typed half is genuinely a *separate* feature, not a small follow-on. This is the
**same** blocker that defers generic-interface `where`-constraints
(`generic-constraints.md §5`, `T : IEnumerator<TElement>`), generic delegates (`Action<T>`), and
comptime generic-T reflection. **If generic-interfaces lands, this design's interface-typed half
becomes a follow-on** that consumes the monomorphized iface id + itable; until then it is
**hard-deferred and decoupled** — the sprint plan can ship lazy-yield in parallel with, and
independent of, generic-interfaces.

## 8. Task breakdown

Each task is agent-assignable with a one-line seed + a concrete acceptance gate. Gates that
must stay green at **every** boundary: verify corpus (`llvm_lowering_verifies_on_real_beef`, the
dynamic `clean == files.len()` ratchet, `corpus.rs:106` — adding a fixture auto-raises the
floor; do not hard-code a count), parser corpus, run-corpus (authoritative), and the **3
eager-yield + 5 foreach + `enum_manual`** pinned programs byte-identical. A task lands only when
its own test plus all prior gates are green.

**LT-T0 — The `[Coroutine]` gate + eager-stays-default (lands the guard FIRST, observable).**
*Seed:* add `has_coroutine_attr(attrs, src)` (model on `has_comptime_attr`, `lower.rs:12869` /
`attr_simple_name`, `:12912`); register `"Coroutine"` in `ATTR_BUILTIN_MARKERS`
(`lower.rs:5086-5096`); widen `collect_type_generator_edits`'s arm (`lower.rs:5421`) to bind
`attributes` and thread `src`; branch on the marker — when **absent**, call the **unchanged**
`collect_generator_edits` (`lower.rs:5354`) verbatim; when **present** but the body shape is
`Unsupported` (LT-T1 classifier), fall back to eager **+ emit a stderr diagnostic** (never
panic). No lazy synthesis yet.
*Accept:* the 3 eager-yield programs (`yield_eager_basic.bf → 6`, `yield_break.bf → 3`,
`yield_empty.bf → 0`) + all 5 foreach + `enum_manual.bf → 6` byte-identical (diff-check the eager
`SrcEdit` output is byte-identical, not just the run values); a new `lazy_fallback.bf` with a
`[Coroutine]` generator of an **unsupported** shape (e.g. nested loops) produces the **correct
eager value** AND its stderr contains the fallback diagnostic (so the gate is provably
*exercised*, not dead); verify + parser + run corpora green. **Observable** — the diagnostic is
the gate's evidence.

**LT-T1 — Shape classifier + cross-yield liveness/induction partition (the hard analysis, no
codegen yet).**
*Seed:* add `classify_generator_shape(body) -> {StraightLine, SingleLoop(LoopForm),
Unsupported}` enumerating the three loop ASTs — `LoopForm::Range` (`Stmt::ForEach` over a
`Range`/`ClosedRange` binary), `LoopForm::While`, `LoopForm::CFor` (a structural recursion
mirroring `stmt_contains_yield`, `lower.rs:5277-5296`) — and a liveness/induction pass returning
the **three-way partition** (§3.1 step 3): captured args, synthesized induction slots (range
`lo`/`cur`/`hi`/pred from `lower.rs:7460-7464`), cross-yield-live user locals (declaration
precedes a yield, read after it). No source emission yet.
*Accept:* unit tests assert (as a span-keyed partition, NOT a flat name set): a 3-yield
straight-line body → `StraightLine`, empty partition; `for (i in 1...n) yield return i` →
`SingleLoop(Range)`, captured-args `{n}`, induction `{cur, hi}`, locals `{}` (so `n` is NOT
double-counted); `var i = 0; while (true) { i = i+1; yield return i; }` → `SingleLoop(While)`,
locals `{i}` (the pre-loop cross-yield local); a nested-loop / `try`-yield body → `Unsupported`.
PLUS a hand-written concrete `.bf` enumerator with the exact `switch(mState){ … } <shared if>`
resume shape (no synthesis) runs to its expected value under the guard — proving the resume-IR
pattern (the §3.2 post-switch fall-through, `lower.rs:7872/7876`) *before* LT-T2b synthesizes it.
No corpus change beyond that one hand-written fixture. **Behavior-preserving.**

**LT-T2a — Straight-line synthesis + the edit machinery (codegen, the simpler half).**
*Seed:* for a `[Coroutine]` **StraightLine** generator, build the three-edit set (§3.1):
whole-body REPLACEMENT (construct + seed + `return __GenN`), RETURN-TYPE replacement to
`__GenN<E>` (§2.4), and the EOF-APPEND of the top-level `struct __GenN<E…>` with `int mState` +
`mCurrent` + the resume `switch` `MoveNext` (§3.2) + `Current`/`Dispose`/`GetEnumerator`;
re-parse with a fresh `FileId` (`lower.rs:5455-5457`). Reuse the 5th `foreach` branch unchanged.
*Accept:* `lazy_straightline.bf → 6` passes under JIT/Stomp (proving the return-type rewrite
routes to the 5th branch, the `__GenN` monomorphizes, the resume switch advances); a unit test
that the appended `__GenN` re-parses to an `Item::Type` and lands in `index_generic_decls`; the 3
eager + 5 foreach + `enum_manual` unchanged; verify corpus green. **Behavior-changing (adds the
straight-line lazy capability).**

**LT-T2b — Single-loop synthesis: cross-yield spill + range induction + unbounded (RISKIEST).**
*Seed:* extend LT-T2a to `SingleLoop` — emit captured-arg fields + induction fields (range
`mCur`/`mHi` + predicate, or re-emitted `While`/`CFor` test/update) + cross-yield-local fields
(LT-T1's partition); synthesize the loop-entry/resume two-state switch + the shared post-switch
`if` test (§3.2); perform the identifier→field rewrite span-by-span (§3.1 step 4).
*Accept:* `lazy_loop.bf → 6` (range induction + captured arg `n` + `<=` predicate) and
`lazy_take_infinite.bf → 10` (the `while (true)` unbounded proof, with the `taken == 4` cross-check
against off-by-one state numbering, §4 ex 2) pass under JIT/Stomp; the 3 eager + 5 foreach +
`enum_manual` unchanged; verify corpus green. **Riskiest task** — the cross-yield spill +
induction synthesis under the guard is where a stale-state/loop-forever miscompile or a guard
fault would surface; isolating it from LT-T2a makes the loop bug independently bisectable.
**Behavior-changing.**

**LT-T3 — Journal + doc cross-link + verify pin.**
*Seed:* add a numbered journal entry (design + outcome) to the **inner repo's** journal
(`e:/NewBF/NewBF/docs/journals/`, continuing the §-sequence past §131 — NOT the outer
`e:/NewBF` repo's §94/§95 sequence); add a focused verify-corpus fixture mirroring `lazy_loop.bf`
(pin the synthesized `__GenN` + resume-switch IR shape); cross-link this design doc.
*Accept:* journal entry present; verify corpus's dynamic `clean == files.len()` ratchet stays at
100% (the new fixture auto-raises the floor); commit pairs with the entry (conventional style +
Co-Authored-By trailer). **Behavior-preserving (test/doc only).**

**Dependency order:** strict chain `LT-T0 → LT-T1 → LT-T2a → LT-T2b → LT-T3` (T0 gates, T1
analyzes + pins the resume-IR pattern, T2a does straight-line + the edit machinery, T2b does the
risky loop spill/induction, T3 pins). **LT-T2b is the critical-path, highest-risk node** — the
cross-yield spill + range-induction synthesis (the liveness transform the codebase lacks),
isolated so it bisects independently of the straight-line success. The interface-typed
`IEnumerator<T>`/`IEnumerable<T>` half is **NOT a task here** — it is a separate work item
hard-blocked on generic-interfaces (#1, §7) and contributes nothing to this v1.

**Final task count: 5** (LT-T0, LT-T1, LT-T2a, LT-T2b, LT-T3).
