# NewBF — Cross-Feature Sprint Plan (Wave 2: safety · comptime-breadth · reflection · mixins)

*Drafted 2026-06-07. Sequences the agent-assignable tasks from the four
Wave-2 design docs in [`docs/design/`](.) —
[`memory-safety.md`](memory-safety.md),
[`comptime-breadth.md`](comptime-breadth.md),
[`reflection.md`](reflection.md),
[`mixins.md`](mixins.md) — into one schedule. Companion to the prior
wave's [`SPRINT-PLAN.md`](SPRINT-PLAN.md), the 12-phase
[`PLAN.md`](../../../PLAN.md), and the original
[`SPRINTS.md`](../../../SPRINTS.md). This wave lives inside PLAN.md
phases 4 (manual-memory + guard), 7 (comptime breadth), 8 (reflection),
and 9 (mixins / Try!). Unlike Wave 1 — which was four dispatch
refinements on one pipeline — this wave touches the **runtime crate, the
JIT/AOT symbol-resolution seam, and the object `$header` ABI**, so it is
materially riskier and its critical path runs through one shared seam.*

## Preamble — cadence and invariants

Cadence is unchanged: **one developer, one agent at a time,
review/test/commit per task.** A "sprint" here is a *review batch* — a
set of tasks reviewed and merged as a group because they share a gate or
co-land atomically. "PARALLELIZABLE" means *no dependency forces an
order* (interleave them however the review queue prefers); "SERIAL"
means they must land in the listed order.

**The three standing gates, green at every task boundary:**

- **Parser corpus** — `newbf-parser/tests/corpus.rs`, **154/154**
  zero-diagnostic (a 100% ratchet — adding a feature-suite file raises
  the denominator).
- **Verify corpus** — `newbf-sema/tests/corpus.rs`, **154/154**
  LLVM-clean (same ratchet). Note it lowers each file **standalone** and
  never calls `run_emission`/`fold_comptime`.
- **Run corpus** — `tests/newbf-tests/tests/run_corpus.rs`, ~204
  programs, JIT-run, full-i32 value check. **The authoritative
  behavioral gate.** Three of the four features in this wave produce
  *verify-clean miscompiles* (a guard-ABI drift, a ClassVData slot-shift,
  a mixin escape-stack desync) that `corpus.rs` cannot catch — for those,
  run-corpus is the only real gate.

**8-bit exit-code caveat:** AOT exit codes truncate to 8 bits; all
value-checks use the JIT run-corpus harness, which reads the full i32.
Runtime-safety fault/abort tests therefore use a **child-process runner**
(a fault kills the process; the SEH filter returns `CONTINUE_SEARCH`), so
they are a **new harness** (`guard_corpus`), not the value-checking
harness.

**Naming convention.** Each task keeps its *home-doc id* with a feature
prefix so it is traceable: `MS-T0` = memory-safety task 0, `CB-T4` =
comptime-breadth task 4, `RF-T2` = reflection task 2, `MX-T3` = mixins
task 3 (`MX-T4.5` keeps the doc's half-step id).

**Where the backend / runtime / ABI risk concentrates (read this first):**

| Feature | newbf-runtime? | JIT/AOT symbol seam? | `$header` ABI? | Pure sema/parser? |
| ------- | -------------- | -------------------- | -------------- | ----------------- |
| memory-safety | **YES** (new stomp/ledger crate, staticlib) | **YES** (ORC absolute symbols `newbf_alloc`/`newbf_free`; AOT staticlib link + `/ENTRY`) | **reads** (`emit_destroy`, interface-delete branch) | Track B (delete-flow) is pure sema |
| comptime-breadth | no (uses existing crash-dump only) | **YES** (ORC absolute symbol `__newbf_ct_emit`) | no | T0/T1/T3/T6 are sema/comptime; T4/T5 wrap analyze+lower |
| reflection | **NO** (deliberately — in-module LLVM accessor) | no (in-module `__newbf_type_by_id`) | **CHANGES** (ClassVData: `{i32 mType,[N×ptr]}` — shifts every vtable slot) | T0 parser, T1/T3 sema/IR, T4/T5 also LLVM emit |
| mixins | no (until staged MX-T8) | no (until staged MX-T8 FatalError) | no | **entirely** sema/parser (the safest feature) |

The two genuinely scary items: **memory-safety's runtime + JIT/AOT seam**
and **reflection's `$header` ABI rework**. Mixins is the safe,
fully-parallel track. comptime-breadth is medium (one JIT seam + a
fixpoint loop wrapping the front-end).

---

## Cross-feature dependency analysis

Each feature is internally a near-linear chain. The cross-feature
couplings, verified against the docs and the tree, are:

1. **One shared JIT seam unblocks two features.** memory-safety MS-T0 and
   comptime-breadth CB-T2 both add **ORC absolute-symbol registration to
   `OrcJit::from_ir`** (jit.rs:113 — verified: today it installs *only*
   `DynamicLibrarySearchGeneratorForProcess` at jit.rs:152, no absolute
   symbols exist). MS-T0 registers `newbf_alloc`/`newbf_free`/
   `newbf_install_crash_handler`; CB-T2 adds a generic
   `OrcJit::add_absolute_symbol(name,addr)` and registers
   `__newbf_ct_emit`. **These are the same mechanism.** Decision:
   **MS-T0 lands the `add_absolute_symbol` API** (it is the de-risking
   prerequisite the whole memory-safety feature stands on, and the
   earliest task in the wave), and **CB-T2 reuses that API** rather than
   re-implementing it. This makes MS-T0 the single highest-fanout task in
   the wave (it unblocks all of memory-safety *and* removes a blocker
   from comptime-breadth). If for scheduling reasons CB starts before MS,
   CB-T2 introduces the API and MS-T0 reuses it — but the recommended
   order lands MS-T0 first.

2. **reflection's `$header` ABI vs memory-safety's `$header` readers —
   ORDER THEM.** reflection RF-T2 rewrites the object header so it always
   points at a `%ClassVData = {i32 mType, [N×ptr]}` global (retiring the
   bare `vtable_name` global and the empty-vimpls `Null` branch), shifting
   every vtable slot and changing what `new` stores. memory-safety reads
   `$header` in two places: the **interface-delete bare-free branch** and
   the **`emit_destroy` concrete-class assertion** (MS-T4), and it does
   **not** change the header (the ledger is out-of-band; header stays
   stable — by design, memory-safety.md §6). They do not *both write* the
   header, but MS-T4's `emit_destroy`/interface-delete code and RF-T2's
   `type_test`/dispatch-GEP rework touch adjacent lowering code, and a
   `delete` after RF-T2 must free an object whose header is now *always*
   non-null ClassVData. **Sequence RF-T2 (the ABI rework, behind its
   named is/as + virtual + iface green list) BEFORE MS-T4 (scope cleanup
   + delete-flow runtime correctness)** so MS-T4's delete path is written
   against the final header shape. RF-T2 is behavior-preserving and
   well-gated; doing it first removes a moving target. (MS first slice —
   T0–T3 — does NOT touch `$header`; only MS-T4 does, so the runtime
   guard can land in parallel with reflection's early tasks.)

3. **comptime emission and reflection are independent in v1 (by explicit
   deferral in both docs).** comptime-breadth defers the reflection FFI
   table / `Type`/`typeof` lowering (CB §1.2, §9 staged); reflection
   defers comptime reflection (RF §10). The emitter uses **primitives
   only** — no `Type`, no `typeof`. So there is **no v1 dependency either
   direction**; they share only the JIT instance (additive) and the
   run-corpus harness. They can run as parallel tracks. (The natural
   next-wave merge — `[Comptime] typeof(T).GetFields()` — is explicitly
   out of both v1s.)

4. **Try! needs mixins; Result needs a proven Unwrap precursor.**
   Within mixins the chain is strict: MX-T1→T2→T2.5→T3→T4 (the splice
   machinery), with **MX-T4.5 (generic-enum-instance `switch(this)`
   Unwrap) a standalone precursor that can run in parallel with T1–T4**
   but must land before MX-T5 (Result.bf in prelude) and MX-T6 (Try!).
   Mixins has **zero** cross-feature dependency on the other three — it is
   pure sema, no runtime, no `$header`, no JIT seam. It is the ideal
   parallel filler track for a single reviewer between the riskier
   landings of the other three.

5. **The comptime fixpoint loop wraps `lower_program`; mixins MX-T8 (staged)
   changes its signature.** `run_emission` (CB-T2/T4) calls
   `analyze` + `lower_program` (lower.rs:3429, verified: takes
   `&[SourceFile]`, returns `Module`) in a loop. Mixins' *staged* MX-T8
   changes `lower_program -> (Module, Vec<Diagnostic>)`. **Both are in this
   plan's tail / staged region; if MX-T8 is ever pulled forward, it must
   co-update `run_emission`'s call site.** v1 mixins (MX-T1..T6) do NOT
   change the signature, so there is no v1 conflict. Flagged so the two
   features don't silently diverge on that function.

6. **Run-corpus leak reconciliation (MS-T5.5) is a corpus edit that every
   feature's run-corpus additions sit on top of.** MS-T5.5 fixes
   genuinely-leaking corpus programs (`prelude_probe.bf`, `list_hof*.bf`)
   to `delete`/`scope` what they `new`. It is behavior-neutral (return
   values unchanged) but it edits shared fixtures. Land it once, early-ish
   (right after MS-T5 identifies them), so later feature programs are added
   to an already-leak-clean corpus.

**Net ordering forces:**
- **MS-T0 first** (the JIT absolute-symbol seam — highest fanout, the
  de-risking prerequisite, unblocks all of memory-safety and a blocker in
  comptime).
- memory-safety first slice (T0–T3) is self-contained runtime + rename +
  host wiring; land it as an early block.
- **RF-T2 (ClassVData ABI) before MS-T4 (delete/scope, which reads the
  post-T2 header).**
- comptime-breadth and reflection run as parallel tracks (no v1 cross-dep).
- mixins is the fully-parallel safe track throughout (only internal deps +
  the MX-T4.5 precursor before MX-T5/T6).
- Staged tails (MS-T7, CB-T7, RF-T6/T7, MX-T7/T8) last.

---

## The critical path

The longest dependency chain, and the one that gates the most downstream
value, runs through **memory-safety** because it owns the JIT seam *and*
the runtime crate *and* the riskiest behavior-changing scope fix:

```
MS-T0 ─→ MS-T1 ─→ MS-T2 ─→ MS-T3 ─→ MS-T4 ─→ MS-T5 ─→ MS-T5.5 ─→ MS-T6 ─→ MS-T7
  │        (runtime)  (rename) (wire)   ↑
  │                                     │
  └── unblocks CB-T2 (reuses add_absolute_symbol)
                                        │
              RF-T0→T1→RF-T2 (ClassVData ABI) ─── must precede ──┘
```

- **MS first slice = MS-T0 → MS-T1 → MS-T2 → MS-T3** (4 serial tasks): a
  deterministic runtime UAF/double-free guard, end-to-end, all gates
  green, with **no red window** (MS-T0 lands the resolution seam before
  the MS-T2 rename, so the renamed symbols always resolve).
- **MS-T4 additionally waits on RF-T2** (it deletes objects whose header
  RF-T2 just changed). So the critical path is:

> **MS-T0 → MS-T1 → MS-T2 → MS-T3 → (join RF-T0→RF-T1→RF-T2) → MS-T4 → MS-T5 → MS-T5.5 → MS-T6 → MS-T7**

Everything else (CB-T0..T7, RF-T3..T7, MX-T1..T8) hangs off this spine
with slack. **The single most-unblocking task is `MS-T0`** (the ORC
absolute-symbol seam + smoke test): it is the root of the longest chain,
it is proven in isolation by a smoke test *before* any rename (no red
window), and the mechanism it introduces is the named prerequisite for
both memory-safety and comptime emission. It is also cheap (one
`jit.rs` edit + minimal `newbf-runtime` thunks + one smoke test).

---

## Sprint schedule

Nine review-batches. Within a sprint, PARALLELIZABLE tasks have no
ordering force; SERIAL tasks must land in order.

### Sprint A — Foundations: the JIT seam + open the safe parallel tracks
*Goal: land the load-bearing JIT absolute-symbol seam (de-risks the whole
wave) and open the two fully-independent infra roots. Demonstrable: the
MS-T0 smoke test JITs `newbf_alloc`/`newbf_free` and runs fault-free;
parser/verify/run corpora unchanged.*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **MS-T0** | JIT absolute-symbol seam (`add_absolute_symbol`) + `newbf_alloc`/`newbf_free` thunks + resolution smoke test | memory-safety | — | SERIAL (do first) |
| **RF-T0** | Parser: `Expr::TypeOf{ty}` + attribute enum-flag capture | reflection | — | PARALLEL with MS-T0 |
| **CB-T0** | Extension member composition in StructTable (append-not-replace) | comptime-breadth | — | PARALLEL with MS-T0 |
| **MX-T1** | AST variants (`MixinCall`/`MixinDecl`/`Member::Mixin`) + 4-site parser rewire + walker audit | mixins | — | PARALLEL with MS-T0 |

All four are roots. They touch disjoint regions (`jit.rs`+runtime /
parser `typeof` / `struct_kind`+extension-fill / mixin AST). RF-T0,
CB-T0, MX-T1 are all behavior-preserving (parser/substrate); MS-T0 is
proven by a standalone smoke test.

### Sprint B — Runtime guard core + the eval/IR substrates
*Goal: the stomp allocator + ledger lands runtime-only; the comptime eval
core and reflection IR plumbing land. Demonstrable: `cargo test -p
newbf-runtime` green (quarantine, double-free tombstone, Thunk path);
CB widened-int eval unit tests; RF IR golden updated.*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **MS-T1** | Stomp allocator + VM shim + ledger + tombstones (runtime-only) | memory-safety | MS-T0 | SERIAL after MS-T0 |
| **CB-T1** | Widen the eval core: `eval_const(m,n,ret)`, sign/zero-extend, float→typed Err | comptime-breadth | — | PARALLEL |
| **RF-T1** | IR metadata repr (`TypeMeta`/`ReflectPolicy`/`LoadTypeId`/`VtableDef.type_id`) | reflection | RF-T0 | PARALLEL |
| **MX-T2** | Mixin collection registry + owned `StructTable.srcs: Vec<String>` | mixins | MX-T1 | PARALLEL |

### Sprint C — Symbol rename + comptime skeleton + reflection ABI prep
*Goal: the alloc-path rename + the comptime emission skeleton (no-op fast
path) + the reflection ClassVData ABI rework. Demonstrable: run-corpus
green after the rename (arrays+closures exercised); comptime fast-path
no-op leaves all corpora unchanged.*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **MS-T2** | Alloc-path symbol rename, shape-aware `heap_alloc(size, AllocKind)` (all 6 sites) | memory-safety | MS-T0, MS-T1 | SERIAL (after MS-T1) |
| **CB-T2** | `run_emission` skeleton + no-op fast path + reuse `add_absolute_symbol` for `__newbf_ct_emit` + `EMIT_SINK` | comptime-breadth | CB-T1, **MS-T0 (`add_absolute_symbol`)** | PARALLEL |
| **RF-T2** | ClassVData ABI: `{i32 mType,[N×ptr]}` + 3 `$header` sites + `load_vtable_base`/`load_type_id` + emission unit test | reflection | RF-T1 | PARALLEL (must precede MS-T4) |
| **MX-T2.5** | `Mixins.bf` shape-by-shape audit + strict-gate predicate spec | mixins | MX-T2 | PARALLEL |

> CB-T2 depends on MS-T0 only for the `add_absolute_symbol` API. If MS-T0
> hasn't landed when CB-T2 is reviewed, CB-T2 introduces the API itself
> and MS-T0 reuses it; the recommended order avoids that.

### Sprint D — Wire the guard end-to-end; sema records emitters
*Goal: the runtime is live in JIT + AOT hosts (guard_corpus harness
exists); sema records comptime emit jobs + rewrites generator bodies.
Demonstrable: `guard_corpus`: `uaf_after_delete`→fault,
`double_free`→abort, `no_leak_balanced`→ledger==0; one debug + one
release AOT parity test.*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **MS-T3** | Wire runtime into JIT + AOT hosts (`set_guard_mode`/`reset`; AOT staticlib + `/ENTRY:newbf_entry`); new `guard_corpus` child-process harness | memory-safety | MS-T0,1,2 | SERIAL (closes MS first slice) |
| **CB-T3** | Sema records emit generators (`EmitJob`) + body rewrite to `__newbf_ct_emit(<owner_id_lit>, …)` | comptime-breadth | CB-T2 | PARALLEL |
| **RF-T3** | Sema policy (`reflect_policy`) + dense name-sorted type-ids + record `module.type_meta` | reflection | RF-T0,1,2 | PARALLEL |

### Sprint E — Reflection emission; comptime first real emission; mixin first slice; safety scope fix
*Goal: each feature's first marquee behavior. Demonstrable:
`reflect_typeid_distinct.bf→1`; `comptime_emit_member.bf→42`;
`mixin_block_yield.bf` passes; safety scope-in-if dtor fires exactly once.*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **RF-T4** | LLVM Type-globals + `__newbf_type_by_id` accessor + `typeof` lowering + corlib `Type.bf` + `StrEq` | reflection | RF-T0,1,2,3 | PARALLEL |
| **CB-T4** | First real emission: EmitTypeBody→extension→reparse→resolve→call; strip emitter/shim; JIT+AOT link-clean | comptime-breadth | CB-T0, CB-T3 | PARALLEL |
| **MX-T3** | FIRST SLICE: stmt + expr (incl. block-trailing-yield) expansion, strict gate, stack truncation | mixins | MX-T2.5 | PARALLEL |
| **MS-T4** | Scope cleanup on all exit edges (per-site null-guarded slots) + delete de-registration + interface-delete bare-free + `emit_destroy` concrete assert | memory-safety | MS-T3, **RF-T2** | SERIAL (after RF-T2) |

### Sprint F — Feature mid-bodies: GetType, fixpoint guards, mixin escape, delete-flow double-free
*Goal: dynamic `GetType`, comptime fixpoint hardening, mixin control-flow
escape, and the first delete-flow diagnostic. Demonstrable:
`reflect_gettype_id_roundtrip.bf→1`; `comptime_emit_idempotent.bf`;
`mixin_return_escape.bf→7`; `provable_double_free.bf` one diagnostic.*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **RF-T5** | `GetType()` runtime lookup (`LoadTypeId` → `__newbf_type_by_id`) | reflection | RF-T2, RF-T4 | PARALLEL |
| **CB-T5** | Fixpoint guards (round/byte caps, idempotent dedup) + diagnostics; `comptime_emit_virtual` | comptime-breadth | CB-T4 | PARALLEL |
| **MX-T4** | Control-flow escape (return/break/continue target caller) + empty-loop/static-this guards + stack discipline | mixins | MX-T3 | SERIAL after MX-T3 |
| **MX-T4.5** | Generic enum instance method `switch(this)` Unwrap (precursor) | mixins | — | PARALLEL (before MX-T5) |
| **MS-T5** | Delete-flow: double-free first (4-state lattice, incl. scope-delete) | memory-safety | — (structural) | PARALLEL |

### Sprint G — Const-eval breadth; field metadata; Result prelude; corpus leak reconciliation
*Goal: widened-int comptime folding, reflection field metadata, the
Result corlib prelude, and the corpus leak fix. Demonstrable:
`comptime_eval_i32_arg.bf→49`; `reflect_field_count_marked.bf→2`;
`generic_result_unwrap.bf` passes with Result.bf in prelude.*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **CB-T6** | Const-eval breadth: widened-int args + fold-width fix (`InstData.ty`) + inner-fold-first | comptime-breadth | CB-T1, CB-T4 | PARALLEL |
| **RF-T6** | Field metadata + `GetFieldCount`/`GetField`/`FieldInfo` | reflection | RF-T4 | PARALLEL |
| **MX-T5** | `Result.bf` corlib prelude + collision reconciliation (full corpora green WITH prelude) | mixins | MX-T4.5 | SERIAL (gates on full corpora) |
| **MS-T5.5** | Corpus leak reconciliation (fix `prelude_probe`/`list_hof*` to delete/scope) | memory-safety | MS-T5 | SERIAL after MS-T5 |

### Sprint H — Behavioral completions: provable leak, method metadata, Try! end-to-end
*Goal: the provable-leak diagnostic, reflection method metadata + phase
report, and `Try!` proven end-to-end. Demonstrable: `provable_leak.bf`
one diagnostic, zero false positives on the fixed corpus;
`result_try_ok.bf`/`result_try_err_escape.bf` pass.*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **MS-T6** | Delete-flow: provable leak (exit-edge Owned-survivor + Moved/Dropped rules) | memory-safety | MS-T5, MS-T5.5 | SERIAL |
| **RF-T7** | Method metadata + `System.Reflection` stubs + `format_reflection` golden | reflection | RF-T6 | PARALLEL |
| **MX-T6** | `Try!` corpus mixin end-to-end (var-param, block-yield, same-error escape) | mixins | MX-T4, MX-T5 | SERIAL |

### Sprint I — Tails: named sites, docs, journals, staged items
*Goal: named fault/leak sites, every feature's journal + verify-corpus
pin + doc cross-link, and the explicitly-staged out-of-v1 items.
Demonstrable: a UAF report names `<function> @ file:line`; CB/RF/MX/MS
journal sections written.*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **MS-T7** | Site-id table + named fault/leak sites (`Module.alloc_sites`) | memory-safety | MS-T2, MS-T3 | PARALLEL |
| **CB-T7** | Comptime docs (COMPTIME.md loop + emit FFI + v1 boundaries) + journal | comptime-breadth | CB-T0..T6 | PARALLEL |
| **MX-T7** | (Staged) generic + cross-file + lvalue-yield + lambda-in-body + canonical Try! + `(.)err` | mixins | MX-T6 | PARALLEL / staged |
| **MX-T8** | (Staged) diagnostics plumbing (`lower_program → (Module, Vec<Diagnostic>)`) + real `Internal.FatalError` + comptime ungating | mixins | MX-T7 | PARALLEL / staged |
| **journals** | Per-feature journal §§ + verify-corpus pins + doc cross-links (MS, RF) | all | feature tails | SERIAL after each feature tail |

> **MX-T8 changes `lower_program`'s signature** — the same function CB's
> `run_emission` calls. If MX-T8 is pulled forward into v1, it MUST
> co-update `run_emission` (CB-T2/T4). Staged here precisely to avoid that
> collision during the v1 wave.

---

## Per-task reference (id · title · feature · deps · agent-prompt seed · acceptance gate)

> Gate shorthand: **3 gates** = parser 154/154 + verify 154/154 +
> run-corpus all-pass. A task lands only when 3 gates **and** its new gate
> are green. Runtime-safety tasks additionally list **how the gate is
> OBSERVED** (a fault/abort via the child-process `guard_corpus` runner, a
> ledger==0 report). Comptime-emission tasks list the emitted-member /
> link-clean observation. Reflection ABI tasks list the named regression
> green list + the non-JIT emission unit test (the only slot-shift detector).

### memory-safety (home doc §9) — *runtime crate + JIT/AOT seam (riskiest)*

- **MS-T0** · *JIT absolute-symbol seam + resolution smoke test* · memory-safety · deps: — ·
  *seed:* Add `OrcJit::add_absolute_symbol(name,&addr)` (an ORC `LLVMOrcAbsoluteSymbols` MaterializationUnit) and, in `from_ir` *before* `AddLLVMIRModule`, register `newbf_alloc`/`newbf_free`/`newbf_install_crash_handler` (addresses from `newbf-runtime` `fn` items); add the `newbf-llvm → newbf-runtime` dep; `newbf-runtime` exports minimal `newbf_alloc`/`newbf_free` (may be malloc/free thunks now), `crate-type=["rlib","staticlib"]`. ·
  *gate:* a `newbf-llvm` smoke test JITs a module calling `newbf_alloc(16,-1,0)`/`newbf_free`, runs without fault; **3 gates unchanged**. *This proves the load-bearing seam before any rename — no red window.*

- **MS-T1** · *Stomp allocator + VM shim + ledger (runtime-only)* · memory-safety · deps: MS-T0 ·
  *seed:* `newbf-runtime/src/guard/{mod,vm,stomp,ledger}.rs`: port `StompAlloc.cpp` behind a `Vm` trait (quarantine — never recycle pages; size-0/page-multiple edge cases); ledger keyed by user-ptr with persistent tombstones; double-free/wild-free → abort+dump (ledger-first, never deref a decommitted header); `newbf_set_guard_mode(Stomp|Thunk)`, `newbf_guard_reset`, `newbf_guard_report_leaks`, `enter/exit_comptime`; publish via existing lock-free `note_*`/`update_guard_metrics`. ·
  *gate:* `cargo test -p newbf-runtime` green incl. alloc/free/decommit, **quarantine** (a freed address never returns from a later alloc), size-0, double-free→tombstone, wild-free, ledger counts, reset, **release Thunk path**; the JIT+stomp smoke test (child-process runner asserts the post-free read faults).

- **MS-T2** · *Alloc-path symbol rename (shape-aware helper)* · memory-safety · deps: MS-T0, MS-T1 ·
  *seed:* `heap_alloc(size, AllocKind{Object(id)|Array{hdr}|Raw})` emitting `newbf_alloc(size:i64, type_id:i32, site_id:i32)`; route all six sites (5603 Raw, 7256 Array, 7395/7468/7492 Object); replace three `free`s with `newbf_free`; **drop the −8 reconstruction at 7557** (free the elements pointer; ledger maps it). ·
  *gate:* verify 154/154 LLVM-clean **and run-corpus ~204 green** (arrays/closures exercised; resolves via MS-T0). **No red window** because MS-T0 precedes this.

- **MS-T3** · *Wire runtime into JIT + AOT hosts + guard_corpus harness* · memory-safety · deps: MS-T0,1,2 ·
  *seed:* `run_corpus.rs` calls `install_crash_handler`+`set_guard_mode` once (+`newbf_guard_reset`; symbols inject via MS-T0 so no new harness Cargo dep); driver startup; `aot.rs` adds the runtime staticlib + `/ENTRY:newbf_entry` bootstrap; **new `guard_corpus` child-process runner** (spawn a runner exe per program, inspect exit code/WER). ·
  *gate (OBSERVED via child-process):* run-corpus ~204 green; `guard_corpus`: `uaf_after_delete.bf`→ACCESS_VIOLATION, `double_free.bf`→guard abort "double free", `no_leak_balanced.bf`→ledger==0; **one debug + one release AOT parity test** (double_free aborts; no_leak clean exit). *Closes the MS first slice.*

- **MS-T4** · *Scope cleanup all-exit + delete de-reg + interface-delete branch* · memory-safety · deps: MS-T3, **RF-T2** ·
  *seed:* Per-site null-guarded slots via an entry-block-alloca API (alloca + explicit `store null`); unify dominating value-list vs non-dominating slot (each alloc in exactly one); `lower_delete` de-registers an explicitly-deleted scope binding; `break`/`continue` run depth-range frame cleanup before branching; interface-typed `Ref` delete takes the bare `newbf_free` branch; `emit_destroy` asserts a concrete class id. **Depends on RF-T2 so the post-ClassVData header shape is final.** ·
  *gate:* **verify-corpus** clean incl. new `scope_in_if_branch.bf`/`scope_in_both_if_branches.bf`/`scope_with_early_return.bf`/`scope_in_if_in_while_break.bf`; run-corpus value-checks a dtor fires **exactly once** on each exit edge (fallthrough/return/break/continue).

- **MS-T5** · *Delete-flow: double-free first* · memory-safety · deps: — (structural) ·
  *seed:* `newbf-sema/src/ownership.rs` (new) + `check_delete_flow` called from `analyze` after `resolve_and_check`; per-body minimal type map; 4-state lattice; diagnose provable double-`delete` **and** `delete` of a `scope`-bound binding only. Pure sema, no IR/llvm. ·
  *gate:* `provable_double_free.bf` (incl. scope-delete case) one diagnostic each; **zero** new diagnostics across the corpus; run-corpus unchanged.

- **MS-T5.5** · *Corpus leak reconciliation* · memory-safety · deps: MS-T5 ·
  *seed:* Fix genuinely-leaking corpus programs (`prelude_probe.bf`, `list_hof*.bf`, others MS-T5 flags) to `delete`/`scope` what they `new`; behavior-neutral. ·
  *gate:* run-corpus **values unchanged**; the fixed corpus is leak-clean (makes MS-T6's ratchet honest).

- **MS-T6** · *Delete-flow: provable leak* · memory-safety · deps: MS-T5, MS-T5.5 ·
  *seed:* Exit-edge `Owned`-survivor → leak; full rules (arg-pass stays Owned; only return/tracked-reassign → Moved; capture/field-store/address-of → Dropped; sugar allocations untracked). ·
  *gate:* `provable_leak.bf` one diagnostic; negatives silent; **zero** false positives across the fixed corpus.

- **MS-T7** · *Site-id table + named fault/leak sites* · memory-safety · deps: MS-T2, MS-T3 ·
  *seed:* `Module.alloc_sites: Vec<AllocSite>`; backend emits `__newbf_alloc_sites`+count; runtime resolves `site_id→"<function> @ file:line"`; sema passes a real `site_id`. ·
  *gate:* a UAF/leak report names `<function> @ file:line`; release omits the table; 3 gates green.

### comptime-breadth (home doc §9) — *one JIT seam + a fixpoint loop*

- **CB-T0** · *Extension member composition (append-not-replace)* · comptime-breadth · deps: — ·
  *seed:* Recognize `TypeKind::Extension` in `struct_kind` (resolve id via `by_name`, don't allocate a new id); partial member-fill that **appends** ctors/methods/virtuals/vimpls and **adds (never replaces)** fields/`field_elems`/`field_inits`; preserve original field defaults; reject duplicate-signature members. ·
  *gate:* hand-written `extension_member_reads_field.bf` (**expect: 42**, no comptime) passes in run-corpus; **verify corpus 154/154 with extension support**; parser unchanged.

- **CB-T1** · *Widen the eval core* · comptime-breadth · deps: — ·
  *seed:* `eval_const(module, name, ret: IrType)` reading `i8/i16/i32/i64/bool` at width with mask-then-sign/zero-extend per `IrType::Int{signed}` (Win64 RAX upper-bits-undefined); float/ptr/struct → typed `Err`; keep `eval_const_i64` as a wrapper. ·
  *gate:* existing eval tests pass; new width tests incl. **negative** (`i32=-1`, `i8=-7`) sign-extend + **unsigned near-max** (`u8=250`) zero-extend + float-`Err`; all corpora unchanged.

- **CB-T2** · *`run_emission` skeleton + no-op fast path + `__newbf_ct_emit` absolute symbol* · comptime-breadth · deps: CB-T1, **MS-T0** ·
  *seed:* New `emit.rs` + `lib.rs` re-export; `Module.emit_jobs: Vec<EmitJob>` (default empty); reuse `OrcJit::add_absolute_symbol` (from MS-T0) to bind `__newbf_ct_emit` + define the `EMIT_SINK` thread-local shim; verify `[EmitGenerator]` parses **and** is retrievable; loop returns the module verbatim when `emit_jobs` empty; wire `run_emission` into driver (main.rs ×3) + run-corpus harness. ·
  *gate:* all run-corpus + both static corpora **unchanged** (fast-path no-op); a unit test that `add_absolute_symbol` + a tiny IR fn calling `__newbf_ct_emit` populates `EMIT_SINK` with **no duplicate-definition error** (shim wins over the process generator).

- **CB-T3** · *Sema records emit generators + body rewrite* · comptime-breadth · deps: CB-T2 ·
  *seed:* `comptime_emitter_of(attrs,src)`; at lower.rs:4168 push `EmitJob{owner_qual_name, symbol}` (also keep `module.comptime`); rewrite the generator body's `Compiler.EmitTypeBody(text)` to `__newbf_ct_emit(<owner_id_literal>, text.Ptr, text.Len)`. ·
  *gate:* a sema unit test shows `emit_jobs` populated and `module.comptime` still contains the generator; corpora unchanged (no corpus program uses the marker yet); verify 154/154 with the new corlib `Compiler` class present.

- **CB-T4** · *First real emission slice* · comptime-breadth · deps: CB-T0, CB-T3 ·
  *seed:* `emit.rs` fixpoint body (per-round name→id map; JIT a `$ct_emit_run` nullary wrapper in a sandbox clone; drain sink; resolve id→qual-name; normalize+dedup; wrap as `extension Owner{…}`; re-analyze+re-lower; **strip emitter/shim before return**); corlib `Compiler.EmitTypeBody` + `__newbf_ct_emit` extern + minimal `Compiler` static class (primitives-only). ·
  *gate (run-corpus authoritative):* `comptime_emit_member.bf` (**expect: 42**, reads a pre-existing field), `comptime_emit_then_call_twice.bf`, `comptime_emit_dead_member.bf` pass; **final module JIT-links AND AOT-links with no unresolved symbols** (explicit assertion); `dump-ir` golden: generated symbol **present**, generator + `__newbf_ct_emit` **absent**.

- **CB-T5** · *Fixpoint guards + diagnostics* · comptime-breadth · deps: CB-T4 ·
  *seed:* `emitted` dedup over normalized text; `MAX_EMIT_ROUNDS` (16, configurable) + byte cap; per-round determinism ordering; `EmitOutcome.diagnostics`; abort on generated-code analyze diagnostics; driver merges them. ·
  *gate:* `comptime_emit_idempotent.bf`, `comptime_emit_virtual.bf` pass; integration tests that a **returning-but-divergent** emitter trips the cap with a diagnostic (no crash/hang) and a missing-field emission aborts with an analyze diagnostic.

- **CB-T6** · *Const-eval breadth: widened-int args + fold-width fix + inner-fold-first* · comptime-breadth · deps: CB-T1, CB-T4 ·
  *seed:* `fold.rs` accepts foldable `i8/i16/i32/i64` returns (via CB-T1's `eval_const`); **rewrite using the call's own `InstData.ty`, not hardcoded I64**; iterate the collect/apply loop to a fixpoint for nested folds. ·
  *gate:* `comptime_eval_i32_arg.bf` (**expect: 49**, verify-clean), `comptime_nested_fold.bf` pass; `keeps_comptime_called_with_runtime_arg` still passes.

- **CB-T7** · *Docs + journal* · comptime-breadth · deps: CB-T0..T6 ·
  *seed:* expand COMPTIME.md (loop, emit FFI shim, float/generic/mixin/`Type` v1 boundaries); add journal §. ·
  *gate:* docs build; journal pairs with the feature commits.

### reflection (home doc §9) — *changes the `$header` ABI (verify-clean-but-wrong risk)*

- **RF-T0** · *Parser: `Expr::TypeOf{ty}` + attr-flag capture* · reflection · deps: — ·
  *seed:* Add `Expr::TypeOf{span,ty}` (mirror `Expr::SizeOf`; add to `span()` + every exhaustive `Expr` match + `print.rs`); re-point `Keyword::TypeOf` (parser.rs:1136-1143) to **keep** the `ty` (it currently discards it); capture `[Reflect(.Fields)]`/`[AlwaysInclude]` enum-flag identifier text into the attribute record. ·
  *gate:* **parser corpus 154/154**; a unit test that `typeof(Dog)` → `TypeOf{ty:Dog}`.

- **RF-T1** · *IR metadata representation + `LoadTypeId`* · reflection · deps: RF-T0 ·
  *seed:* `module.rs`: `ReflectPolicy`, `FieldMeta`, `MethodMeta`, `TypeMeta`, `Module.type_meta`, `add_type_meta`, `VtableDef.type_id` (default 0); `inst.rs`: `LoadTypeId{obj}` (result I32); `print.rs` + every exhaustive `InstKind`/`VtableDef` match in sema/llvm (compile-only stubs). ·
  *gate:* workspace compiles; **verify + run corpus unchanged** (no emission yet); IR golden/print tests updated.

- **RF-T2** · *ClassVData ABI + 3 `$header` sites + helpers (load-bearing)* · reflection · deps: RF-T1 ·
  *seed:* Registration loop (lower.rs:3482) registers ClassVData for **every** `StructKind::Ref` id (entries empty when vimpls empty); `new`-site (7398) always stores `&classvdata_name(id)` (delete the empty-vimpls Null branch); `type_test` (8006) compares `classvdata_name`; add `load_vtable_base` (struct-GEP `%ClassVData` field 1) + `load_type_id` (field 0 as i32); route virtual (8526) + iface (8366) dispatch through `load_vtable_base`; `newbf-llvm` `emit_vtables`→`emit_classvdata` emitting `%ClassVData={i32,[N×ptr]}`, retire bare `vtable_name`; **all three sites change atomically**; update the itable invariant harness to stay green; add the `%ClassVData`-shape + field-1-GEP emission unit test. ·
  *gate (the regression wall — NAMED green list):* `is_as.bf`, `virtual_poly.bf`, `virtual_basic.bf`, `abstract_method.bf`, `base_call.bf`, the three `iface_*` programs — **all green**; the **non-JIT emission unit test** (the only slot-shift detector) green; verify 154/154. No reflection behavior yet. *Must precede MS-T4.*

- **RF-T3** · *Sema policy + dense type-ids + record `type_meta`* · reflection · deps: RF-T0,1,2 ·
  *seed:* `reflect_policy(attrs,src,default)`; **name-sorted** dense type-id assignment over reflectable types (stable across corlib growth); populate `module.type_meta` after monomorph+vtable layout; set `VtableDef.type_id`. Pure sema→IR, no LLVM. ·
  *gate:* verify/run green; a unit test asserts `module.type_meta` has correct `(name, policy, field_count)` for marked vs unmarked.

- **RF-T4** · *LLVM Type-globals + registry accessor + typeof + corlib Type.bf + StrEq* · reflection · deps: RF-T0,1,2,3 ·
  *seed:* `emit_metadata` (Type globals; policy-gated FieldInfo/MethodInfo arrays; `__newbf_type_table`/`_count`/`_unknown`; the in-module `__newbf_type_by_id` LLVM function); `Expr::TypeOf` arm (`GlobalAddr` of `type_global_name`, sentinel for non-class); new `bf/Type.bf` **registered in `prelude()`** (a value `struct`) + `StrEq(char8*,char8*)` in Internal/String + a standalone `streq_basic.bf` smoke; the corlib-`Type`-layout-vs-`%struct.Type` unit test. ·
  *gate:* run-corpus `reflect_typeid_distinct.bf→1`, `reflect_typeof_name.bf→1`, `streq_basic.bf→1`, `reflect_strip_vs_marked.bf→1` (differential 2-vs-0), `reflect_typeof_size.bf` pass; the strip emission unit test (mFields null when unmarked) passes.

- **RF-T5** · *`GetType()` runtime lookup* · reflection · deps: RF-T2, RF-T4 ·
  *seed:* `recv.GetType()` (gated on heap `Ref` receiver + no user override; value-type → `typeof(static)`) → `LoadTypeId` + `__newbf_type_by_id`; lower `LoadTypeId` in `newbf-llvm`. ·
  *gate:* run-corpus `reflect_gettype_id_roundtrip.bf→1`, `reflect_gettype_polymorphic.bf→1` pass.

- **RF-T6** · *Field metadata + GetFieldCount/GetField* · reflection · deps: RF-T4 ·
  *seed:* emit `[k×%FieldInfo]` (name/offset/typeId) under `policy.has(FIELDS)`; corlib `GetFieldCount`/`GetField(i)->FieldInfo` + `FieldInfo.GetName`. ·
  *gate:* run-corpus `reflect_field_count_marked.bf→2`, `reflect_field_name.bf→1` pass.

- **RF-T7** · *Method metadata + System.Reflection stubs + phase report* · reflection · deps: RF-T6 ·
  *seed:* emit `%MethodInfo` arrays under `policy.has(METHODS)`; corlib `System.Reflection` (`MethodInfo` name-only, `BindingFlags` stub); `format_reflection` diff-gated golden (rows keyed by name). ·
  *gate:* a `reflect_method_count.bf` passes; the reflection report captured as a golden file. (Includes the RF journal § + verify-corpus pin + doc cross-link.)

### mixins (home doc §9) — *pure sema (the safe, fully-parallel track)*

- **MX-T1** · *AST variants + 4-site parser rewire + walker audit* · mixins · deps: — ·
  *seed:* `Expr::MixinCall{callee, scope_qualifier, type_args, args}`, `Stmt::MixinDecl`, `Member::Mixin` (+`span()` arms + `print.rs`); switch all **four** parser emit sites (3105/1339/540/560 — incl. `name!<T>(…)` → `MixinCall` with `type_args`); update/wildcard-skip-with-intent every `Stmt` walker in newbf-sema (`collect_lambdas_stmt`, `collect_local_fns_stmt`, `caps_stmt`, lowering `stmt`). ·
  *gate:* parser-corpus + sema no-panic + verify-corpus all green (sema still ignores the new variants). Behavior-preserving.

- **MX-T2** · *Mixin collection registry + owned `srcs`* · mixins · deps: MX-T1 ·
  *seed:* `MixinDef`/`MixinParam`/`MixinParamKind` (owned `String`/`AstType`/`MethodBody`); `StructTable.mixins: HashMap<String,Vec<MixinDef>>`; **`StructTable.srcs: Vec<String>`** populated in `build` (owned per-file source copy — the cross-src resolution); `collect_mixins` walker recording `owner`/`src_file`/`has_lambda_or_localfn`/`yields_place`; generics collected + flagged. ·
  *gate:* verify-corpus green (collection only); a unit assertion that a known mixin lands in `mixins` with the right `src_file` + gate flags. Behavior-preserving.

- **MX-T2.5** · *`Mixins.bf` shape-by-shape audit + strict-gate spec* · mixins · deps: MX-T2 ·
  *seed:* enumerate every construct in `feature-suite/src/Mixins.bf` against the §3.8 table; define the gate predicate so each unsupported shape returns `None` from `expand_mixin` and falls back to the EXISTING verifiable path (`_ => {}` / unresolved-default). No new behavior. ·
  *gate:* documented disposition for every `Mixins.bf` construct; the gate predicate compiles; with expansion still off, verify-corpus stays green. Behavior-preserving.

- **MX-T3** · *FIRST SLICE: stmt + expr (incl. block-trailing-yield) expansion* · mixins · deps: MX-T2.5 ·
  *seed:* `MixinFrame`/`mixin_stack` (reset in `Lowerer::new`); `expand_mixin` (strict gate from 2.5; lockstep scope/defer/scope_allocs frame; param-bind-once in caller src incl. limited `VarInfer`; splice in `srcs[src_file]`; **block-trailing-`Stmt::Expr` → store into pre-alloca'd result slot guarded by `!terminated`**; no-target two-pass/diagnostic; depth guard; **unconditional stack truncation to the pre-splice snapshot**); wire into `stmt` (before the skip) + the `expr` `MixinCall` arm (after fn-value, before unresolved-default). ·
  *gate:* **full verify corpus 100% clean-verify with expansion ON, 0 new failures on `Mixins.bf`**; run-corpus `mixin_stmt_basic`, `mixin_expr_value`, `mixin_block_yield`, `mixin_arg_once`, `mixin_this_field`, `mixin_nested`, `mixin_local_no_leak` pass; static-`this` + untargeted-subexpr verify files clean.

- **MX-T4** · *Control-flow escape + stack discipline + guards* · mixins · deps: MX-T3 ·
  *seed:* confirm `return`/`break`/`continue` target the caller; empty-`loops` guard; terminated-after-escape result-load guard; `caller_loops_len`/`caller_ret_ty` snapshots; verify the unconditional truncation across an escaping splice. ·
  *gate:* run-corpus `mixin_return_escape→7`, `mixin_break_loop`, `mixin_stmts_after_escape` pass; break-outside-loop verify file clean (no panic).

- **MX-T4.5** · *Generic enum instance method switch-on-this (Unwrap)* · mixins · deps: — (parallel; before MX-T5/T6) ·
  *seed:* prove/fix `Result<int32,bool>.Unwrap()` (generic enum instance method, `switch(this)`, **confirm `var` binding** in `enum_pattern`, `.Err`→`default`) lowers, monomorphizes, runs — independent of mixins. ·
  *gate:* `generic_result_unwrap.bf` passes; gates green.

- **MX-T5** · *`Result.bf` corlib prelude + collision reconciliation* · mixins · deps: MX-T4.5 ·
  *seed:* `bf/Result.bf` (`Result<T,E>`, `Result<T>`, `Value`/`Unwrap` with `.Err`→`default`, **no FatalError**); grep+reconcile existing bare `Result`/`Option` fixtures (`result_generic.bf`, `corlib-slice/Result.bf`, `corlib-slice/Platform.bf`); confirm/namespace monomorph keys so `System.Result` ≠ bare `Result`. ·
  *gate:* **full verify + run corpora green WITH `Result.bf` in the prelude**; a program constructs+`Unwrap`s a happy path; the `.Err`-branch-lowers verify file clean.

- **MX-T6** · *`Try!` corpus mixin end-to-end* · mixins · deps: MX-T4, MX-T5 ·
  *seed:* run-corpus files defining the v1 concrete `Try!` (`var res` param, block-trailing `res.Value` yield, same-error escape) + `result_try_ok.bf`, `result_try_err_escape.bf`. ·
  *gate:* both run-corpus programs pass value checks; gates green. (Includes the MX journal §.)

- **MX-T7** · *(Staged) generics + cross-file + lvalue-yield + lambda-in-body + canonical Try! + `(.)err`* · mixins · deps: MX-T6 ·
  *gate:* generic `Try!` drives multiple monomorphized programs; a cross-file corlib mixin runs; gates green. Out of v1 slice.

- **MX-T8** · *(Staged) diagnostics plumbing + real FatalError + comptime ungating* · mixins · deps: MX-T7 ·
  *seed:* `lower_program -> (Module, Vec<Diagnostic>)` (update run_corpus.rs, verify corpus, driver, **AND CB's `run_emission` call site**); real `Internal.FatalError` (extern → `newbf-runtime` abort symbol resolvable in BOTH OrcJit process-search AND AOT link — reuses MS-T0's `add_absolute_symbol`); ungate comptime/const mixins. ·
  *gate:* diagnostic snapshot tests; a fatal-path program aborts in both JIT and AOT; comptime-mixin run-corpus folds correctly. Out of v1 slice.

---

## Recommended execution order (single reviewer, one agent at a time)

A linearization of the DAG keeping every commit behind green gates and
minimizing context-switching. Critical-path tasks marked ★.

1. **MS-T0** ★ — the JIT absolute-symbol seam + smoke test. Cheapest
   highest-fanout change; root of the longest chain; unblocks all of
   memory-safety AND comptime emission. **Do this first.**
2. **RF-T0**, **CB-T0**, **MX-T1** — open the three other tracks (parser
   `typeof`, extension substrate, mixin AST). All behavior-preserving,
   easy to review in isolation.
3. **MS-T1** ★ — stomp allocator + ledger (runtime-only `cargo test`).
4. **RF-T1**, **CB-T1**, **MX-T2** — IR metadata plumbing, eval widening,
   mixin collection. Still no behavior change in the corpora.
5. **MS-T2** ★ — the alloc-path rename (run-corpus is the gate; resolves
   via MS-T0, no red window).
6. **RF-T2** — the ClassVData ABI rework (behind its named is/as +
   virtual + iface green list + the emission unit test). **Land before
   MS-T4.**
7. **CB-T2**, **MX-T2.5** — comptime skeleton (reuses MS-T0's API),
   mixin gate spec.
8. **MS-T3** ★ — wire the guard into JIT + AOT; stand up the
   `guard_corpus` child-process harness. **First big runtime-safety
   milestone** (UAF faults, double-free aborts).
9. **RF-T3 → RF-T4** — reflection policy + the LLVM emission slice (lands
   `reflect_typeid_distinct→1`, the canonical reflection green).
10. **CB-T3 → CB-T4** — sema records emitters, then the first real
    emission (lands `comptime_emit_member→42` + the link-clean assertion).
11. **MX-T3** — the mixin first slice (stmt + expr + block-yield).
12. **MS-T4** ★ — scope cleanup all-exit + delete de-reg + interface-delete
    (now safe — RF-T2's header shape is final).
13. **MX-T4 + MX-T4.5** — mixin escape; the Unwrap precursor (parallel).
14. **RF-T5** — `GetType()` dynamic lookup. **CB-T5** — fixpoint guards.
15. **MS-T5 → MS-T5.5** — delete-flow double-free, then corpus leak fix
    (do MS-T5.5 before adding more run-corpus programs anywhere).
16. **CB-T6** — const-eval breadth. **RF-T6** — field metadata.
    **MX-T5** — Result.bf prelude (gates on full corpora green).
17. **MS-T6** ★ — provable-leak diagnostic. **MX-T6** — Try! end-to-end.
    **RF-T7** — method metadata + report.
18. **MS-T7** — named sites. **CB-T7** — comptime docs/journal. Journals
    + verify-corpus pins for each feature.
19. **Staged (next wave or end-of-wave slack): MX-T7, MX-T8.** MX-T8 must
    co-update CB's `run_emission` call site.

> **Earliest demoable state:** after step 8 you have a deterministic
> runtime memory guard (UAF faults, double-free aborts) — the marquee
> Beef signature. After step 11 you additionally have reflection
> `typeof`, comptime member emission, and mixin splicing all green. Steps
> 12–17 turn each into a full feature; the provable-leak + Try!
> end-to-end (step 17) sit on top.

---

## Risk register (cross-cutting)

| # | Risk | Affected | Mitigation |
| - | ---- | -------- | ---------- |
| R1 | **JIT symbol resolution** — `newbf_alloc`/`newbf_free`/`__newbf_ct_emit` are host-EXE Rust symbols, NOT PE exports; the process generator won't find them (verified: `from_ir` has only `DynamicLibrarySearchGeneratorForProcess`, no absolute symbols today). | memory-safety, comptime | **MS-T0 lands ORC absolute symbols + `add_absolute_symbol` + a smoke test BEFORE any rename** (no red window); CB reuses the same API. The shim is registered *before* the process generator so explicit definitions win (no duplicate-def error). |
| R2 | **`$header` ABI rework breaks dispatch/is-as verify-clean-but-wrong** — RF-T2 shifts every vtable slot; `corpus.rs` and the itable invariant harness CANNOT detect a physical slot-shift. | reflection (+ memory-safety delete path) | One `load_vtable_base` (struct-GEP field 1) routes all three dispatch sites; **a non-JIT `%ClassVData`-shape/field-1-GEP emission unit test is the only detector**; a NAMED run-corpus green list (is/as + virtual + iface + abstract); **RF-T2 sequenced before MS-T4** so delete runs against the final header. |
| R3 | **NEW runtime crate + NEW JIT-vs-AOT link change** — the stomp allocator is a new staticlib; AOT must link it + use `/ENTRY:newbf_entry`; the strip is by a runtime mode flag, not `cfg!(debug_assertions)`. | memory-safety | MS-T1 is runtime-only (`cargo test -p newbf-runtime`); MS-T3 wires JIT (symbols via MS-T0, no new harness dep) + AOT (staticlib + `/ENTRY`) with **one debug + one release AOT parity test**; mode flag decouples strip from the runtime's own build profile. |
| R4 | **NEW child-process test harness** — a fault/abort kills the process (SEH returns CONTINUE_SEARCH), so the value-checking run-corpus CANNOT observe a UAF/double-free in-process. | memory-safety | MS-T3 ships a **`guard_corpus` child-process runner** (spawn a runner exe per program, inspect exit code / WER); `newbf_guard_reset()` between programs; atexit leak report suppressed under the value harness. |
| R5 | **Verify-clean miscompiles** — guard ABI drift (array −8/+8 vs page-end), mixin escape-stack desync, ClassVData slot-shift all pass `corpus.rs` yet crash/misbehave at runtime. | all but reflection-policy | **Run-corpus is the authoritative gate** for MS-T2/T3, MX-T3/T4, RF-T2/T5; `AllocKind` (front-align Array/Raw, page-end Object) + ledger-keyed free (no −8); mixin **unconditional stack truncation to snapshot** + `mixin_stmts_after_escape.bf`; per-call arity asserts. |
| R6 | **Fixpoint non-termination / eager-link of surviving emitter externs** — a divergent emitter loops; a surviving `__newbf_ct_emit` extern fails `lookup("Program.Main")` in the app/run JIT (which doesn't register the shim). | comptime | Triple guard (normalized-text dedup + round cap + byte cap, diagnostic on trip); `run_emission` **strips emitter/shim before return**; CB-T4 asserts the final module **JIT-links AND AOT-links clean** (incl. a dead-emitted-member program). Internal-infinite-loop emitters hang (documented v1 boundary). |
| R7 | **`Mixins.bf` breaks the 100% clean-verify ratchet the moment expansion lands.** | mixins | Strict gate (MX-T2.5) — v1 expands only supported shapes; every unsupported shape falls back to the EXISTING verifiable path (no novel IR); **pre-MX-T3 acceptance = full verify corpus with expansion ON, 0 new failures on `Mixins.bf`**, shape-by-shape. |
| R8 | **Shared mutated surfaces across features** — `lower_program` (CB wraps it; MX-T8 changes its signature); the corpus fixtures (MS-T5.5 edits; every feature adds programs); `OrcJit::from_ir` (MS-T0 + CB-T2). | memory-safety, comptime, mixins | `add_absolute_symbol` is **one API** landed by MS-T0, reused by CB/MX-T8; **MX-T8 is staged** and must co-update CB's `run_emission` call (flagged in the schedule); **MS-T5.5 lands once, early**, before later feature programs. |
| R9 | **SSA dominance ("instruction does not dominate all uses")** — the MS scope-slot fix, the mixin block-yield slot, and RF's `LoadTypeId` all emit new value sequences. | memory-safety, mixins, reflection | MS scope fix uses per-site entry-block null-init slots (only slot-ptr + loaded-ptr-or-null cross blocks); mixin result slots are allocas + guarded loads; RF `typeof` is a constant `GlobalAddr`, `LoadTypeId` emits at the receiver use site (receiver dominates). Each gated by verify-corpus (the bug is a verifier failure). |
| R10 | **sema must NOT depend on newbf-llvm** (hard invariant). New edges: `newbf-llvm→newbf-runtime` (MS), `newbf-comptime→newbf-runtime` (MS-A6 phase bit). | all four | Verified clean (no cycle): runtime depends on nothing; comptime calls sema (legal); reflection adds **no** Rust runtime code (in-module LLVM accessor); mixins is pure sema. Each task's gate includes "no new `use newbf_llvm` in sema." |

**Tasks that need newbf-runtime / JIT-vs-AOT work (the riskier set):**
MS-T0, MS-T1, MS-T3, MS-T7 (runtime + link), MS-T4 (reads post-ABI
`$header`), CB-T2 (JIT absolute symbol), CB-T4 (JIT+AOT link-clean
assertion), MX-T8 (staged — runtime FatalError + link). **Pure
sema/parser (safer):** RF-T0/T1/T3, CB-T0/T1/T3/T6, all of MS Track B
(MS-T5/T5.5/T6), and **all of mixins MX-T1..T6**. Reflection's RF-T2/T4/T5
are sema+LLVM-emit but **no runtime/no link change** (in-module accessor)
— their risk is the ABI slot-shift (R2), not symbol resolution.

---

## Notes on what was *not* sequenced

- **Deferred open-questions** from each doc are out of this wave by
  design and carry no tasks here: virtual dtors through interfaces +
  closure-owned cleanup + per-JITDylib comptime teardown +
  `[AllowAppend]`/custom allocators + array forward-overrun page
  protection (memory-safety §10); float const-eval (`__real@` JIT gap) +
  `EmitAddInterface`/`EmitMixin` + generic emit generators + a reflection
  FFI table + bounded execution (comptime §9 staged/§10); `typeof`
  primitives/generic-T-params + value-type `GetType` runtime-null +
  interface Type metadata + Beef-exact `Type` ABI + AOT metadata
  verification + comptime reflection (reflection §10); generic/cross-file/
  lvalue-yield/lambda-in-body mixins + labeled-loop escape + `(.)err`
  cross-error cast + the `lower_program` diagnostic-sink signature change
  (mixins §10, staged as MX-T7/T8).
- **The natural next-wave merge points** are explicitly out of all four
  v1s: `[Comptime] typeof(T).GetFields()` (comptime × reflection),
  `EmitAddInterface` (comptime × itables), virtual-dtor-through-`$header`
  (memory-safety × reflection × itables), and `EmitMixin` (comptime ×
  mixins). Each becomes tractable once this wave's substrates (ClassVData,
  the emission fixpoint, the absolute-symbol seam, the splice machinery)
  exist.
