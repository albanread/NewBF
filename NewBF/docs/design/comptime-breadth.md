# Comptime Breadth — Emission + the Fixpoint Worklist

> Status: design (implementation-ready). Supersedes the COMPTIME.md stub for the
> emission slice. Verified against the code at the §114 wave (citations are
> `file:line` at time of writing — re-check before editing).

## 1. Problem & goal

Today comptime in NewBF is a **leaf** operation: it can compute a value at compile
time and substitute a literal, but it cannot influence what the compiler resolves.
`eval_const_i64` JIT-runs a nullary `i64`-returning `[Comptime]` function
(eval.rs:21); `fold_comptime` rewrites call sites whose args are all integer
constants into `add v, 0` literals via a synthesized nullary `$ct_eval` wrapper,
then drops the unreferenced comptime functions (fold.rs:36, 122-130). That is the
full extent of the machinery. It is integer-only and `i64`-return-only (fold.rs:48 —
`f.ret == IrType::I64`; the ORC/RTDyld JIT can't resolve `__real@` float-constant
relocations — MEMORY), post-lowering only (driver-orchestrated, never re-entrant
into sema), and value-producing only (no type or member generation).

The genuinely hard part of Beef's `CeMachine` (PLAN §1.6, §2.5c, Phase 7) is
**emission that feeds back into resolution**. A `[Comptime]` method emits **Beef
source text** appended to a type body (`Comptime_EmitTypeBody(typeId, StringView
text)` — Compiler.bf:412-416; CeMachine.cpp:6972-7031), which must be re-parsed and
re-resolved, which can trigger *more* comptime — a **fixpoint worklist**. This is
the keystone integration risk: comptime is invoked *from* the semantic core and
generates types feeding back into resolution, and the `newbf-comptime ↔ newbf-sema`
boundary must allow re-entrancy without a circular crate dependency.

### 1.1 The v1 target (observably testable)

The emitted member must **read a pre-existing field** — that is the case the type
table currently breaks (see §2.1), so it is the proof the slice works:

```beef
// comptime_emit_member.bf  — expect: 42
class Vec2 {
    public int x;
    public int y;

    // A compile-time generator. NO reflection types: it takes no Type/StringView.
    // Sema injects the owner-id literal; the body emits Beef source via a shim.
    [Comptime, EmitGenerator]
    public static void Generate() {
        // EmitTypeBody appends source to Vec2 as an `extension Vec2 { … }` unit.
        // Owner id is supplied by sema (a lowered literal) — see §3.3.
        Compiler.EmitTypeBody("public int SumXY() { return this.x + this.y; }");
    }
}

class Program {
    public static int32 Main() {
        // SumXY did NOT exist in the source; comptime emitted it, the compiler
        // re-resolved Vec2 via an `extension`, and the call below now resolves.
        Vec2 v = scope Vec2();
        v.x = 30; v.y = 12;
        return (int32)v.SumXY();   // 30 + 12 = 42
    }
}
```

The `// expect: 42` value is computable **only if** the generated member exists,
resolves, and correctly reads the original `x`/`y` fields — there is no other path
for `Main` to return it. That is the emission proof.

Plus const-eval breadth bounding the float gap: **non-`i64` integer returns and
arguments** (`i8`/`i16`/`i32`/`i64`/`bool`/`char8`), and **non-constant-but-
comptime-known args** (an arg that is itself a foldable comptime call). Float
const-eval stays scoped *out* of v1 (the `__real@` gap), surfaced as a clean
diagnostic rather than a miscompile.

### 1.2 Non-goals for v1 (staged later, §9 tail)

A reflection FFI table (`GetReflectType`/`GetString`, `Type`/`typeof` lowering),
`StringView`, generic-method comptime (kept rejected per lower.rs:1536), bounded
execution (step/time/memory caps; emitter-fault-to-`Result` recovery),
`EmitAddInterface`, `EmitMixin`, and float const-eval.

## 2. Current state (verified)

- **eval core** — `eval_const_i64(module, name) -> Result<i64,String>` (eval.rs:21):
  `OrcJit::from_ir`, `lookup`, `transmute` to `extern "C" fn() -> i64`, call
  in-process (eval.rs:29). Hardcoded nullary `i64`.
- **fold pass** — `fold_comptime(&mut IrModule)` (fold.rs:36): no-op if
  `module.comptime` empty; `foldable` = comptime ∧ non-extern ∧ `ret==I64`
  (fold.rs:45-50); a call folds only if every arg is `Const::Int` (fold.rs:70-77);
  evaluated via a nullary `$ct_eval` wrapper in a *clone* (fold.rs:122-130);
  memoized on `(symbol, args)` (fold.rs:79-87); **rewrite hardcodes I64**
  (`Bin{Add, int(v,I64), int(0,I64)}`, fold.rs:96-100); drop via
  `reachable_from_ordinary` (fold.rs:109-112,137-173).
- **sema marking** — `has_comptime_attr(attrs, src)` (lower.rs:9278);
  `m.comptime.push(full_name)` during method lowering (lower.rs:4168-4169). Sema
  records, never invokes.
- **the generic-comptime guard** — comptime generics are not lowered (lower.rs:1536;
  lib.rs analyze treats them as a clean no-error def-graph). This stays.
- **driver** — `fold_comptime(&mut module)` runs **after** `lower_program` in
  `dump_ir`/`dump_llvm`/`compile` (main.rs:293, 352, 412). **The run-corpus harness
  calls NEITHER `fold_comptime` NOR anything comptime** (run_corpus.rs:41-43): it
  `analyze`s, `lower_program`s, and JITs `Program.Main` directly. Any comptime
  feature observable in the authoritative gate must be invoked there.
- **`lower_program(files, _program)`** (lower.rs:3429) ignores `_program`; it
  re-parses the corlib prelude (lower.rs:3433-3447), composes user files
  (lower.rs:3448-3454), `StructTable::build(&all)` (lower.rs:3457), then lowers.
  **The compiler is already a source-in / module-out function, and the prelude
  proves "inject extra source and lower the whole thing once."**
- **`analyze(files) -> Program`** (lib.rs:58) builds the def-graph and runs
  `resolve_and_check`, returning `Program.diagnostics` (lib.rs:63,74). `Program`
  carries diagnostics; `lower_program` has **no** diagnostic sink.
- **`SourceFile<'a>`** borrows both `src: &'a str` and `unit: &'a CompUnit`
  (build.rs:16-19). `lower_program`/`analyze` take `&[SourceFile<'_>]`.
- **IR** — `Module.comptime: Vec<String>`; `IrType` is `Copy`, `StructId(u32)`
  (ty.rs:9-13); **`IrType::Int { bits, signed }`** distinguishes signedness; there
  is **no `char` variant** (`char8`=`Int{8}`, `char`=`Int{32}`).
  **`InstData.ty: IrType`** carries each instruction's result type (inst.rs:293-296)
  — the hook for the T6 fold-width fix. `Const::{Int(i128,IrType),Float,Bool,Str}`.
- **JIT** — `OrcJit::from_ir` (jit.rs:113) installs **only**
  `LLVMOrcCreateDynamicLibrarySearchGeneratorForProcess` (jit.rs:152-159), which
  resolves **exported process symbols** (CRT/kernel32). A Rust `#[no_mangle] extern
  "C"` symbol in a statically-linked rlib is **not** a process export and will
  **not** be found — `__newbf_ct_emit` requires an explicit absolute-symbol
  definition (§5.4). `OrcJit` exposes `from_ir`/`lookup` only today.
- **type table** — `register_type_struct` **skips duplicate simple names**
  (`if !t.by_name.contains_key(&name)`, lower.rs:2217); `struct_kind` returns
  **`None` for `extension`** ("enum / extension — not yet", lower.rs:52);
  `fill_fields_at` **overwrites**: `t.defs[id.0].fields = fields;` (lower.rs:2733)
  and `t.field_elems[id.0] = elems` (lower.rs:2734). `register_type_struct` /
  `fill_members_at` also (re)build `ctors`/`methods`/`virtuals`/`vimpls`/`field_inits`
  per decl.
- **analyze duplicate check** — `check_duplicate_types` **exempts
  `TypeKindD::Extension`** (resolve.rs:113); two *definitional* same-name+arity types
  in one container produce a `duplicate type definition` diagnostic (resolve.rs:106-135).
- **parser** — `TypeKind::Extension` parses (ast.rs:783); `[OnCompile(.TypeInit),
  Comptime]` already appears in feature-suite/src/Comptime.bf:90 (the attribute form
  parses). `typeof` parses but **is not lowered** anywhere.
- **runtime** — `newbf-runtime` minimal: SEH crash-dump handler live
  (`install_crash_handler`, lib.rs:39); stomp/leak guard is a stub. A comptime fault
  **propagates / crashes the compiler** (eval.rs:16-20) — it does **not** return a
  `Result::Err`.

The load-bearing corrections this design makes over a naive "wrap in `class Owner
{…}` and re-lower" approach: that approach (a) trips `check_duplicate_types`
(definitional dup), and (b) even if exempted, `fill_fields_at` clobbers the
owner's `x`/`y` with `[$header]` only — the last-filled decl wins. **The fix is to
emit into an `extension` (the duplicate-exempt form) and make extensions
*append-not-replace*.** See §2.1.

### 2.1 The partial-type substrate problem (the real first slice)

The only way to add a member to an existing type via reparsed source is to reopen
it. NewBF parses `extension Vec2 { … }` (ast.rs:783) and `analyze` already exempts
extensions from duplicate-type errors (resolve.rs:113) — but `extension` is
**unimplemented** in the layout/member path (`struct_kind` → `None`, lower.rs:52),
so its members never reach `defs`/`methods`/`virtuals`. Implementing
*append-not-replace* extension member composition is a **prerequisite, non-comptime,
independently testable** task (T0), proven by a hand-written `extension` run-corpus
program before any JIT/emit machinery exists. This isolates the hardest, riskiest
correctness work behind a green gate that does not depend on comptime at all.

## 3. Approach

The design rests on one observation: **`lower_program` is already a pure
`source → Module` function, and emission in Beef is `(target, source-text)`.** The
lowest-risk re-entrancy seam is **not** a callback threaded into single-pass
lowering — it is an **outer fixpoint loop** (in `newbf-comptime`, driver-called)
that (1) analyzes + lowers, (2) JIT-runs emit generators in a sandboxed module,
(3) collects emitted source as `extension Owner { … }` units, (4) re-analyzes +
re-lowers with the generated units appended, until no new emissions appear
(fixpoint) or a guard trips.

This sidesteps the circular-dependency problem: **sema never calls comptime**.
Sema's only new job is to *recognize and record* emit generators as data on the
`Module` (exactly as it records `module.comptime`). `newbf-comptime` depends on
`newbf-sema` (allowed) + `newbf-llvm` (already does); the driver (which legally
depends on all three) runs the loop.

### 3.1 The pipeline, restated

```
        ┌──── fixpoint loop (newbf_comptime::run_emission, driver/harness-called) ────┐
 files ─┤  loop:                                                                       │
        │    parsed = base_units + generated_units (owned Vec<(String, CompUnit)>)     │
        │    program = analyze(&parsed)        ── diagnostics? → abort with them ──────┤
        │    module  = lower_program(&parsed, &program)                                │
        │    if module.emit_jobs empty → break (no-op fast path)                       │
        │    for each emit job: JIT a nullary wrapper in a sandbox clone; drain SINK   │
        │    for each (owner_qual_name, text): normalize; dedup; append `extension`    │
        │    grew? → loop ;  else → break (fixpoint)                                    │
        │  STRIP emitter + comptime fns reachable to __newbf_ct_emit → final module    │
        └─────────────────────────────────────────────────────────────────────────────┘
                                            │
                       fold_comptime(final module)  (value folding, §3.5)
                                            │
                                  codegen: JIT (run/app) or AOT (emit_object+link)
```

`fold_comptime` runs **once at the end**, after emission fixpoint. Emission changes
the program's *shape* (new members); folding collapses *values*.

### 3.2 How an emitter communicates emitted text out of the JIT

NewBF has no VM; the emitter is native JIT'd code. It calls an **`extern` runtime
shim** that `newbf-comptime` defines as a host symbol and registers as a JIT
**absolute symbol** (§5.4). The v1 corlib surface uses **primitives only** — no
`Type`, no `StringView`, no `typeof` (none of which lower today):

```beef
static class Compiler {
    // The host shim. Resolved by an absolute-symbol definition in the comptime JIT.
    [LinkName("__newbf_ct_emit")]
    static extern void Comptime_Emit(int32 ownerTypeId, char8* textPtr, int32 textLen);

    // The user-facing emit op. The owner-id arg is INJECTED by sema (§3.3) — the
    // user calls EmitTypeBody(text); sema lowers it to Comptime_Emit(<id>, ptr, len).
    [Comptime]
    public static extern void EmitTypeBody(String text);
}
```

`__newbf_ct_emit` is a Rust `extern "C"` fn in `newbf-comptime` that appends
`(ownerTypeId, String)` to a thread-local sink the loop drains after each JIT call.
`String` already exposes pointer/length in corlib (used as `(char8*, int32)` at the
shim boundary). This is the FFI-helper equivalent of Beef's reflection opcodes.

### 3.3 What triggers emission, and how the emitter is actually called

**Trigger.** A `[Comptime, EmitGenerator]` static method (the bare `[EmitGenerator]`
marker is guaranteed to parse with the existing attribute grammar; `[OnCompile(.TypeInit)]`
also parses — feature-suite:90 — and may be adopted if its enum-arg retrieves
cleanly, decided in T2's verify step). Sema's `comptime_emitter_of(attrs, src)`
recognizes it and records an **emit job**.

**The owner id is injected by sema, not read from reflection.** When sema lowers an
emit generator, it knows the owner's `StructId` and qualified name. It records
`EmitJob { owner_qual_name, symbol }`. The user writes `EmitTypeBody(text)`; the
loop never needs `typeof`/`Type`.

**Invocation (the missing recipe).** An emit generator is **not** nullary-callable
as-is, and even when nullary it returns `void` and takes no value. The loop
synthesizes a **nullary wrapper** in the sandbox clone — the `$ct_eval` pattern
(fold.rs:122-130) — that calls the generator with no user args:

```text
$ct_emit_run_<symbol>() { <symbol>(); }      // void wrapper; lookup + call
```

The wrapper is `void`-returning; the loop `transmute`s to `extern "C" fn()` and
calls it. The generator's body, during lowering, was rewritten so its
`Compiler.EmitTypeBody(text)` calls became `__newbf_ct_emit(<owner_id_literal>,
text.Ptr, text.Len)` — the literal is the **per-round** owner id (see §3.4
determinism). The required generator signature is fixed: **`static void Name()`**
(no params in v1); any other shape is rejected with a diagnostic.

> If a future revision wants `void Name(Type self)`, the wrapper passes the owner
> id literal as the first arg (typed `int32`) — the recipe is identical, just with
> one literal operand. v1 keeps it nullary to avoid pinning a `Type` ABI.

### 3.4 The worklist, determinism & termination

The loop owns `generated: Vec<(String /*src*/, CompUnit)>` (so the parsed units
outlive the per-round `SourceFile` borrows — §5.3) and `emitted: HashSet<EmitKey>`
where `EmitKey = (owner_qual_name, fnv1a(normalize(text)))`. Each iteration:

1. Build `parsed = base + generated`; `program = analyze(&parsed)`. **If
   `program.diagnostics` is non-empty, abort** and surface them (the generated
   source ran through the full front-end; a malformed emission is an ordinary
   parse/sema diagnostic, not a silent miscompile).
2. `module = lower_program(&parsed, &program)`. If `module.emit_jobs` empty → done.
3. For each emit job, capture a **per-round `name → StructId` map** at the same
   time the generator is JIT'd. JIT the `$ct_emit_run` wrapper in a sandbox clone
   (the generator + the absolute shim present); drain `EMIT_SINK`.
4. For each `(owner_id, text)` emitted: resolve `owner_id` back to a qualified name
   via the **per-round** map (StructId assignment shifts between rounds — see
   "Determinism"). Compute `EmitKey` over `normalize(text)` (trim + collapse
   interior whitespace runs, strip line comments) so cosmetic differences dedup.
   If in `emitted`, **skip** (idempotent cycle/dup guard). Else insert, parse
   `extension <qual_name> { <text> }` into an owned `(String, CompUnit)`, append to
   `generated`.
5. If `generated` grew, loop. Else, fixpoint.

**Determinism.** StructIds are assigned by `StructTable::build` order
(lower.rs:2218 — `StructId(t.defs.len())`); appending generated units shifts ids
across rounds. Therefore: **emits are keyed end-to-end by qualified name, never by
a StructId held across rounds.** The owner id passed through the JIT shim is only
valid *within* the round it was produced (resolved via that round's `name→id` map).
When multiple generators run in one round, process them in a **fixed order (owner
qualified name, then symbol)** so generated-source text order — and thus the next
round's StructId assignment — is reproducible.

**Termination — triple-guarded:** (a) the `emitted` set over *normalized* text
makes identical emissions idempotent (A emits B, B emits A-identical → no-op);
(b) a hard **iteration cap** (`MAX_EMIT_ROUNDS`, default 16, configurable) trips a
diagnostic; (c) a hard **total-emitted-bytes cap** (default 1 MiB) bounds growth.
The dangerous case is A→B→A' with *different* text each round; (b)/(c) catch it.
The cap is documented as **anti-cycle backstop only**, not a correctness guarantee;
a legitimate >16-round generator must raise the cap explicitly.

> **Non-termination caveat:** these caps bound the *number of rounds*, between which
> the loop regains control. An emitter with an **internal infinite loop** (never
> returns) hangs — there is no mitigation in v1 (bounded execution is deferred).
> v1 emitters must be trusted and terminating. The cap tests therefore use a
> generator that **returns normally but emits divergent text each round**, so the
> outer round/byte caps are the thing under test.

### 3.5 Const-eval breadth (the eval.rs side)

Generalize `eval_const_i64` to
`eval_const(module, name, ret: IrType) -> Result<ConstVal, String>`, reading the
JIT result at the right width and **sign-correctly**:

- `Int { bits<=64, signed }` / `Bool` → `transmute` to `extern "C" fn() -> i64`,
  **mask to `bits`, then sign-extend iff `signed`, else zero-extend.** The Win64
  ABI returns sub-64-bit integers in RAX with **upper bits undefined**, so mask
  first, then extend per signedness. A folded `i32 = -1` must become `0xFFFF_FFFF`
  masked → sign-extended to `i64 = -1`, not `0x0000_0000_FFFF_FFFF`.
- `Float { .. }` → `Err("comptime: float return types are not yet supported (ORC
  float-constant gap)")` — the bounded scope, a diagnostic never a crash.
- `Ptr`/`Ref`/`Struct` returns → `Err` in v1 (no heap-value marshalling).

`eval_const_i64` stays as a thin wrapper (`eval_const(m, n, I64).map(|c| c as i64)`).

**Fold width fix (T6).** The fold rewrite currently hardcodes I64 (fold.rs:96-100);
the LLVM `Bin` lowering derives result type from operands. Folding an `i32`-returning
call whose SSA uses expect `i32` must rewrite to an `i32` literal: use the folded
call instruction's **own** `InstData.ty` (inst.rs:295) for the literal type (and the
identity `+0` of the same type), so the result width matches every use and the
module verifies.

**Non-constant-but-comptime-known args / inner-fold-first.** A folded comptime
result is already a `Const::Int` by the time an outer call is examined, so
`Outer(Inner(3))` folds bottom-up if the collect/apply loop is **re-run until no new
site folds** (bounded by instruction count). No new mechanism — just iterate
`fold_comptime`'s collect phase to a fixpoint.

### Alternatives considered & rejected

- **(A) Trait-object callback threaded into single-pass `lower_program`.** Rejected
  for v1: lowering is a single top-down pass; injecting comptime mid-pass exposes a
  half-built `StructTable` (SSA-dominance + partial-graph hazards the invariants
  warn about) and forces the `dyn` boundary immediately. The outer loop gets
  feedback with **zero** changes to lowering internals and a fully-built type graph
  at every emitter invocation. Add the deep callback later only if a feature needs
  mid-resolution emission (none in v1 do).
- **(B) Direct IR `StructDef`/`Function` injection (skip reparse).** Rejected: the
  emitter produces Beef *source* (upstream model); IR injection bypasses **all** of
  sema's checks (a generated method referencing a missing field would make
  verifier-clean-but-wrong IR → run-corpus miscompile) and can't reuse overload
  resolution / inheritance. Source-emit-and-reparse runs generated code through the
  identical front-end as hand-written code — the safety property we want.
- **(C) Wrap emitted text in `class Owner { … }` and re-lower.** **Rejected as
  incorrect** (this was the original draft's plan): a second definitional `class
  Vec2 {}` trips `check_duplicate_types` (resolve.rs:106-135), and even exempted,
  `fill_fields_at` overwrites `t.defs[id].fields` (lower.rs:2733), clobbering the
  owner's real fields with `[$header]` only — the §1 example reads garbage. The
  correct form is `extension Owner { … }` with append-not-replace fill (T0).
- **(D) Make the eval JIT resolve `__real@` float constants.** Rejected for v1: a
  separate, deep RTDyld/JITLink investigation (MEMORY) orthogonal to emission.
  Floats are scoped out with a diagnostic, revisited independently.
- **(E) Pass the owner as a cross-round `StructId` literal.** Rejected: StructIds
  shift when generated units change registration order (lower.rs:2218). Keyed by
  qualified name instead; the JIT-boundary id is per-round only.

## 4. Representation / IR / runtime / ABI changes

### 4.1 IR (`newbf-ir`)

Add one field to `Module`, **named distinctly** to avoid colliding with the existing
`local_fn_emits`/`lambda_emits` plumbing in lower.rs (3461-3477):

```rust
// module.rs — alongside `pub comptime: Vec<String>`
/// A comptime member-emitting generator recorded by sema.
pub struct EmitJob {
    pub owner_qual_name: String, // e.g. "Demo.Vec2" — the cross-round routing key
    pub symbol: String,          // the generator's mangled symbol (nullary `void`)
}
pub emit_jobs: Vec<EmitJob>,     // default empty — empty ⇒ loop is a no-op
```

Owned data, no lifetimes, no `StructId` held across rounds — invariants preserved.
`IrType` untouched (emitted types are concrete after reparse). No `$header`/layout
change — emission produces ordinary members.

### 4.2 The emit sink (host-side, in `newbf-comptime` — compiler-internal)

```rust
thread_local! {
    static EMIT_SINK: RefCell<Vec<(i32 /*ownerTypeId*/, String)>> = RefCell::new(Vec::new());
}
#[unsafe(no_mangle)]
pub extern "C" fn __newbf_ct_emit(owner_type_id: i32, ptr: *const u8, len: i32) {
    // Panic-safe: copy out first; never hold the borrow across the FFI return.
    let s = unsafe { std::slice::from_raw_parts(ptr, len.max(0) as usize) };
    let text = String::from_utf8_lossy(s).into_owned();
    EMIT_SINK.with(|b| b.borrow_mut().push((owner_type_id, text)));
}
```

Thread-local because `OrcJit` runs the emitter on the calling thread, in-process.
The loop snapshots+clears the sink around each JIT call. This is the **only** new
host symbol beyond CRT/kernel32; it is registered as a JIT absolute symbol (§5.4).

### 4.3 ABI / mangling

No mangling changes. Emitted members lower through the identical path as source
members (`lower_method`, owner-mangling) — symbols indistinguishable from
hand-written. `__newbf_ct_emit` uses plain C ABI. `$Func`, itables, the `$header`
vtable are untouched: generated methods participate exactly as if hand-written
(because they *are* reparsed source).

### 4.4 Alloc-path interaction

Emitters JIT-run via `OrcJit::from_ir` → process-symbol generator → CRT
`malloc`/`free`, identical to app code. A realistic emitter builds its text with
corlib `String` concatenation, so it hits the heap. When the stomp/leak guard lands
in `newbf-runtime` it hooks this path for both worlds. **Caveat:** a stomp-guard
`abort()` (vs an access-violation) in an emitter aborts the whole compiler with no
SEH recovery — documented as a v1 boundary, mitigated only by trusted emitters.

## 5. Sema / parser / comptime / runtime / codegen changes

### 5.1 Parser

**No grammar changes.** `[EmitGenerator]` parses with the existing attribute
grammar; `extension` already parses (ast.rs:783). T2 verifies the marker attribute
both parses **and** is retrievable on the method's attribute list (a parser-corpus
spot-check), defaulting to the bare `[EmitGenerator]` if `[OnCompile(.TypeInit)]`'s
enum-arg retrieval needs work. Parser corpus stays 154/154.

### 5.2 Sema (`newbf-sema`)

- **T0 (substrate):** implement `extension` in the type table. `struct_kind` must
  recognize `TypeKind::Extension` by resolving the reopened type's id via `by_name`
  (not allocating a new id), and member-fill for an extension must **append**
  ctors/methods/virtuals and **add (never replace)** fields/`field_elems`/`field_inits`
  to the existing id — i.e. a partial-`fill_members_at` that does not run
  `t.defs[id].fields = fields` but extends. Preserve original field default
  initializers (the merge must not clobber `field_inits`). Prove with a hand-written
  `extension` run-corpus program (no comptime): an extension method reading a
  base-decl field. Make "verify corpus stays 154/154 with extension support" a gate.
- Add `comptime_emitter_of(attrs, src) -> bool` next to `has_comptime_attr`
  (lower.rs:9278).
- During method lowering (lower.rs:4168), when a method is an emit generator: push
  `EmitJob { owner_qual_name, symbol: full_name.clone() }` into `m.emit_jobs`, and
  still push to `m.comptime` (so the generator drops from the final program). Rewrite
  the generator body's `Compiler.EmitTypeBody(text)` calls into
  `__newbf_ct_emit(<owner_id_literal>, text.Ptr, text.Len)` during lowering, where
  the literal is the owner's current `StructId`.
- **No callback, no JIT dependency, no re-entrancy inside sema.** Sema stays a pure
  `source → Module` producer; the new behavior is *recording data* + a local body
  rewrite, the same shape as the existing `module.comptime.push`.
- The generic-comptime guard (lower.rs:1536) is untouched — generic emit generators
  stay rejected in v1.

### 5.3 Comptime (`newbf-comptime`)

New module `emit.rs`. **Ownership model (the trickiest plumbing):** `run_emission`
owns `generated: Vec<(String, CompUnit)>` for the whole loop; each round it rebuilds
the borrow set `Vec<SourceFile>` (base + generated) and calls the **real**
`newbf_sema::analyze` + `newbf_sema::lower_program` inline (the dependency direction
`comptime → sema` is legal — sema's Cargo.toml has no comptime dep; newbf-llvm is a
dev-dep). **No `fn`-pointer/`GenUnit` abstraction** — it cannot type-check against
the borrowed `SourceFile<'_>` API (`SourceFile` borrows owned units), and is
unnecessary since comptime may call sema directly.

```rust
pub struct EmitOutcome { pub rounds: u32, pub diagnostics: Vec<String> }

/// Drive emission to a fixpoint and return the final, codegen-ready module.
/// Re-runs analyze + lower_program each round; aborts (Err / diagnostics) on a
/// generated-code analyze error; strips emitter + shim before returning so the
/// final module JIT-links / AOT-links with no unresolved symbols.
pub fn run_emission(base: &[SourceFile<'_>]) -> Result<(IrModule, EmitOutcome), String>;
```

The loop: analyze+lower → if `emit_jobs` empty, **strip nothing extra and return**
(no-op fast path) → else JIT each generator's `$ct_emit_run` wrapper in a sandbox
clone, drain `EMIT_SINK`, dedup over normalized text, wrap in `extension Owner {…}`,
re-analyze+re-lower; cap at `MAX_EMIT_ROUNDS`/byte-cap. **Before returning, strip**
every function in `module.comptime`/`module.emit_jobs` plus any function
transitively referencing `__newbf_ct_emit`, and the `__newbf_ct_emit` extern decl
itself (reuse fold.rs's `reachable_from_ordinary` sweep). Register `__newbf_ct_emit`
as a JIT absolute symbol before lookup (§5.4). `fold.rs` runs **after**
`run_emission` on the final module.

### 5.4 Codegen / JIT / AOT

- **`OrcJit::add_absolute_symbol(name, addr)`** — an explicit, separately
  unit-tested addition (an ORC absolute-symbols `MaterializationUnit` / a
  definition generator binding `__newbf_ct_emit`'s address). Budget for llvm-sys
  binding friction. It must be installed on the **same instance** the emitter runs
  in, and **before** the process-search generator (so the explicit definition wins,
  avoiding a duplicate-definition error if the shim ever leaks as a process export).
  This affects **only the comptime sandbox JIT**; app JIT/AOT are unchanged.
- **Eager-link correctness (blocker):** RTDyld eagerly links the **whole** module
  on first lookup. If an emitter function (calling the extern `__newbf_ct_emit`)
  survives into the final module, `lookup("Program.Main")` fails with
  `Symbols not found: [__newbf_ct_emit]` — because the **app/run JIT** does not
  register the shim (only the comptime sandbox does). Therefore `run_emission`
  **must strip** all emitter/shim references before returning (§5.3), and an
  acceptance check asserts the final module JIT-links cleanly (T4) — this is **not**
  delegated to `fold_comptime` (the run-corpus harness never calls it).
- **JIT vs AOT for the final program:** emission resolves **entirely at compile
  time, before codegen, in both modes**. By the time AOT `emit_object`/`link` runs
  or the app JIT runs, generated members are concrete IR and `__newbf_ct_emit` is
  gone. **Both paths get generated code for free**; the shim never reaches the
  shipped binary. **Add an AOT acceptance check** for at least one emission program
  (the same strip property is what makes AOT link).
- **Driver wiring (main.rs ×3):** replace `lower_program` + `fold_comptime` with
  `run_emission` (which internally analyzes/lowers/strips) followed by
  `fold_comptime`. Merge `EmitOutcome.diagnostics` into the driver's diagnostic
  stream (printed like parse/sema diagnostics).
- **run-corpus harness:** route `run` through `run_emission` (build the final
  module via it, then `fold_comptime`, then JIT). Programs without generators hit
  the no-op fast path (zero overhead; all existing programs stay green). The harness
  asserts emission diagnostics as failures for positive programs.
- **Float gap:** `eval_const` returns a typed `Err` for float; `fold_comptime` never
  attempts float-returning functions — preserved, now with an explicit diagnostic if
  one is forced.

## 6. Interactions

- **`$Func` / function values:** an emitted method can be a method-ref / lambda
  target — reparsed source, so `$Func={code,target}` applies unchanged.
- **Itables / interfaces:** `EmitAddInterface` is **out of v1**. An emitted *method*
  that implements a source-declared interface resolves through the normal itable
  path because the whole graph is rebuilt each round. Adding an interface via
  emission is staged (touches `iface_slot_base` recompute).
- **Virtual emitted methods:** because the type graph (and vtables, lower.rs:3482) is
  recomputed from the full set each round, an emitted `virtual` method joins the
  vtable on rebuild — **but only if T0's extension merge feeds it into
  `virtuals`/`vimpls`** before vtable globals emit. Add an explicit test (an emitted
  virtual override).
- **Field defaults:** T0's append-merge must preserve the owner's `field_inits` so
  constructed objects keep their defaults (the extension adds, never resets).
- **Two-phase args:** emission produces *declarations*, resolved by the normal
  two-phase machinery on the next round — no interaction with arg-resolution
  internals.
- **`module.comptime` vs `module.emit_jobs`:** a method that is both a folded
  comptime fn and an emitter is pushed to both; the strip sweep (§5.3) and
  `fold_comptime`'s `reachable_from_ordinary` must agree — both treat
  `module.comptime` members + anything reaching `__newbf_ct_emit` as droppable, so
  there is no function one keeps that the other needs.
- **The other three wave features:** orthogonal; the only shared surface is the
  comptime JIT instance and the run-corpus harness, both additive.
- **Diagnostics model:** `lower_program` has no sink (invariant). Emission/analyze
  diagnostics are returned in `EmitOutcome.diagnostics` and merged in the driver —
  keeping the no-sink-in-lowering invariant intact.

## 7. Risks & mitigations

- **Partial-type field-wipe (was a correctness blocker):** solved by emitting into
  `extension` (duplicate-exempt) **and** T0's append-not-replace merge. Regression
  test: an emitted member reads a pre-existing field (§1, expect 42).
- **Eager-link unresolved `__newbf_ct_emit` (was a blocker):** `run_emission` strips
  emitter/shim before returning; T4 asserts the final module JIT-links and AOT-links
  with no unresolved symbols. A specific test: a program whose emitted member is
  **never called** (dead) — the emitter still ran; the module must still link.
- **`__newbf_ct_emit` not a process export (was a blocker):** explicit
  `add_absolute_symbol`, unit-tested (T2), installed before the process generator.
- **StructId↔TypeId instability across rounds (was a blocker):** keyed by qualified
  name end-to-end; JIT-boundary id resolved via the per-round `name→id` map.
- **Fold rewrite hardcodes I64 (T6 blocker):** use the call's `InstData.ty`; assert
  the i32 program verifies.
- **LLVM dominance:** no new in-block IR emission — emitters are whole, already-
  verified functions; generated members are reparsed + lowered by the existing pass.
  Avoided structurally (a reason to reject alternative (A)).
- **Re-entrancy / circular dep:** direction `comptime → sema`; sema only records
  data. No `dyn` in v1.
- **Emitter faults are NOT recoverable in v1 (corrected claim):** the SEH crash
  handler writes a dump and the process **still dies** — it does not return a
  `Result::Err`. A segfault/abort across the JIT/FFI frame is not catchable into a
  `Result` without SEH `__try`/out-of-process isolation (deferred bounded-execution
  work). v1 documents: emitters must be trusted/terminating; an emitter fault
  crashes the compiler (matching current eval.rs:16-20). **No test asserts graceful
  recovery from a faulting emitter.**
- **Float in an emitter:** an emitter doing float math fails to JIT (the `__real@`
  gap), surfaced as a typed diagnostic, not a miscompile.
- **Fixpoint non-termination:** triple-guarded (idempotent normalized-text set +
  round cap + byte cap), diagnostic on cap-trip. Internal-infinite-loop emitters
  hang (documented v1 boundary).
- **Cost:** each emission program pays N full front-end passes (re-parse prelude +
  re-analyze + re-lower per round). Acceptable at v1 scale (expected round count 1-3
  for real generators); the no-op fast path protects all generator-free programs.
- **Metadata/binary bloat:** emitter fns + shim stripped before codegen; never in
  the binary. Golden `dump-ir` asserts emitter symbol absent **and** `__newbf_ct_emit`
  extern absent.

## 8. Testing strategy

**Gates green at every task boundary:** parser corpus (154/154), verify corpus
(154/154 LLVM-clean — note it lowers each file **standalone** via `lower_program`,
corpus.rs, and never calls `run_emission`; confirm a recorded `emit_jobs` is inert
there and the new corlib `Compiler` class + emitter predicate keep Comptime.bf
verify-clean), run corpus (JIT, full-i32, authoritative).

**Unit tests (`newbf-comptime`):**
- `eval.rs`: `eval_const` for `i8/i16/i32/i64/bool` reads the right width; **negative
  values** (`i32 = -1`, `i8 = -7`) prove sign-extension; an unsigned near-max (`u8 =
  250`) proves zero-extension; float return yields the typed `Err`.
- `jit`: `add_absolute_symbol` + a tiny IR fn calling `__newbf_ct_emit` populates
  `EMIT_SINK`; assert **no duplicate-definition error** with the process generator
  also present.
- `emit.rs`: a synthetic module with one `EmitJob` whose generator pushes a known
  string produces that text; idempotent re-emission stops at fixpoint in 2 rounds;
  a generator that **returns but emits divergent text each round** trips the
  iteration cap with the cycle diagnostic (no crash, no hang).

**Run-corpus programs (behavioral proof; each `Program.Main → int32` with `// expect:`):**
- `extension_member_reads_field.bf` (**T0, no comptime**) — a hand-written
  `extension Vec2 { int SumXY() => x+y; }` reading base fields. **expect: 42**.
  Proves the substrate before emission exists.
- `comptime_emit_member.bf` — the §1 `Vec2.SumXY` example. **expect: 42**.
  Emit→reparse→resolve→call, reading a pre-existing field.
- `comptime_emit_then_call_twice.bf` — emit a method, call from two sites with
  different args. Proves the generated member is a reusable symbol.
- `comptime_emit_dead_member.bf` — emit a member that is **never called**; the
  module must still JIT-link (eager-link regression). **expect:** a value from
  non-emitted code.
- `comptime_emit_idempotent.bf` — two generators emitting the **same** (normalized)
  member text; dedup must not double-define. Proves the `emitted` guard.
- `comptime_emit_virtual.bf` — an emitted `virtual` override picked up by the vtable
  rebuild. Proves the merge feeds `virtuals`/`vimpls`.
- `comptime_eval_i32_arg.bf` — `[Comptime] int32 F(int32 x) => x*x;` folded at
  `F(7)`. **expect: 49**. Proves widened-int eval + the fold-width fix (verify-clean).
- `comptime_nested_fold.bf` — `Outer(Inner(3))` both comptime. Proves inner-fold-first.

**AOT:** one emission program compiled AOT and linked (proves the strip makes AOT
link; emission is a pure front-end transform).

**Golden `dump-ir`:** on `comptime_emit_member.bf`, assert the generated `SumXY`
symbol **present**, the generator symbol **absent**, and `__newbf_ct_emit`
**absent** — proving both halves.

**Negatives (separate `newbf-comptime` integration tests, not run-corpus):** the
divergent-emitter cap diagnostic; a generated member referencing a missing field
produces an analyze diagnostic that aborts the loop (not a miscompile). The
run-corpus holds positives only.

## 9. Task breakdown (ordered, agent-assignable; each lands behind the green gates)

T1–T2 + T0 are **behavior-preserving/additive** (no existing-program output
changes); T3 onward are **behavior-changing**.

**T0 — Extension member composition in StructTable (the true minimal slice;
non-comptime).** Scope: `newbf-sema/src/lower.rs` — recognize `TypeKind::Extension`
in `struct_kind` (resolve id via `by_name`), implement append-not-replace
member-fill (add fields/`field_elems`/`field_inits`, append ctors/methods/virtuals/
vimpls; reject duplicate-signature members with a diagnostic), preserve original
field defaults. Deps: none. Accept: hand-written `extension_member_reads_field.bf`
(**expect: 42**) passes in run-corpus; verify corpus 154/154 with extension support;
parser corpus unchanged.

**T1 — Widen the eval core (behavior-preserving).** Scope: `eval.rs` — add
`eval_const(module, name, ret)` reading `i8/i16/i32/i64/bool` at width with
sign/zero-extension per `IrType::Int{signed}`; float/ptr/struct → typed `Err`; keep
`eval_const_i64` as a wrapper. Deps: none. Accept: existing eval tests pass; new
width tests incl. negative + unsigned cases + float-`Err`; all corpora unchanged.

**T2 — `run_emission` skeleton + no-op fast path + JIT absolute symbol
(behavior-preserving).** Scope: new `emit.rs` + `lib.rs` re-export; `Module.emit_jobs`
(`module.rs`, default empty); `OrcJit::add_absolute_symbol` + `__newbf_ct_emit` shim
+ `EMIT_SINK`; verify the `[EmitGenerator]` attribute parses **and** is retrievable.
The loop returns the module verbatim when `emit_jobs` empty. Wire `run_emission` into
the driver (main.rs ×3) and the run-corpus harness. Deps: T1. Accept: all run-corpus
+ both static corpora **unchanged** (fast path no-op); unit test that
`add_absolute_symbol` + a tiny IR fn calling `__newbf_ct_emit` populates `EMIT_SINK`
with no duplicate-definition error.

**T3 — Sema records emit generators + body rewrite (behavior-changing, minimal).**
Scope: `lower.rs` — `comptime_emitter_of`; push `EmitJob{owner_qual_name, symbol}` at
lower.rs:4168 (also keep `module.comptime`); rewrite `Compiler.EmitTypeBody(text)`
in the generator body to `__newbf_ct_emit(<owner_id_literal>, text.Ptr, text.Len)`.
Deps: T2. Accept: a sema unit test shows `emit_jobs` populated and `module.comptime`
still contains the generator; corpora unchanged (no corpus program uses the marker
yet); verify corpus stays 154/154 with the new corlib `Compiler` class present.

**T4 — First real slice: EmitTypeBody → extension → reparse → resolve → call
(behavior-changing).** Scope: `emit.rs` fixpoint loop body (per-round name→id map;
JIT `$ct_emit_run` wrapper in sandbox clone; drain sink; resolve id→qual-name;
normalize+dedup; wrap in `extension Owner {…}`; re-analyze+re-lower; **strip
emitter/shim before return**); corlib `Compiler.EmitTypeBody` + `__newbf_ct_emit`
extern + minimal `Compiler` static class (primitives-only signature — no
`Type`/`StringView`/`typeof`). Deps: T0, T3. Accept: `comptime_emit_member.bf`
(**expect: 42**, reads a pre-existing field), `comptime_emit_then_call_twice.bf`,
`comptime_emit_dead_member.bf` pass in run-corpus; **final module JIT-links and
AOT-links with no unresolved symbols** (explicit assertion); `dump-ir` golden shows
generated symbol present, generator + `__newbf_ct_emit` absent; all prior gates
green.

**T5 — Fixpoint guards + diagnostics (behavior-changing, hardening).** Scope:
`emit.rs` — `emitted` dedup over normalized text; `MAX_EMIT_ROUNDS` (configurable) +
byte cap; per-round determinism ordering; `EmitOutcome.diagnostics`; abort on
generated-code analyze diagnostics; driver merges emission diagnostics. Deps: T4.
Accept: `comptime_emit_idempotent.bf`, `comptime_emit_virtual.bf` pass; integration
tests that a returning-but-divergent emitter trips the cap with a diagnostic and that
a missing-field emission aborts with an analyze diagnostic (no crash/hang).

**T6 — Const-eval breadth: widened-int args + fold-width fix + inner-fold-first
(behavior-changing).** Scope: `fold.rs` — accept foldable returns for
`i8/i16/i32/i64` (via T1's `eval_const`); **rewrite using the call's `InstData.ty`,
not hardcoded I64**; iterate the collect/apply loop to a fixpoint for nested folds.
Deps: T1, T4. Accept: `comptime_eval_i32_arg.bf` (**expect: 49**, verify-clean) and
`comptime_nested_fold.bf` pass; `keeps_comptime_called_with_runtime_arg` still passes.

**T7 — Docs + journal (behavior-preserving).** Scope: expand `docs/COMPTIME.md` with
the loop, the emit FFI shim, the float/generic/mixin/Type v1 boundaries; add journal
§115. Deps: T0–T6. Accept: docs build; journal pairs with the feature commits.

**Staged beyond v1:** `EmitAddInterface` (itable recompute), `EmitMixin` (statement
injection + hygiene + diagnostic-sink dep), generic emit generators (lift the
lower.rs:1536 guard), float const-eval (`__real@` JIT gap), a reflection FFI table
(`GetReflectType`/`GetString`, `Type`/`typeof`/`StringView` lowering), and bounded
execution (step/time/mem caps + SEH/out-of-process emitter-fault-to-diagnostic).

## 10. Open questions / decisions deferred

1. **Attribute spelling for the generator marker** — default to bare
   `[EmitGenerator]` (guaranteed to parse); adopt `[OnCompile(.TypeInit)]`
   (Beef-faithful, parses per feature-suite:90) only if its `.TypeInit` enum-arg
   retrieves cleanly. Decided in T2's verify step. Parser corpus stays 154/154.
2. **`owner_qual_name` resolution corner cases** — nested types / namespaced owners:
   confirm `register_type_struct`'s `by_name` keying (simple name, lower.rs:2217) is
   sufficient, or extend the routing key to a fully-qualified path if simple names
   collide across namespaces. Decide in T0/T4.
3. **Where final-program emission diagnostics live** — `EmitOutcome.diagnostics`
   merged in the driver (chosen); run-corpus asserts none for positives; negatives
   are separate `newbf-comptime` integration tests (chosen).
4. **`run_emission` behind a `dyn` trait?** Only if a second consumer (the IDE
   incremental pipeline) needs it; v1 ships the concrete function calling sema
   directly. Deferred until that consumer exists.
5. **Float const-eval** — revisit the `__real@`/RTDyld gap independently (JITLink or a
   synthetic float-constant-materialization pass); off this feature's critical path.
6. **Text normalization aggressiveness** — v1 normalizes whitespace + line comments
   for the dedup key; whether to canonicalize further (parse + re-print the member
   for signature-level dedup) is deferred unless cosmetic-difference double-defines
   show up in practice (T0's duplicate-signature diagnostic is the backstop).
