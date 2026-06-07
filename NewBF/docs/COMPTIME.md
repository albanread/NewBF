# Comptime

NewBF's compile-time-evaluation and member-emission system, as it exists after
the CB-T0…CB-T6 wave. This documents the **landed implementation** — the code in
`src/newbf-comptime/` (`eval.rs`, `fold.rs`, `emit.rs`), the `EmitJob` plumbing in
`src/newbf-ir/src/module.rs`, the sema recording in `src/newbf-sema/src/lower.rs`,
and the corlib surface in `src/newbf-corlib/bf/Compiler.bf`.

Design rationale and the full risk/alternatives analysis live in
[`docs/design/comptime-breadth.md`](design/comptime-breadth.md); this doc is the
developer-facing description of what was actually built. Where the two differ,
**this doc follows the code.**

> Historical note: an earlier stub described comptime as "a genuine compile-time
> interpreter … invoked re-entrantly from `newbf-sema` through a trait-object
> callback." That is **not** what was built. NewBF deliberately reversed Beef's
> bytecode `CeMachine`: comptime runs on the **same JIT** the application uses,
> and the re-entrancy is an **outer fixpoint loop** in `newbf-comptime` that calls
> `newbf-sema` directly (no callback, no `dyn`, no circular crate dependency).

---

## 1. Overview

**There is no comptime VM.** Comptime is the *same* `newbf-ir → newbf-llvm → ORC
JIT` pipeline the application uses, run at compile time. A `[Comptime]` method
lowers to ordinary IR and is JIT-compiled and called during compilation; its
result feeds back into the compile.

The system has two distinct capabilities:

- **(a) Const-fold** (`eval.rs` + `fold.rs`). A `[Comptime]` function called with
  constant arguments is JIT-evaluated and its call site is replaced by a
  width-correct constant. The comptime function is then dropped — it never reaches
  the final program. *Folding collapses values.*

- **(b) Member emission** (`emit.rs`). A `[Comptime, EmitGenerator]` function emits
  **Beef source text** that is spliced into a type (as an `extension`), re-parsed,
  re-analyzed, and re-lowered — feeding back into resolution so the emitted members
  become callable. This is driven by a **fixpoint loop**. *Emission changes the
  program's shape.*

Both run at compile time, before codegen, in **both** JIT (run/app) and AOT modes.
By the time codegen runs, comptime functions and the emit shim are gone; the
shipped binary contains only ordinary, reparsed-and-lowered code.

The driver runs them in order: **emission first, then folding** (emission changes
shape; folding collapses values). See [§7](#7-where-it-runs-the-pipeline).

```
 source ──► run_emission ──► fold_comptime ──► codegen (JIT run/app, or AOT object+link)
            (shape: emit       (values: fold
             members,           comptime calls
             strip generators)  to constants)
```

---

## 2. The const-fold

### 2.1 `eval_const` — the width-correct evaluator (`eval.rs`)

```rust
pub fn eval_const(module: &IrModule, name: &str, ret: IrType) -> Result<i64, EvalError>
```

`eval_const` JIT-compiles `module` and calls its **nullary** function `name`,
interpreting the return value at the **width and signedness** of `ret`. It is the
beating heart of comptime: the identical IR→LLVM→ORC pipeline the application uses,
run during compilation.

**The type gate runs *before* the JIT.** `eval_const` decides how to read the
result before touching the JIT, so unsupported result types never reach
materialization and can't crash the compiler:

- `IrType::Bool` → `(bits=1, signed=false)`.
- `IrType::Int { bits, signed }` with `bits <= 64` → read at that width.
  `bits > 64` returns `EvalError::WidthTooLarge` (no such type exists in the
  current IR, but the guard is explicit).
- `Void` / `Float { .. }` / `Ptr` / `Struct(_)` / `Ref(_)` → return
  `EvalError::Unsupported` **without JIT-running the function**. Float in
  particular *must not* reach the JIT: the ORC/RTDyld linker cannot resolve
  `__real@` float-constant relocations (see [§8](#8-v1-boundaries) and project
  memory), so attempting it would fail to materialize rather than fabricate a
  value.

**Width interpretation (the load-bearing detail).** The result is read by
transmuting the entry point to `extern "C" fn() -> i64` and calling it. On Win64
the integer return lives in RAX, and for a sub-64-bit return the callee leaves
**RAX's upper bits undefined**. So the raw machine word is **masked to `bits`
first**, then **sign-extended if `signed`, else zero-extended** to the canonical
`i64` the caller stores (`extend_to_i64`). Concretely:

| IR value  | masked      | extended      |
|-----------|-------------|---------------|
| `i32 = -1`| `0xFFFF_FFFF` | sign → `-1i64` |
| `u8 = 250`| `0xFA`      | zero → `250i64` (not `-6`) |
| `i8 = -7` | `0xF9`      | sign → `-7i64` |
| `bool`    | `0x1`       | `0`/`1`       |

`eval_const_i64(module, name)` remains as a thin wrapper —
`eval_const(m, n, IrType::I64).map(Into::into)` — for callers that only need the
i64 case.

**`EvalError`** is a typed enum (`SymbolNotFound`, `Unsupported { name, ret }`,
`WidthTooLarge { name, bits }`, `Jit(String)`) so every failure becomes a clean
diagnostic, never a miscompile. `Display`/`From<EvalError> for String` turn it into
the driver's diagnostic stream.

### 2.2 `fold_comptime` — folding call sites (`fold.rs`)

```rust
pub fn fold_comptime(module: &mut IrModule) -> Result<(), String>
```

A **no-op when `module.comptime` is empty** (the common case — ordinary programs
pay nothing). Otherwise it walks the call sites of the symbols sema recorded in
`module.comptime`, JIT-evaluates each foldable one, rewrites the call into a
width-correct literal, then drops the comptime functions nothing references.

**Foldable** (`is_foldable_ret`) = a non-extern comptime function whose return
type is a width-bounded integer (`i8/i16/i32/i64`, signed or unsigned) or `bool` —
exactly what `eval_const` can read. Float/ptr/struct returns are left as real calls.

**Constant args via a synthesized wrapper.** A call `F(7)` is folded by adding a
nullary `$ct_eval() => F(7)` to a *clone* of the module and JIT-evaluating *that*
(`eval_call`). This marshals arguments without an FFI calling convention; the
wrapper is type-safe by construction because it copies the original (already
type-checked) call. Results are memoized on `(symbol, arg-values)`.

**The fold-width fix (CB-T6).** The rewrite uses the **call instruction's own
`InstData.ty`** — *not* a hardcoded `i64`. A folded `i32`-returning call is
rewritten to an `i32` literal so the result width matches every SSA use and the
module verifies:

```rust
// the folded call instruction becomes an identity `add v, 0` of its OWN type:
InstKind::Bin { op: Add, lhs: Value::int(v, ty), rhs: Value::int(0, ty) }
//                                          ^^ ty = the call's InstData.ty (e.g. i32)
```

The instruction id (and every SSA use of it) stays valid — no operand rewiring —
and LLVM folds the `+0` away.

**Inner-fold-first / fixpoint.** A comptime call whose arguments are themselves
comptime calls — `Outer(Inner(3))` — folds bottom-up. The collect/apply loop
iterates to a **fixpoint** (until a pass folds nothing), bounded by the total
instruction count. The mechanism: once `Inner(3)` folds to `add 4, 0`, `arg_const`
resolves the `Outer` argument (an SSA reference to that identity-add) back to the
constant `4`, so the next pass folds `Outer(4)` too. `arg_const` resolves three
operand shapes to a `(value, type)`:

1. a literal `Const::Int(v, t)`;
2. an **identity `add v, 0` of integer constants** — a previously-folded inner
   comptime call;
3. an **int→int cast of an integer constant** (`trunc`/`zext`/`sext`/`bitcast`) —
   because sema lowers a literal `7` as `i64` and inserts a width cast to the
   parameter type (`F(7)` becomes `F(trunc i64 7 to i32)`), so the call's arg is an
   `Inst` pointing at that cast. The resolved type is the cast's **result** width.

**Dropping comptime functions.** After folding, `reachable_from_ordinary` computes
which comptime symbols are still reachable from non-comptime code (following the
call graph). A comptime function kept alive only by its own recursion is dropped;
one still reached from an unfolded call site is kept (no dangling reference).

### 2.3 Worked examples

- **`comptime_eval_i32_arg.bf`** (expect: **49**) — `[Comptime] int32 F(int32 x) =>
  x*x`, called as `F(7)`. Folds at compile time to the `i32` constant 49, typed at
  the call's own width (the fold-width fix), and `F` is dropped. The proof the call
  was folded, not run.
- **`comptime_nested_fold.bf`** (expect: **40**) — `Outer(Inner(3))`, both
  `[Comptime]`. Folds bottom-up: `Inner(3)` → `4`, then `Outer(4)` → `40`. Both
  comptime functions are dropped; `Main` is left with a single literal — proving the
  collect/apply loop iterates to a fixpoint.

---

## 3. The emission fixpoint loop (`emit.rs`)

```rust
pub fn run_emission(base: &[SourceFile<'_>]) -> Result<(IrModule, EmitOutcome), String>
pub fn run_emission_with(base: &[SourceFile<'_>], config: &EmitConfig) -> …  // explicit caps
```

`run_emission` takes the **source** (a borrowed `SourceFile` set — the same one the
driver/harness hand to `analyze`/`lower_program`) because it re-parses /
re-analyzes / re-lowers every round. Lowering is already a pure `source → Module`
function, so the loop just augments the source set with `extension Owner { … }`
units and re-lowers until no new member is emitted.

### 3.1 The pipeline, per round

1. **Build + lower.** `files = base + generated` (the owned synthesized
   `extension` units), then `analyze(&files)` and `lower_program(&files, &program)`.
   The generated `FileId`s sit at `GENERATED_FILE_BASE = 900_000`, well clear of the
   prelude band (`10_000+`) and user files (`0..n`).
2. **Fast path / fixpoint exit.** If the lowered module records **no `emit_jobs`**,
   strip and return — round 0, a pure pass-through. This is *every generator-free
   program* (the entire current corpus pays nothing).
3. **Per-round owner map.** Build `owner_id_to_qual`: sema injects each generator's
   owner `StructId.0` as the `__newbf_ct_emit` literal; `module.structs[id].name` is
   that id's *simple* name; match it to the `EmitJob.owner_qual_name`. **StructIds
   shift between rounds**, so this map is rebuilt every round and the cross-round
   routing key is always the **qualified name**, never a held StructId.
4. **Run the generators.** `run_generators` clones the module into a sandbox, adds a
   single nullary wrapper `$ct_emit_run() { gen0(); gen1(); … }` calling every
   generator in a **deterministic order** (owner qual name, then symbol), JITs it,
   binds `__newbf_ct_emit` as an absolute symbol, looks up `$ct_emit_run`,
   transmutes to `extern "C" fn()`, and calls it. Then drains `EMIT_SINK`.
5. **Resolve + dedup + splice.** For each drained `(owner_id, text)`: resolve
   `owner_id` → qualified name via the per-round map; compute the dedup key
   `(qual, normalize(text))`; if **new**, splice `extension <qual> { <text> }` into
   an owned `(String, CompUnit)` appended to `generated`.
6. **Loop or stop.** If anything new was spliced, loop; else fixpoint reached.

### 3.2 Determinism

StructIds are assigned by `StructTable::build` order; appending generated units
shifts ids across rounds. Therefore emissions are keyed end-to-end by **qualified
name**, and the JIT-boundary id is resolved via *that round's* `name→id` map.
Generators run in a fixed order and new units are sorted before splicing, so the
emitted-source order — and thus the next round's StructId assignment — is
reproducible.

### 3.3 `normalize` and the dedup key

`normalize(text)` trims, strips `//` line comments, and collapses interior
whitespace runs (including newlines) to a single space. The dedup key is
`EmitKey = (owner_qual_name, normalize(text))`, held in a `seen: HashSet`.
Cosmetically-different re-emissions of the same member (indentation, a trailing
comment) thus map to the same key and dedup — which is the **termination
guarantee** (A emits B, B re-emits B-identical → no new unit → fixpoint).

### 3.4 Worked examples

- **`comptime_emit_member.bf`** (expect: **42**) — the marquee. `Vec2.Generate()`
  (a `[Comptime, EmitGenerator]`) emits `public int32 Sum() { return this.mX +
  this.mY; }`. The emitted `Sum()` reads the **pre-existing** fields `mX`/`mY` — a
  value computable only if emission fed back into resolution: the member was
  reparsed as `extension Vec2 { … }`, re-analyzed, re-lowered, and is now callable.
  `30 + 12 = 42`.
- **`comptime_emit_then_call_twice.bf`** (expect: **42**) — emitted once, called
  from two sites with different receivers. Proves the generated member is a reusable
  symbol like any hand-written method.
- **`comptime_emit_dead_member.bf`** (expect: **7**) — emits a member that is
  **never called**. The generator still *ran*, so the emitter and shim existed
  during emission; the strip ([§4](#4-the-__newbf_ct_emit-ffi-shim-and-the-strip))
  removes them so the final module JIT-links clean despite the dead member. The
  eager-link regression guard.
- **`comptime_emit_idempotent.bf`** (expect: **42**) — **two** generators on the
  same owner emit the *same* (normalized) member text, one with extra whitespace and
  a trailing `// comment`. `normalize` collapses both to one key; the `seen` dedup
  splices `Sum()` exactly once (a second splice would be a `duplicate member`
  analyze error). Proves the dedup guard.
- **`comptime_emit_virtual.bf`** (expect: **42**) — `Dog`'s `override Speak()` is
  emitted, not written. Because the type graph and every vtable are recomputed from
  the full source set each round, the emitted override (spliced as `extension Dog {
  … }`) joins Dog's virtuals/vimpls on rebuild. An `Animal a = new Dog()` then
  dispatches `a.Speak()` through the vtable to the emitted override (42), not
  Animal's body (7). Proves emitted virtuals participate in dynamic dispatch.

---

## 4. The `__newbf_ct_emit` FFI shim and the strip

NewBF has no VM, so the emitter (native JIT'd code) communicates emitted text out
through a **host runtime shim**:

```rust
#[unsafe(no_mangle)]
pub extern "C" fn __newbf_ct_emit(owner_type_id: i32, ptr: *const u8, len: i32)
```

It copies `len` bytes from `ptr` (lossy-UTF-8; negative/zero `len` → empty) and
pushes `(owner_type_id, text)` into a **thread-local sink**:

```rust
thread_local! { static EMIT_SINK: RefCell<Vec<(i32, String)>> = …; }
```

Thread-local because `OrcJit` runs the emitter on the calling thread, in-process;
the loop snapshots-and-clears the sink (`drain_emit_sink`) around each JIT call. The
borrow is never held across the FFI return (text is copied out first), so a
re-entrant emit can't panic on an already-borrowed cell.

**Binding (the MS-T0 seam).** `__newbf_ct_emit` is a `#[no_mangle] extern "C"`
symbol in a statically-linked rlib — *not* a process export — so the ORC
process-search generator can't find it. `run_generators` binds it explicitly by
address, on the **same JIT instance** the generator runs in, **before** the lookup:

```rust
jit.add_absolute_symbol("__newbf_ct_emit", __newbf_ct_emit as *const () as usize)?;
```

The absolute definition wins over the on-demand process-search generator, so there
is **no duplicate-definition error** (proven by the `shim_populates_sink_via_
absolute_symbol` unit test, the retained CB-T2 acceptance gate).

**The corlib surface** (`bf/Compiler.bf`). A `static class Compiler` with two
members:

```beef
[LinkName("__newbf_ct_emit")]
public static extern void Comptime_Emit(int32 ownerTypeId, char8* textPtr, int32 textLen);

[Comptime]
public static void EmitTypeBody(String text) { }   // body NEVER executed — see below
```

A user writes `Compiler.EmitTypeBody(text)`. **Sema (CB-T3) rewrites that call**,
inside an emit-generator body, to `__newbf_ct_emit(<owner-id literal>, text.Ptr,
text.Len)` — the owner id is *injected by sema*, never read from reflection (v1 has
no `Type`/`typeof`). So `EmitTypeBody`'s body is never run; it exists purely so the
call parses and type-checks. `Compiler.bf` rides the prelude exactly like `Type.bf`
(a duplicate corpus `Compiler` is skipped by `register_type_struct`, first/prelude
wins), keeping the verify corpus at 154/154.

**The strip (load-bearing for linking).** Before returning, `strip_emitter_and_
shim` removes:

- every function that **transitively references `__newbf_ct_emit`** *and* is a
  `module.comptime` symbol (i.e. the generators), computed by a fixpoint reverse-
  reachability sweep over the comptime call graph; and
- the `__newbf_ct_emit` extern declaration itself.

The emitted members (now ordinary reparsed source — not comptime, not referencing
the shim) **stay**. This is mandatory: the **app/run JIT and the AOT link never
register the shim** (only the comptime sandbox does), and RTDyld eagerly links the
whole module on first lookup — so a surviving `__newbf_ct_emit` reference would fail
`lookup("Program.Main")` / link with "Symbols not found: [__newbf_ct_emit]". The
strip view agrees with `fold_comptime`'s `reachable_from_ordinary`: both treat
`module.comptime` members reaching the shim as droppable, so neither keeps a
function the other needs. `emit_jobs` is cleared so a downstream inspector sees a
settled module.

A `dump-ir` golden on `comptime_emit_member.bf` asserts: the generated `Sum` symbol
**present**, the generator symbol **absent**, `__newbf_ct_emit` **absent**.

---

## 5. The termination guards (CB-T5)

Emission is a fixpoint loop over JIT'd user code, so it is **triple-guarded**, and a
tripped guard is always *reported*, never a hang or crash:

1. **Dedup over normalized text** (`seen`) — identical re-emissions are idempotent;
   this is the fixpoint exit.
2. **Round cap** (`EmitConfig::max_rounds`, default `16`) — bounds a generator that
   *returns normally but emits divergent text each round* (every emission is new, so
   dedup never fires). On trip the loop **stops** and pushes a non-convergence
   diagnostic into `EmitOutcome::diagnostics`, returning the module-so-far (not an
   `Err`).
3. **Byte cap** (`EmitConfig::max_bytes`, default `1 MiB`) — bounds *total emitted
   bytes* across rounds. A generator emitting unique *growing* text defeats dedup but
   trips this cap (bytes are accounted before dedup), with the same
   stop-with-diagnostic behavior.

```rust
pub struct EmitConfig { pub max_rounds: u32, pub max_bytes: usize }   // Default = (16, 1<<20)
pub const DEFAULT_MAX_EMIT_ROUNDS: u32 = 16;
pub const DEFAULT_MAX_EMIT_BYTES: usize = 1 << 20;
```

Both caps are documented as **anti-cycle backstops, not correctness guarantees** —
a legitimate generator that genuinely needs more rounds/bytes must raise the
relevant cap explicitly. The defaults never affect the corpus (no divergent
emitters).

**Generated-code diagnostics are surfaced.** Once `generated` units exist, if
re-analyzing the spliced sources produces analyze diagnostics — the *emitted* code
is malformed (e.g. a duplicate member) — the loop surfaces every diagnostic into
`EmitOutcome::diagnostics` and **stops**: it does not lower garbage IR (a silent
miscompile) and does not loop forever. (Base-program round-0 diagnostics are the
driver's own `analyze` job, so they aren't double-reported.) A *parse* error in
generated source aborts via `Err` (structural, not a recoverable program
diagnostic).

`EmitOutcome { rounds, diagnostics }` is returned alongside the module; the driver
and run-corpus harness merge `diagnostics` into the diagnostic stream like
parse/sema ones (the run-corpus harness treats any emission diagnostic on a positive
program as a failure). The no-op fast path runs `rounds == 0` with no diagnostics.

Unit tests in `emit.rs` cover each guard: `divergent_emitter_trips_round_cap_with_
diagnostic`, `growing_emitter_trips_byte_cap_with_diagnostic`,
`idempotent_emitter_converges_no_diagnostic`, and `generated_code_analyze_
diagnostic_aborts_with_diagnostic`. They inject a synthetic generator-runner via the
`run_emission_inner` seam, because a real JIT'd generator can't emit per-round-
divergent text without host state.

---

## 6. The IR + sema plumbing

**`newbf-ir` (`module.rs`).** Two `Module` fields carry comptime data as **owned
data only** (no lifetimes, no cross-round `StructId`), so `IrType` stays `Copy`:

```rust
pub comptime: Vec<String>,     // names of [Comptime] fns — what fold/strip may drop
pub emit_jobs: Vec<EmitJob>,   // recorded emit generators — empty ⇒ loop is a no-op

pub struct EmitJob {
    pub owner_qual_name: String, // e.g. "Demo.Vec2" — the cross-round routing key
    pub symbol: String,          // the generator's mangled symbol (nullary void)
}
```

**`newbf-sema` (`lower.rs`).** Sema **records, never invokes** — it stays a pure
`source → Module` producer. During method lowering, when `comptime_emitter_of(attrs,
src)` recognizes a `[Comptime, EmitGenerator]`:

- it pushes the name onto `m.comptime` (so the strip/fold sweep drops it), **and**
- pushes `EmitJob { owner_qual_name, symbol }` onto `m.emit_jobs`, **and**
- declares the `__newbf_ct_emit` extern once with the exact C ABI
  (`void(i32, char8*, i32)`), **and**
- passes the owner `StructId` to `lower_method` as `emit_owner: Option<StructId>`,
  so the body's `Compiler.EmitTypeBody(text)` calls rewrite to
  `__newbf_ct_emit(<owner_id as i32 literal>, text.Ptr, text.Len)`.

The generic-comptime guard is untouched — generic emit generators stay rejected in
v1.

---

## 7. Where it runs (the pipeline)

All three callers run **`run_emission` then `fold_comptime`** on the final module:

- **Driver** (`newbf-driver/src/main.rs`) — in `dump-ir`, `dump-llvm`, and
  `compile`. Each replaces the old inline `lower_program` with `run_emission`
  (which internally analyzes/lowers/splices/strips), merges `EmitOutcome.
  diagnostics` into the driver's diagnostic stream, then runs `fold_comptime`.
- **Run-corpus harness** (`tests/newbf-tests/tests/run_corpus.rs`) — the
  authoritative gate. Routes each program through `run_emission` (asserting no
  emission diagnostics for positives), then `fold_comptime`, then JITs
  `Program.Main`. Generator-free programs hit the no-op fast path, so every existing
  program stays green.

**JIT vs AOT.** Emission and folding resolve **entirely at compile time, before
codegen, in both modes**. By the time AOT `emit_object`/`link` runs or the app JIT
runs, generated members are concrete IR and `__newbf_ct_emit` is gone — both paths
get generated code for free, and the shim never reaches the shipped binary. An AOT
acceptance check compiles+links at least one emission program (the same strip
property is what makes AOT link).

---

## 8. v1 boundaries

What is **in** v1 and what is **deferred**. These are honest limits, matching the
design doc §9/§10.

**In v1:**

- Const-fold of `[Comptime]` functions returning `i8/i16/i32/i64` (signed/unsigned)
  or `bool`, with all-constant integer args, including nested (inner-fold-first) and
  width-correct (the call's own `InstData.ty`).
- Member emission via `[Comptime, EmitGenerator]` + `Compiler.EmitTypeBody(text)`,
  spliced as `extension Owner { … }`, to a fixpoint, with round/byte/dedup guards.
- Emitted methods (including `virtual` overrides) and fields that read pre-existing
  members; emitted members participate in vtables exactly like hand-written ones.
- The strip that makes the final module JIT-link and AOT-link clean.

**Deferred (out of v1):**

- **Float const-eval** — the ORC/RTDyld JIT cannot resolve `__real@` float-constant
  relocations. `eval_const` returns a typed `Err(Unsupported)` for float *without*
  attempting the JIT; `fold_comptime` never folds a float-returning function. A
  float-doing **emitter** also fails to JIT, surfaced as a typed diagnostic, not a
  miscompile. Revisited independently (JITLink / a float-constant materialization
  pass).
- **`Ptr`/`Ref`/`Struct` const-eval** — no heap-value marshalling; typed `Err`.
- **A reflection FFI table / `[Comptime] typeof(T)` / `Type` / `StringView`** — none
  of these lower in the emission path today. The v1 emit surface is
  **primitives-only**: the owner id is *injected by sema* as a literal, not read
  from a `Type`. `typeof` parses but is not lowered.
- **Generic emit generators** — the generic-comptime guard (`lower.rs`) keeps them
  rejected.
- **`EmitAddInterface` / `EmitMixin`** — adding an interface via emission (itable
  recompute) and statement-injection mixins (hygiene + a diagnostic-sink dependency)
  are staged later.
- **Bounded execution** — the round/byte caps bound the *number of rounds*, between
  which the loop regains control. An emitter with an **internal infinite loop**
  (never returns) still **hangs** — there is no mitigation in v1. Likewise an
  emitter **fault** (segfault/abort across the JIT/FFI frame) is **not** recoverable
  into a `Result`: the `newbf-runtime` SEH handler writes a crash dump and the
  process **still dies**. v1 emitters must be **trusted and terminating**; no test
  asserts graceful recovery from a faulting emitter. Out-of-process / SEH-`__try`
  isolation is deferred.
- **Struct-payload edges**, further dedup canonicalization (parse-and-reprint
  signature-level dedup), and nested/namespaced owner-routing corner cases beyond
  the simple-name `by_name` keying are deferred per design §10.

---

## 9. File map

| Concern | Location |
|---|---|
| Width-correct evaluator + `EvalError` | `src/newbf-comptime/src/eval.rs` |
| Const-fold (widened args, width fix, inner-fold-first) | `src/newbf-comptime/src/fold.rs` |
| Emission fixpoint loop, shim, sink, strip, caps | `src/newbf-comptime/src/emit.rs` |
| `EmitJob`, `Module.emit_jobs`, `Module.comptime` | `src/newbf-ir/src/module.rs` |
| Sema recording + body rewrite | `src/newbf-sema/src/lower.rs` |
| Corlib emit surface | `src/newbf-corlib/bf/Compiler.bf` |
| Driver wiring (`dump-ir`/`dump-llvm`/`compile`) | `src/newbf-driver/src/main.rs` |
| Authoritative run-corpus gate | `tests/newbf-tests/tests/run_corpus.rs` |
| Worked-example programs | `beef-tests/run-corpus/comptime_*.bf` |
| Design doc (rationale, alternatives, risks) | [`docs/design/comptime-breadth.md`](design/comptime-breadth.md) |
