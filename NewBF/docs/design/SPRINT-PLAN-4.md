# NewBF — Cross-Feature Sprint Plan (Wave 4: generic-interfaces · iterators-lazy · comptime-metaprogramming-v2 · delegates-events)

*Drafted 2026-06-10. Sequences the agent-assignable tasks from the four Wave-4
design docs in [`docs/design/`](.) —
[`generic-interfaces.md`](generic-interfaces.md) (GI, the foundation),
[`iterators-lazy.md`](iterators-lazy.md) (IL),
[`comptime-metaprogramming-v2.md`](comptime-metaprogramming-v2.md) (CM),
[`delegates-events.md`](delegates-events.md) (DE) — into one schedule. Companion
to [`SPRINT-PLAN-3.md`](SPRINT-PLAN-3.md) (Wave 3, landed — `dcaf2d7` … `fc18a6d`),
[`SPRINT-PLAN-2.md`](SPRINT-PLAN-2.md) (Wave 2), [`SPRINT-PLAN.md`](SPRINT-PLAN.md)
(Wave 1), the 12-phase [`PLAN.md`](../../../PLAN.md), and
[`SPRINTS.md`](../../../SPRINTS.md). Modeled on SPRINT-PLAN-3's format + rigor.*

*Wave 4 differs structurally from Wave 3 in **one decisive way**: Wave 3 was four
**independent** tracks (the cross-feature couplings were all "data-shape ordering"
and "ratchet reconciliation," never a hard build-on dependency). Wave 4 has a
**real critical path** — generic-interface monomorphization (GI) is the **foundation**
that the **deferred** halves of three other features were waiting on. The sprint's
central job is therefore (a) to identify the **exact** dependency edges through GI,
(b) to ship GI's foundational tasks FIRST, and yet (c) to recognize that — verified
against all four docs and the tree — **every Wave-4 feature has a fully-decoupled v1
slice that ships WITHOUT GI**. The dependency edges through GI are all to **deferred,
explicitly-out-of-v1** halves. So GI is the critical path by **chain length and
downstream-value**, not because it gates any v1 slice. The dominant risks are two
genuinely-hard backend/transform lifts: **GI's env-driven itable fill at a minted
mono id** and **IL's lazy-yield cross-yield-spill state machine** — both verified
below as new code the codebase lacks, both gated only by the authoritative run-corpus
under the Stomp guard.*

## Preamble — cadence and invariants

Cadence is unchanged from Waves 2-3: **one developer, one agent at a time,
review/test/commit per task.** A "sprint" here is a *review batch* — a set of tasks
reviewed and merged as a group because they share a gate or co-land atomically.
"PARALLEL" means *no dependency forces an order* (interleave them however the review
queue prefers); "SERIAL" means they must land in the listed order.

**The standing gates, green at every task boundary:**

- **Parser corpus** — `newbf-parser/tests/corpus.rs`, a **100% ratchet**
  (`clean == files.len()`, verified `corpus.rs:79-84`; collects `feature-suite/src`
  + `corlib-slice`). Adding a feature-suite/corlib file raises the denominator.
  **Only DE touches the parser** (the `Member::Event` variant + contextual `event`
  keyword + `print.rs` round-trip), so the parser ratchet moves only in DE. **IL,
  CM, GI do NOT touch the parser** (IL's `yield` surface landed in Wave 3; GI/CM are
  pure sema/llvm).
- **Verify corpus** — `newbf-sema/tests/corpus.rs`, **162/162** LLVM-clean (the
  dynamic `clean == files.len()` ratchet, verified `corpus.rs:106-110`; lowers each
  `feature-suite/src` + `corlib-slice` file **standalone**, assertions ON). This is
  the ratchet GI must hold against — and it is GI's **dominant** risk: the moment GI
  monomorphizes generic interfaces, the existing corpus classes that implement them
  (`ClassD`/`ClassE` Interfaces.bf:229/247, `EnumeratorTest` Loops.bf:17,
  `IndexTest`/`IndexTestExplicit` Indexers.bf:96/112 — all **re-verified present**)
  get a real `iface_bases` entry that `apply_itables` must resolve completely **or
  `resolve_itable_impl` panics** (`lower.rs:1337`, a `debug_assert!(false)` LOUD in
  the assertions-on verify corpus). This is GI's R-A analogue of Wave 3's
  `%struct.Type` slot-shift: a verify-corpus *panic*, not a silent miscompile, but
  the same "the static corpus is the detector" posture.
- **Run corpus** — `tests/newbf-tests/tests/run_corpus.rs`, **264** programs
  (verified), JIT-run, full-i32 value check under the **Stomp memory guard**
  (`set_guard_mode(GuardMode::Stomp)` `:89`; the harness **never** calls
  `report_leaks` `:114`). **The authoritative behavioral gate.** Every Wave-4 feature
  produces at least one *verify-clean-but-wrong miscompile class* the static corpus
  cannot catch — a generic-interface dispatch through a wrong slot (release-silent,
  R10), a lazy-yield stale-state spill that loops forever, a delegate invoke-all
  arity drift (LLVM builds the indirect-call type from the *args*, so it is
  verify-clean, fn-values §1), a comptime double-emit — for those, run-corpus (or a
  deterministic dump-ir/layout pin) is the only real gate.

**8-bit exit-code caveat (MEMORY):** AOT exit codes truncate to 8 bits; all
value-checks use the JIT run-corpus harness, which reads the full i32. Keep AOT probe
values ≤255; corpus value checks run under the JIT harness anyway. All four features'
worked-example `expect:` values fit i32 and are ≤255.

**Debug-assertions caveat (GI-specific, R10):** GI's correctness nets — the itable
bounds keystone `debug_assert!(slot_base >= vimpls[i].len())` (`lower.rs:1268`) and
the unresolved-slot `debug_assert!(false)` (`lower.rs:1337`) — are `debug_assert!`:
**LOUD in debug, silent null-pad/null-slot in release.** Run the verify-corpus + all
GI dump-ir gates under the default **assertions-on `cargo test`** profile; the
run-corpus value check (`123`, not garbage) is the only release-active net. This is a
standing-gate amendment unique to GI.

**Naming convention.** Each task keeps its *home-doc id* with a feature prefix so it
is traceable: `GI-T2` = generic-interfaces task 2, `IL-T2b` = iterators-lazy task
2b, `CM-T1.5` = comptime-metaprogramming-v2 task 1.5, `DE-T3a` = delegates-events
task 3a. (The home docs use `T-PRE`/`T0…T6` for GI, `LT-T0…LT-T3` for IL,
`CMV2-T0…T5` for CM, `T0…T4` for DE; this plan normalizes IL's `LT-` and CM's
`CMV2-` to the `<PREFIX>-T<n>` form: `IL-T0` = LT-T0, `CM-T1.5` = CMV2-T1.5.)

**The landed substrate every Wave-4 feature stands on (verified):**

| Substrate (Wave 1-3) | Lives at | Wave-4 consumer |
| -------------------- | -------- | --------------- |
| Non-generic itables: `%ClassVData` header, `apply_itables`, `emit_iface_dispatch`, `resolve_itable_impl` + `itable_abi_matches` ABI gate | `lower.rs:1238`/`:11878`/`:1337`/`:1353` (id-keyed, generic-agnostic) | **GI extends it to mono iface ids with ZERO `newbf-llvm` change** |
| Monomorphization: `index_generic_decls` `(name, arity)` keying (member-blind), `record_inst`/`register_mono`/`mangle_generic` | `lower.rs:716`/`:1745`/`:907`/`:13126` (**excludes interfaces** at `:737`) | GI lifts the `:737` exclusion; IL/DE ride the value-struct/value-class mono path unchanged |
| Eager `yield` + 5th `foreach` branch (GetEnumerator/MoveNext/Current/Dispose), `ScopeAlloc::DisposeHook`, value-struct `this`-aliasing | `rewrite_generators` `:5444`; 5th branch `:7619-7747`; DisposeHook `:7003-7015` | **IL adds the gated LAZY path**; DE reuses the spill/DisposeHook patterns |
| comptime emission: EmitTypeBody `Ref(String)` path, `__newbf_ct_emit`, `run_emission` sandbox + strip, the FieldInfo sandbox pin | `lower.rs:10598-10711`; `emit.rs` (`from_ir` sandbox, `:909` FieldInfo pin) | **CM reads MethodInfo/AttributeInfo in the sandbox** (no new IR) |
| Reflection + custom-attributes metadata: `MethodInfo`/`AttributeInfo` arrays emitted into **every** module incl. the sandbox clone; corlib `Type`/`MethodInfo`/`AttributeInfo` accessors | `newbf-llvm/lower.rs:515-539`/`:551-612`; `Type.bf`/`MethodInfo.bf`/`AttributeInfo.bf` | **CM's entire read-side; one sema filter (CM-T1.5) is the only lowering edit** |
| Function values: `$Func` two-word `{code,target}` value struct at `StructId(0)`, one uniform `call_indirect(code,[target,args…])` shape, `$mref$`/`$mrefb$` thunks | `register_func_struct` `:863`; invoke shape `:8368-8392` | **DE's `Multicast` holds N `$Func`s; invoke-all reuses the shape** |
| `alloc_array` real-`size_of_ty` block sizing + explicit `Struct(...)` elem stride | `lower.rs:10461-10462`; elem stride at each `elem_addr` | **DE's bespoke 16-byte `$Func` buffer** (NOT `List<$Func>`'s 8-byte stride) |
| def-graph `TypeIndex` keyed `(name, arity)` (generic interfaces keyed `("IFaceD",1)` **today**) | `constraints.rs:239`; `transitive_reaches`/`resolve_base` `:954`/`:981` | **GI's Seam G constraint enforcement rides it, independent of the itable lift** |

Everything Wave 4 needs already exists and is green. The HARD invariant (sema ⊥ llvm;
`IrType: Copy`; `StructTable` owns its data) is preserved by all four (each home doc
§2 re-verifies it). **GI, IL, CM all add ZERO `newbf-llvm` change and ZERO new IR
instruction** (re-verified per doc); **DE adds ZERO `newbf-llvm`/`newbf-ir`/runtime
change** (`Multicast`/`event` lower to existing `call`/`call_indirect`/`field_addr`/
`alloc_array`). There is **no new cross-crate Rust edge** anywhere in the wave.

**Where the risk concentrates (read this first):**

| Feature | New IR instr? | New `newbf-llvm`? | Parser? | Sandbox/comptime? | Headline risk |
| ------- | ------------- | ----------------- | ------- | ----------------- | ------------- |
| generic-interfaces (GI) | **no** | **no** | no | no | **the env-driven `imethods` fill at a minted mono id** (R1 keystone) + **the existing-corpus ratchet panic** (R-A/R7) |
| iterators-lazy (IL) | no (reuses Switch→cmp/cond_br) | no | no (yield landed W3) | no (uses the splice/reparse *parser*, never comptime eval) | **the cross-yield-spill + induction + resume-switch synthesis** (the liveness transform the codebase lacks) |
| comptime-metaprog-v2 (CM) | **no** | **no** | no | **YES — the spine** (MethodInfo/AttributeInfo value-struct return inside `$ct_emit_run`) | the **one** real sema edit (CM-T1.5, comptime-method exclusion) + the net-new 10-field-`Type` sandbox pin (CM-T1) |
| delegates-events (DE) | no (reuses `call_indirect`) | no | **YES** (`Member::Event` + contextual `event`) | no | **invoke-all arity drift** (verify-clean, fn-values §1) + the 16-byte-buffer stride trap |

The two genuinely load-bearing items the prompt flags: **IL's lazy-yield state
machine** (the cross-yield-local spill + range-induction synthesis + resume-switch —
a liveness transform the codebase has never had) and **GI's generic-interface itable
lift** (the env-driven `imethods` fill at a minted id + the dual mono-link routing,
the C′ map-routing trap all three GI reviews flagged). The risk register (§4) and the
schedule (§5) de-risk each by isolating it as a single starred task gated on a
hand-built IR-shape pin BEFORE the run-corpus value depends on it.

---

## Completeness-critic addendum (corrections to apply at execution — verified against the tree)

*A wave-level completeness critic reviewed all five artifacts after synthesis (Ultracode
quality pass). Verdict: **NEEDS-RESEQUENCING, narrowly** — the architecture is sound (no
repr conflicts, no hidden v1→GI dependency, both hard tasks honestly scoped), but three
corrections must be applied during execution. These OVERRIDE the corresponding rows below
where they conflict.*

- **[F1, BLOCKER] CM-T1.5 has an ordering inversion — fix it before executing the CM
  method axis.** R5 / the CM-T1.5 row say "build a `HashSet` from `m.comptime` inside
  `assign_type_ids_and_meta`." **This is a silent no-op:** `assign_type_ids_and_meta`
  runs at `lower.rs:5574`, but `m.comptime` is not populated until `lower_items`
  (`m.comptime.push`, `lower.rs:6304`) which runs *after*, at `:5583` — **independently
  re-verified.** At `:5574` the set is empty, so every `[Comptime]` method is still
  counted, and the chosen control (`reflect_method_count.bf → 2`, generator-free) cannot
  detect it → the inflation ships latent. **CORRECTED CM-T1.5:** source the
  comptime-method name-set from a **pre-pass over `all`** (the `Vec<SourceFile>` in scope
  at `:5546`) using `has_comptime_attr`, collect `full_name`s into a `HashSet<String>`,
  and thread it as a new parameter into `assign_type_ids_and_meta` — BEFORE `:5574`.
  **Harden the gate:** the unit test must assert a `[Reflect(.Methods)]` class **carrying
  a generator** counts non-comptime methods only.
- **[F2, HIGH] GI-PRE must explicitly resolve `EnumeratorTest : IEnumerator<int32>`, not
  silently carve it out.** "Narrow the v1 trigger so no class panics" is **circular** for
  `EnumeratorTest` (Loops.bf:17): its `MoveNext`/`Current` are genuine method slots (not
  the empty-`imethods` property/indexer-safe case), and `IEnumerator<int32>` is the very
  interface GI exists to deliver. **GI-PRE's acceptance MUST settle it:** either (a) prove
  its `MoveNext`/`Current`/`Dispose` resolve to ABI-matching impls (the likely-good case —
  it declares them concretely → make it the *headline existing-corpus proof* of GI), or
  (b) if it genuinely cannot, document that the v1-narrowing weakens the foundation thesis
  and flag for re-scope. Do this in GI-PRE, not at GI-T2's verify run.
- **[F5, LOW] GI is reschedulable behind the other three spines.** Every v1 slice is
  GI-independent (§dependency-analysis.1); if GI-PRE surfaces an intractable
  existing-corpus panic, GI may be deferred to last with **zero loss to the wave's
  shippable surface**. "GI-T0 first" is a value/risk preference, not a hard gate.
- **[F3, MEDIUM] Keep IL-T1 and IL-T2b as SEPARATE diffs** — IL-T1's hand-written
  `switch(mState){…}<shared if>` enumerator is the only proof the resume-IR pattern works
  before the risky synthesis depends on it; do not merge it into IL-T2b.
- **[F4/F6, hardenings] Make CM-T1's 10-field-`Type` and DE-T0's 16-byte-`$Func` layout
  pins assert against a programmatically-derived layout** (`size_of_ty` / the
  `emit_metadata` field offsets), not a hard-coded shape. If F1 proves sticky, CM may ship
  **attribute-axis-only** (CM-T3, zero lowering change) as the honest smaller v1.

---

## Cross-feature dependency analysis (the REAL edges through generic-interfaces)

This is the section Wave 3 did not need. Each feature is internally a near-linear
chain; the cross-feature couplings — verified against all four docs **and** the tree
— are:

### 1. There is exactly ONE foundation feature, and its v1 blocks NOTHING in the other three v1s.

GI is the foundation **for the deferred halves**. But the precise, re-verified status
(each dependent's own §7 + GI §7) is that **every other feature's v1 is decoupled**:

| Dependent v1 slice | Needs GI? | Why (verified) |
| ------------------ | --------- | -------------- |
| **IL — lazy `[Coroutine]` yield** (the whole v1) | **NO** | The synthesized `__GenN<E>` is a **concrete monomorphized generic VALUE struct** (`StructKind::Value`) resolved **statically by name** on the concrete type by the unchanged 5th `foreach` branch — no interface type, no dynamic dispatch. The mono-index exclusion (`lower.rs:737`, interfaces-only) **never touches a value struct** (IL §7, GI §7). |
| **CM — method/attribute reflection** (the whole v1) | **NO** | Reads **value-struct metadata** (`MethodInfo`/`AttributeInfo` over `.rodata`) through `typeof(T)` (a constant `GlobalAddr`) — no itable, no `emit_iface_dispatch`. CM §7: "a **leaf** in the wave-4 dependency graph — neither blocks nor is blocked by generic-interface monomorphization." |
| **DE — concrete multicast + event** (the whole v1) | **NO** | `$Func` is signature-agnostic (one 16-byte layout); the buffer is a concrete `alloc_array`'d block; invoke reuses `call_indirect`. No interface dispatch anywhere (DE §7). |

So the prompt's hypothesis — "iterators-lazy (IEnumerable<T>), delegates-events
(generic delegates), comptime-v2 (generic-T) **may** depend on GI" — resolves to:
**each of those is the DEFERRED half, explicitly out of the respective v1.** The
**real** edges (below) are all GI → a deferred follow-on, never GI → a v1 slice.

### 2. The REAL edges (GI → deferred follow-ons, not v1 tasks).

These are the dependency edges the prompt asks for. **None of them is a Wave-4 v1
task** — they are recorded so the sequencer knows what GI *unblocks for next wave*:

- **GI (the mono iface id + itable) → IL's interface-typed `IEnumerator<T>`/
  `IEnumerable<T>` half.** IL §7: `foreach` over an `IEnumerable<T>`-typed value
  needs (a) the lifted `:737` exclusion so `IEnumerator<int32>` resolves to
  `Ref(mono_iface_id)`, (b) `collect_iface_own_type`/`collect_iface_bases_type` to
  handle generic interface methods (both gated `generic_params.is_empty()` today),
  (c) an `IEnumerator`/`IEnumerable` pair **in corlib** (verified: zero matches in
  `newbf-corlib`), (d) the 5th branch dispatching through `emit_iface_dispatch`.
  **GI delivers (a)+(b); (c)+(d) remain.** Deferred, separate work item.
- **GI (Seam G, the `GenericInterface` constraint kind) → generic-constraints'
  `T : IEnumerator<TElement>`** (the Wave-3 GC §5 deferral). GI §7 + Seam G: this
  rides the def-graph `TypeIndex` (keys generic interfaces `(name,arity)` **today**,
  `constraints.rs:239`), so **GI's Seam G itself delivers the enforcement diagnostic
  WITHOUT the itable lift** — it is GI-internal task **GI-T5**, independent of
  GI-T0…T4. The GC §5 deferral is lifted by GI-T5.
- **GI → DE's full upstream `Event<T> where T : Delegate`.** DE §7: its
  `Enumerator : IEnumerator<T>` is the exact excluded construct; also needs
  `rettype(T)`/`params T`/`as List<T>`/bit-packed `mData`. Deferred (each its own
  feature). **DE's v1 concrete event needs none of it.**
- **GI → DE's / IL's generic-T variants are NOT GI edges.** DE's **generic
  delegates** (`Action<T>`) need a *delegate monomorph path* (mirror
  `record_inst`/`register_mono`) but **NOT** GI — `$Func` is signature-agnostic
  (DE §7, verified). CM's **generic-T reflection** is blocked by the
  `record_method_inst` `[Comptime]`+generic guard (`lower.rs:1851-1857`) +
  `typeof(generic-T)`, **NOT** by GI (CM §7, explicit). These are recorded so the
  sequencer does **not** mis-attribute them to GI.

### 3. What decouples (the four parallel v1 spines).

Because every v1 is GI-independent, the four features are **four parallel spines**
with **no inter-feature ordering force at the v1 level**:

- **GI** internal chain: `GI-T0 → GI-T1 → GI-T2★ → GI-T3 → GI-T4`, with `GI-PRE`
  (analysis) gating the scope and `GI-T5` (Seam G) **independent** of T0-T4 (rides
  the def-graph).
- **IL** internal chain: `IL-T0 → IL-T1 → IL-T2a → IL-T2b★ → IL-T3` (strict).
- **CM** internal chain: `CM-T0 → CM-T1★ → CM-T1.5 → {CM-T2 ∥ CM-T3} → CM-T5`
  (CM-T3 can start at CM-T1; CM-T2 needs CM-T1.5; CM-T4 off-path).
- **DE** internal chain: `DE-T0 → {DE-T1 ∥ DE-T2} → {DE-T3a → DE-T3b} → DE-T4`.

The **only** cross-feature ordering forces are soft/confounding-avoidance, not hard
deps:

- **GI vs the verify ratchet (a hard *scope* constraint, not an ordering edge).**
  GI-PRE MUST complete (and bound the v1 trigger so no existing corpus class panics
  `resolve_itable_impl`) before GI-T2 lands — this is intra-GI, but it interacts with
  the shared 162/162 gate every other feature also depends on. If GI-T2 regresses the
  verify corpus, **all four features' boundaries go red** until fixed. ⇒ GI's
  ratchet-touching tasks (T2/T3) should land in **review batches that do not also
  carry another feature's first behavior-changing task**, so a verify regression's
  signal is not confounded (the Wave-3 "force #4" pattern, generalized).
- **DE is the only parser-ratchet mover** (DE-T2). Land it in a batch where no other
  feature also moves a ratchet, so a parser-corpus regression is unambiguous.
- **No feature changes `lower_program`'s signature** (re-confirmed: GI is sema-internal;
  IL's `rewrite_generators` runs *inside* `lower_program` without changing it; CM's
  CM-T1.5 reads `&mut Module` already in `assign_type_ids_and_meta`; DE adds a
  `StructTable` pass + `assign` interception). The Wave-2 staging concern stays retired.

### 4. Net ordering forces (what the reviewer MUST respect).

1. **GI-PRE first within GI** — the analysis that bounds T0-T2's scope so no existing
   corpus class panics. It is the ratchet-keystone; it pairs with T0/T1/T2 acceptance
   (run the verify corpus after each).
2. **GI's foundational chain (T0→T1→T2) is the wave's critical path** (§3) — start it
   first, because it is the longest chain × the deepest single edit (T2's env-driven
   fill) × the broadest downstream value (unblocks three next-wave follow-ons).
3. **GI-T5 (Seam G) is independent** of GI-T0…T4 and may land first / in parallel —
   it rides the def-graph, not the itable.
4. **The other three spines are fully parallel** to GI and to each other at the v1
   level; sequence them by reviewer bandwidth, respecting only the soft batch-isolation
   of GI's ratchet tasks (force §2 above) and DE's parser-ratchet task.
5. **Within each spine the internal chain is strict** (IL especially: T2a's edit
   machinery must precede T2b's risky loop synthesis).

---

## The critical path

The longest dependency chain — and the one gating the most novel downstream value
(it unblocks **three** next-wave follow-ons: IL's interface-typed enumerators, GC's
`T : IEnumerator<TElement>`, DE's full `Event<T>`) — runs through
**generic-interfaces**, because it is the foundation feature and its keystone (GI-T2)
is the wave's deepest single edit:

```
GI-PRE ─(bounds scope)─┐
                       ▼
GI-T0 ──→ GI-T1 ──→ GI-T2★ ──→ GI-T3 ──→ GI-T4
(lift     (Seam B   (env-fill   (mono-    (distinct
 :737      td.bases  imethods    iface     args +
 mono-     mono      at minted   dispatch  is/as,
 index     request)  id + dual   + 5 run-  Seam F′)
 excl.)              link C/C′)  corpus)
                       │
GI-T5 (Seam G constraint enforce) ── INDEPENDENT (rides def-graph) ──→ GI-T6 (docs)
```

- **GI critical sub-chain = GI-T0 → GI-T1 → GI-T2 → GI-T3 → GI-T4** (5 serial nodes
  after GI-PRE bounds the scope; GI-T5 branches off the def-graph independently).
  GI-T0 (lift the `:737` exclusion) is the **first domino** — nothing downstream fires
  without it (`record_inst` bails at its `else { return; }` until the interface is in
  `GenericDecls`). GI-T2 is the **keystone and the wave's hardest backend edit**: the
  env-driven `imethods` fill at a minted mono id (resolving `T → i16`, `this =
  Ref(mono_id)` not `Ref(template_id)`) PLUS the dual mono-link routing (Seam C into
  the `imethods`-flatten map AND Seam C′ into the **class-routing** `iface_links` map
  — the separate-object trap all three GI reviews flagged). A wrong `this` id or
  unresolved `T` desyncs the ABI gate `itable_abi_matches` (`:1353`) →
  `resolve_itable_impl`'s terminal `debug_assert!(false)` (`:1337`) panic.

> **CRITICAL PATH: GI-PRE → GI-T0 ★ → GI-T1 ★ → GI-T2 ★★ → GI-T3 ★ → GI-T4 ★**
> (GI-T5 a def-graph-independent side-branch; GI-T6 the tail.)

The other three features hang off this spine with **full slack** (their v1s are
GI-independent, §dependency-analysis):
- **IL** (`IL-T0 → IL-T1 → IL-T2a → IL-T2b★ → IL-T3`, 5 serial) is the *second-hardest*
  spine — IL-T2b (the cross-yield spill + range-induction synthesis) is the wave's
  other genuinely-hard task (the prompt's named pair-mate to GI-T2). Interleave it
  against GI's slack.
- **CM** (`CM-T0 → CM-T1★ → CM-T1.5 → {CM-T2 ∥ CM-T3} → CM-T5`) is the lowest-backend-
  risk spine (one sema filter, the rest is sandbox pins + run-corpus programs over
  landed machinery).
- **DE** (`DE-T0 → {DE-T1 ∥ DE-T2} → {DE-T3a → DE-T3b} → DE-T4`, 6 tasks) is the only
  parser-ratchet mover; DE-T3a (invoke-all) is its riskiest node.

**The single most-unblocking task is `GI-T0`** — lift the `td.kind !=
TypeKind::Interface` conjunct at `lower.rs:737` (after GI-PRE has bounded the scope so
the existing corpus stays panic-free). It is the root of the longest chain, it is the
first domino (every other GI task is inert until a generic interface enters
`GenericDecls`), and it is cheap (one conjunct removed) but **scope-critical** — doing
GI-PRE then GI-T0 first, with the verify corpus re-run as the gate, de-risks the whole
GI chain (and the three next-wave follow-ons it unblocks) before any run-corpus
program depends on it.

*(Contrast with Wave 3, where the most-unblocking task was the longest-chain
*keystone* CR-T0 because the shared seams were already landed and the couplings were
soft. In Wave 4 the most-unblocking task is the foundation's *first domino* GI-T0 —
same role (de-risk the spine first) but now because there is a **real** foundation,
not just a long chain.)*

---

## Per-feature risk table

| Feature | backend (llvm) | runtime/guard | itable/ABI | sandbox/comptime | SSA | ratchet (verify/parser/run) |
| ------- | -------------- | ------------- | ---------- | ---------------- | --- | --------------------------- |
| GI | none (no IR change; id-keyed dispatch reused) | dispatch is `call_indirect` through an existing vtable global; no heap | **the headline** — env-driven `imethods` fill + dual link routing; ABI gate desync → `:1337` panic | none | trivial (inline single-block dispatch dominates, `:11900-11914`) | **HIGH** — existing corpus impls panic `resolve_itable_impl` if the flip leaves an unresolved slot (R-A/R7); debug-gated nets (R10) |
| IL | none (Switch → existing cmp/cond_br) | value-struct enumerator under Stomp; larger mutated field set than `ListEnumerator` | none (concrete value struct) | none (splice/reparse uses the *parser*, never comptime eval) | **the headline** — every cross-yield value MUST be a `__GenN` field (loaded fresh), never an SSA reg across resume | MED — gated behind `[Coroutine]`; 3 eager + 5 foreach + `enum_manual` byte-identical; EOF-append is first-of-kind |
| CM | none | sandbox `new String` double-free faults the **compiler** under Stomp | none (reads metadata, doesn't change it) | **the spine** — `MethodInfo`/`AttributeInfo` value-struct return inside `$ct_emit_run`; CM-T1 attr half is net-new 10-field-`Type` scaffolding | straight-line at the use site | MED — CM-T1.5 is one real sema filter (verify gate); `reflect_method_count.bf → 2` control must stay green |
| DE | none (reuses `call_indirect`/`alloc_array`/`field_addr`) | bespoke 16-byte `$Func` buffer; DisposeHook free-once (scope) / documented field leak | `Multicast` layout pinned `{Ptr,i64,i64}`; 16-byte stride at **every** `elem_addr` | none | invoke-all `$Func` **spill** to alloca (Get returns by value) | MED — **moves the parser ratchet** (contextual `event`); invoke-all arity drift is verify-clean (R-DE1) |

## Risk register (cross-cutting, numbered with mitigations)

| # | Risk | Affected | Mitigation |
| - | ---- | -------- | ---------- |
| **R1** | **GI's env-driven `imethods` fill at a minted mono id (the deepest correctness point of the wave).** A wrong `this` id (`Ref(template_id)` not `Ref(mono_id)`) or an unresolved `T` (`ret = Ptr`/`Ref(template)`) desyncs the ABI gate `itable_abi_matches` (`:1353`) → `resolve_itable_impl` terminal `debug_assert!(false)` (`:1337`) → LOUD verify-corpus panic. | GI | The Seam-C helper threads the mono `env` into `lower_value_ty`/`param_ir_ty`/`pointer_elem_env` (so `T → i16`) and passes `vec![IrType::Ref(mono_id)]` as the leading `this`. **GI-T2's dump-ir gate** asserts `imethods[IFaceX$i16] == [("GetVal", sig)]` on a **clean inline fixture** (NOT feature-suite `IFaceD`, which carries deferred method-generics/statics/extension) with `sig.ret == i16` and `sig.params[0] == Ref(IFaceX$i16)` BEFORE any dispatch task. The run-corpus value (`123`, not garbage) is the behavioral net. |
| **R2 / R-A** | **GI ratchet panic over the EXISTING corpus generic-interface impls (the DOMINANT GI risk — the R-A analogue).** Once GI-T0…T2 monomorphize generic interfaces, `ClassD`/`ClassE` (Interfaces.bf:229/247), `EnumeratorTest` (Loops.bf:17), `IndexTest`/`IndexTestExplicit` (Indexers.bf:96/112) each get a real `iface_bases` entry `apply_itables` must resolve completely or `resolve_itable_impl` panics (`:1337`) → verify 162/162 regresses. The static corpus cannot detect this in advance — only running it post-flip does. | GI (the whole chain) | **GI-PRE (§3.0) is the gating analysis**: enumerate every such class, classify each as (a) method-only/resolves, (b) property/indexer-shaped (empty `imethods`, safe — `IIndexable<float>` walks `Member::Method` only), or (c) a genuine gap → **narrow the v1 trigger** (defer that shape) so no class panics. The §3.9 deferred-feature paths (`GetVal2`/`IDAdd`/`SGet`) must stay verify-clean under the flip. Run the verify corpus after EACH of T0/T1/T2. `Interfaces.bf` is **verify-only** (its `Test.Assert` is never JIT-run) — the behavioral proof is the new `generic_iface_*.bf` run-corpus programs. |
| **R3** | **IL cross-yield spill + induction synthesis = the SSA-dominance trap (THE hard IL part).** A lazy `MoveNext` resumes *after* the last yield; every value live across a yield is the classic "instruction does not dominate all uses." | IL | The transform **spills every captured arg / cross-yield-live local / synthesized induction slot into a `__GenN` field**; the synthesized `MoveNext` reads/writes those fields, so the value lives in the value-struct body (loaded fresh via `field_addr`/`load`), **never in an SSA reg across the resume**. The synthesized body is ordinary source → `lower_method`'s alloca-everything codegen (`:6661-6666`) handles in-method SSA. The genuine difficulty is the **liveness/induction analysis** (IL §3.1 step 3), bounded to the feasible shapes. `lazy_loop.bf → 6` + `lazy_take_infinite.bf → 10` (with a `taken == 4` cross-check killing off-by-state-number aliasing) pin it under Stomp. **IL-T1 pins a HAND-WRITTEN `switch(mState){…} <shared if>` enumerator BEFORE IL-T2b synthesizes it** — the resume-IR pattern is proven before the synthesis depends on it. |
| **R4** | **IL takes the WRONG `foreach` branch if the return type is not rewritten.** If `Gen()` still declared `List<E>`, `coll_ty = Ref(list_id)` → the 4th (Count/Get) branch fires (`:7542-7606`) and calls `List.Count/Get` on a `__GenN` value (miscompile/Stomp fault); the 5th branch never enters. | IL | IL §2.4/§3.1 step 4: rewrite the return type to `__GenN<E>` (a `Struct`, no Count/Get) via edit (ii), so the 4th branch's `if let IrType::Ref` guard (`:7543`) fails and the 5th fires. `lazy_straightline.bf → 6` (the smallest case) catches a regression under the guard. **The return-type rewrite is the mechanism, not an incidental detail.** |
| **R5** | **CM-T1.5 — comptime methods inflate the reflected method count (the central CM review finding).** A `[Comptime, EmitGenerator]` generator is an ordinary body-having method recorded into `structs.methods[id]` (`:3335-3337`) and thence `MethodMeta` (`:5153-5168`) with NO filter — so `GetMethodCount()` on a generator-bearing class is off by one+, and an emitted method member makes it non-convergent. | CM | **CM-T1.5** (the one v1 lowering edit): build a `HashSet<&str>` from `m.comptime` (the `Vec<String>` of `[Comptime]` `full_name`s, populated `:6304`) and `continue` over any `sig` in it before pushing `MethodMeta` at `:5155-5162` (no `MethodSig` change; `&mut Module` already in scope). Gate: a unit test asserts a `[Reflect(.Methods)]` class **carrying** a generator counts only non-comptime methods; `reflect_method_count.bf → 2` (the generator-free control) stays green. The §4.1/§4.2 examples ALSO emit onto a **separate probe** so no emitted member re-enters the counted set (R6). |
| **R6** | **CM reflecting emitted-this-round members (only PARTIALLY solved by CM-T1.5).** CM-T1.5 removes the *generator*, but an emitted **non-comptime method** re-enters `type_meta` at round k+1 → a same-set method-count read shifts → non-convergence. | CM | v1 examples emit onto a **separate probe class** (§4.1/§4.2), so the reflected count is over a generator-free, emission-free type. A *general* same-set emit-and-reflect is **deferred** (CM §5). The attribute axis is structurally immune (an emitted member adds no attribute bracket → the attribute set is invariant). |
| **R7** | **CM value-struct method-chain trap (highest-probability CM bug).** `GetMethod(i)`/`GetCustomAttribute(i)` return `MethodInfo`/`AttributeInfo` **by value** (`Struct(id)` rvalue); `struct_base` (`:10280-10307`) accepts `Struct` only via its lvalue arm — a method-call-result rvalue falls to the rvalue arm (`Ref` only) → undef receiver. | CM | The established pattern: bind `MethodInfo m = …; m.GetName()` / `AttributeInfo a = …; a.GetIntArg(0)` in **both** generator code AND emitted runtime text. Pinned by the existing corpus (`reflect_method_count.bf:18`, `attr_int_arg.bf:23`) + every §4 example. **Not new risk** — a known discipline to spell into each example. |
| **R8** | **CM-T1 sandbox pin — the attribute half is NET-NEW scaffolding, NOT a FieldInfo mirror.** The FieldInfo precedent (`emit.rs:909`) hand-builds an **8-field** `Type` via `TypeMeta::new` (which hard-codes `attributes: Vec::new()`, `module.rs:169`) indexing only `mFields`@6. | CM | CM-T1 method half mirrors the precedent (index `mMethods`@7, already a field); the **attribute half must** (a) extend the hand-built `Type` to the full **10-field** layout (`mAttrCount`@8/`mAttributes`@9, matching `emit_metadata` `:413-428`), (b) build `TypeMeta` **struct-literally** (not `::new`) with a non-empty `attributes`, (c) index `mAttributes`@9, (d) rely on `emit_metadata` synthesizing `.attrinfo`. **Risk raised** vs the original "symmetric" framing; CM-T1 is CM's riskiest task. |
| **R-DE1** | **DE invoke-all arity drift is verify-clean (the dominant `$Func` failure mode).** LLVM builds the indirect-call type from the *args*, so an arity/type drift in the invoke loop passes verify and only the run-corpus catches it (fn-values §1). | DE | The per-entry `call_indirect` copies the proven single-target shape (`:8368-8392`) **including** its `debug_assert_eq!(call_args.len(), ptys.len()+1)` arity guard (`:8385`). `event_multicast_two.bf → 30`, `event_add_then_invoke_arg.bf → 25` pin it under Stomp; run-corpus is authoritative. |
| **R-DE2** | **`List<$Func>` truncation (the DE representation keystone).** `List<T>` hardcodes an 8-byte slot stride (`List.bf:16,140`), so a 16-byte `$Func` element aliases/overruns (a Stomp out-of-bounds fault). | DE | The backing store is a **bespoke `$Func*` buffer via `alloc_array`** (block **sized** by real `size_of_ty($Func)` = 16, `:10461-10462`) **indexed with an explicit `Struct(func_struct)` element** at **every** `elem_addr` (the 16-byte stride). NEVER `List<$Func>`, never a bare-`Ptr` element. The `Multicast` layout unit test (`{Ptr,i64,i64}`) + `mcast_manual.bf`/`event_multicast_two.bf` (two entries, no aliasing) under Stomp pin it. |
| **R-DE3** | **DE `event`-as-keyword breaks the parser ratchet day one.** `event` is a parameter identifier in `corlib-slice/Platform.bf` (lines 391-433) + `Event.bf` (line 232), both in the 100%-clean parser corpus — reserving it globally is an instant ratchet break. | DE | `event` is a **contextual** keyword recognized only in `member()` via `at_ident_text` (the `get`/`set`/`not` precedent, `parser.rs:114-119,278,3760`); stays a normal `Ident` everywhere else. No `Keyword::Event`. DE-T2's acceptance checks `Platform.bf`/`Event.bf` still parse clean. |
| **R-DE4** | **DE value-struct field/local dtors are NEVER chained (the buffer-free reality).** `emit_destroy` (`:11067-11089`) walks only the class inheritance chain; scope cleanup is `Ref`-only (`:8520`). A `Multicast` field in a heap class never gets `~this()`; a by-value copy that later freed `mItems` would **double-free** → Stomp abort. | DE | v1 makes **no** "exactly-once free via `~this()`" claim (that `~this()` is dead code). `scope`-local events free via the existing **`ScopeAlloc::DisposeHook`** (`:7003-7015`) — the only guard-relevant free; `event` **fields** of heap classes **leak the buffer** (documented, benign — the guard tolerates a leak, never `report_leaks`). All `Multicast` methods take `this` **by address** + an event is never loaded by value → no copy → no double-free. `event_scope_dispose.bf → 0` pins the free-once path. Field-dtor chaining is **deferred** (DE §5). |
| **R-DE5** | **DE no `StructTable`→delegate-sig data path.** The def-graph `DelegateSig` (`model.rs:138`) is unreachable from lowering (`lower_program` ignores `_program` `:5466`; `lower_value_ty`/`fn_sigs` see only `&StructTable`); a delegate name never enters `StructTable` (`register_struct_names` matches only `Item::Type`). | DE | **DE-T1 adds a new `StructTable` pass**: `delegate_sigs: HashMap<String,(IrType,Vec<IrType>)>` populated from arity-0 `Item::Delegate` AST in `StructTable::build`; `lower_value_ty` + both `fn_sigs` sites consult it (§3.2). `delegate_concrete_call.bf → 12` pins it. Generic delegates (arity>0) are NOT registered → deferred. |
| **R9** | **GI `is`/`as` against a generic RHS needs NEW code (not confirm-only).** `type_id_of` (`:11521`) resolves only `Expr::Ident`/`Expr::Paren` → a generic RHS (`Expr::Generic`, `ast.rs:307`) returns `None` and `is`/`as` fall to `false`/`null`. | GI | Seam F′ (GI-T4) adds the `Expr::Generic` arm (lower each arg via `lower_ty_env`, mangle via `mangle_generic`, look up `by_name` for the mono id). `generic_iface_is_as.bf → 1` gates it. |
| **R10** | **GI's dump-ir/assert nets are debug-gated.** The bounds keystone (`:1268`) and unresolved-slot assert (`:1337`) are `debug_assert!` — loud in debug, silent null-pad/null-slot in release. | GI | Run the verify corpus + all GI dump-ir gates under the **assertions-on `cargo test`** default profile; the run-corpus value check (`123`, not garbage) is the only release-active net. (Standing-gate amendment, Preamble.) |
| **R11** | **IL/DE first-of-kind on the executable path under Stomp.** IL: a SYNTHESIZED generic value struct with a larger mixed-type mutated field set + the EOF-append edit (grows the item list — a new code path). DE: a bespoke 16-byte-strided heap buffer of value structs. | IL, DE | IL: `ListEnumerator<T>` (`enum_manual.bf → 6`) proves the generic-value-struct ABI; the field count grows but the ABI is inherited — `lazy_loop.bf` (multiple mutated fields) pins it; **IL-T2a pins a unit test that the appended `__GenN` re-parses to an `Item::Type` and lands in `index_generic_decls`**. DE: `mcast_manual.bf → 30` (T1-independent, existing `function` subscribers) proves the buffer in isolation BEFORE events layer on. |
| **R12** | **IL/DE walker audit — the compiler does NOT enforce wildcard walks.** IL's `yield` arms landed in W3 (verified present: `collect_insts_stmt :2229`, `for_each_stmt_expr :3825`, etc.) — **no new IL walker work**. DE's new `Member::Event` forces only the two exhaustive walks (`Member::span()`/`print.rs::member`); every other member-walk is wildcard. | DE (IL clear) | DE-T2 hand-edits the member-registration walks (`register_type_struct` / `build.rs` member loop) to synthesize the backing `Multicast` field + record `(owner,name)` in the synthesized-event set, AND ships a focused "event-registers-a-field" test (the wildcard skip would otherwise drop the event silently). |
| **R13** | **CM sandbox String double-free faults the COMPILER (not a leak).** The generator's `new String` body routes through `newbf_alloc` → the Stomp ledger *during compilation* (run-corpus under Stomp). A double-free faults the compiler; a pure leak does not abort. | CM | Every §4 generator `delete`s exactly once; acceptance pins **"no double-free under Stomp"**, NOT "allocations balance" (the harness never calls `report_leaks`). The `char8*` buffer is CRT malloc/free, invisible to the guard. Identical to the landed CR-T3/T4 — no new hazard. |
| **R14** | **sema ⊥ llvm HARD invariant.** | all four | Verified per home doc §2: GI is pure sema (+`constraints.rs`), `newbf-llvm`/`newbf-ir`/parser/runtime untouched, names only the mono iface mangle + impl symbols; IL is a pure-sema source rewrite emitting IR shapes the eager path/5th branch/generic value structs already emit; CM names only `__newbf_ct_emit`/corlib accessors + reads `m.comptime` (a sema-owned `Vec<String>`); DE emits named `Multicast.*` symbols + existing `field_addr`/`call_indirect`. **No new `use newbf_llvm` in sema; no new cross-crate Rust edge anywhere in the wave.** |

**Tasks that need the riskiest review attention:** **GI-T2** (the env-driven
`imethods` fill at a minted id + the dual C/C′ link routing — R1, the wave's deepest
edit), **GI-T0…T2 collectively vs GI-PRE** (the existing-corpus ratchet panic —
R2/R-A/R7), **IL-T2b** (the cross-yield spill + range-induction synthesis — R3, the
wave's other hard task), **CM-T1** (the net-new 10-field-`Type` sandbox attribute pin
— R8), **DE-T3a** (invoke-all arity drift + the `$Func` spill + the dispatch-seam
interception — R-DE1). **Lowest-risk (behavior-preserving roots):** GI-T0 (one
conjunct, gated by GI-PRE), IL-T0 (the gate, eager stays default), CM-T0 (audit), DE-T0
(the buffer in isolation), GI-PRE/GI-T5 (analysis / def-graph-independent).

---

## Sprint schedule (review-batches)

Six review-batches. Within a sprint, PARALLEL tasks have no ordering force; SERIAL
tasks must land in the listed order. The cadence remains one agent at a time; a
"sprint" groups tasks that share a gate or co-land. **The reviewer MUST keep GI's
ratchet-touching tasks (GI-T2/T3) and DE's parser-ratchet task (DE-T2) in batches
where no *other* feature's first behavior-changing task co-lands**, so a verify/parser
regression's run-corpus signal is never confounded (force §dependency-analysis.3).

### Sprint A — Foundation analysis + the four behavior-preserving roots
*Goal: GI-PRE bounds the GI scope; GI-T0 lifts the mono-index exclusion (gated by the
re-run verify corpus); the three other spines open at their isolated, additive roots.
All behavior-preserving (GI-T0 is behavior-neutral except where a generic interface is
in a type position). Demonstrable: verify 162/162 after GI-T0; `enum_manual.bf → 6`
(IL substrate, pre-existing); `mcast_manual.bf → 30` (DE buffer in isolation);
CM-T0 audit reproduces the method-count inflation.*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **GI-PRE** ★ | Triage every existing-corpus generic-interface impl (`ClassD`/`ClassE`/`EnumeratorTest`/`IndexTest`*); classify resolves/safe/gap; bound the v1 trigger so none panics `resolve_itable_impl`; document the §3.9 deferred paths | generic-interfaces | — | SERIAL (gates GI-T0…T2 scope; analysis only) |
| **GI-T0** ★ | Lift the `td.kind != TypeKind::Interface` conjunct (`lower.rs:737`) so generic interfaces enter `GenericDecls`; confirm `record_inst` mints `kinds=Interface` + `lower_ty_env` returns `Ref(mono_id)` (no code change) | generic-interfaces | GI-PRE | SERIAL (the first domino; re-run verify after) |
| **DE-T0** | Corlib `Delegate.bf` `Multicast` value-struct + hand-emit `Add`/`Get`/`Grow` with explicit `Struct(func_struct)` 16-byte stride + `Dispose`+`DisposeHook`; layout pin `{Ptr,i64,i64}`; `mcast_manual.bf → 30`, `event_scope_dispose.bf → 0` (existing `function` subscribers, T1-independent) | delegates-events | — | PARALLEL |
| **CM-T0** | Audit/confirm `emit_metadata` emits MethodInfo/AttributeInfo into a sandbox-shaped `from_ir` module + corlib `GetMethod`/`GetCustomAttribute` JIT-resolve; **reproduce the method-count inflation** (motivates CM-T1.5) | comptime-metaprog-v2 | — | PARALLEL |

> IL does not open in Sprint A only to keep one agent on each track; IL-T0 may
> equally open here (it is fully independent). Sequenced into Sprint B below for
> batch balance.

### Sprint B — GI's first registration + the parallel spines' guards/skeletons
*Goal: GI discovers interface-base mono requests (Seam B); IL lands the `[Coroutine]`
gate (eager stays byte-identical); DE registers named delegates + parses `event`; CM
lands the one sema filter + the sandbox pin. Demonstrable: GI dump-ir `IFaceD$i16`
registered; `lazy_fallback.bf` (eager value + fallback diagnostic);
`delegate_concrete_call.bf → 12`; CM unit test (generator excluded from method count).*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **GI-T1** ★ | Seam B: one `td.bases` walk in `collect_insts_type` (`:2056`, before the member loop) via `use_in_type` so `class ClassD : IFaceD<int16>` requests the mono in pass 1; the `monos2.is_empty()` assert holds | generic-interfaces | GI-T0 | SERIAL after GI-T0 |
| **IL-T0** | `[Coroutine]` gate: `has_coroutine_attr` + register `"Coroutine"` in `ATTR_BUILTIN_MARKERS`; widen `collect_type_generator_edits` arm to bind `attributes`; absent ⇒ unchanged eager path; unsupported-shape ⇒ eager + stderr diagnostic | iterators-lazy | — | PARALLEL |
| **DE-T1** | `StructTable.delegate_sigs` pass over arity-0 `Item::Delegate`; `lower_value_ty` + both `fn_sigs` sites consult it; `delegate_concrete_call.bf → 12` | delegates-events | DE-T0 | PARALLEL |
| **DE-T2** | Contextual `event` keyword in `member()` + `Member::Event` AST + forced `span()`/`print.rs` arms + hand-edit member-registration walks (synthesize backing field + record event set) | delegates-events | DE-T0 | PARALLEL (**moves the parser ratchet** — isolate from GI-T2/T3) |
| **CM-T1** ★ | Sandbox method/attr value-struct-return pin: method half mirrors the FieldInfo precedent (`mMethods`@7); **attribute half net-new** (extend hand-built `Type` to 10 fields, `TypeMeta` struct-literally, index `mAttributes`@9) | comptime-metaprog-v2 | CM-T0 | PARALLEL (CM's riskiest task — R8) |
| **CM-T1.5** | The one v1 lowering edit: exclude `module.comptime` methods from `MethodMeta` (`:5155-5162`); unit test (generator-bearing class counts non-comptime only); `reflect_method_count.bf → 2` control green | comptime-metaprog-v2 | CM-T0 | PARALLEL (independent of CM-T1) |

### Sprint C — The two hard keystones: GI-T2 + IL's lazy machinery
*Goal: the wave's deepest edits land in isolation. GI-T2 (env-driven itable fill +
dual link routing); IL-T1 (the liveness analysis + the hand-written resume-IR pin) +
IL-T2a (straight-line synthesis). Keep GI-T2 ALONE among ratchet-touchers this batch.
Demonstrable: GI dump-ir (`imethods[IFaceX$i16]`, `iface_bases.contains`, vtable slot);
IL-T1 hand-written enumerator runs; `lazy_straightline.bf → 6`.*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **GI-T2** ★★ | Seams C+C′+D: extract the per-method slot body into a `(mono_id, template_decl, mono_env)` helper (env-resolved `T`, `this=Ref(mono_id)`); fill via `t.monos` in `fill_iface_members`; **Seam C′** insert `iface_links[mono_iface_id]` in `collect_iface_bases`; dump-ir gate | generic-interfaces | GI-T1 | SERIAL (the keystone — R1; the ONLY ratchet-toucher this batch) |
| **IL-T1** ★ | `classify_generator_shape` (3 loop ASTs) + the cross-yield liveness/induction **3-way partition** (captured args / induction slots / cross-yield locals); **hand-written `switch(mState){…} <shared if>` enumerator runs** (pins the resume-IR pattern before synthesis) | iterators-lazy | IL-T0 | PARALLEL |
| **IL-T2a** | Straight-line synthesis + the 3-edit machinery (whole-body REPLACE + RETURN-TYPE replace to `__GenN<E>` + EOF-APPEND top-level struct); `lazy_straightline.bf → 6`; `__GenN` re-parses to `Item::Type` | iterators-lazy | IL-T1 | SERIAL after IL-T1 |

### Sprint D — First behavior: GI dispatch + IL's risky loop + CM/DE marquees
*Goal: GI's first run-corpus dispatch (5 programs); IL's hard loop synthesis (the
riskiest IL node, isolated); CM's method/attribute marquees; DE's invoke-all. Keep
GI-T3 from co-landing with DE-T3a's first behavior in the same review (both run-corpus
behavior-changers, but distinct features — review separately). Demonstrable:
`generic_iface_dispatch.bf → 123`; `lazy_loop.bf → 6`, `lazy_take_infinite.bf → 10`;
`comptime_reflect_attr_arg.bf → 42`; `event_multicast_two.bf → 30`.*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **GI-T3** ★ | Mono-iface dispatch (confirm `emit_iface_dispatch`/Seam E/F id-keyed) + 5 run-corpus: `generic_iface_dispatch.bf → 123`, `_two_impls.bf → 357`, `_param.bf → 357`, `_inherit.bf → 5`, `_default.bf → 7` | generic-interfaces | GI-T2 | SERIAL after GI-T2 (the behavioral core) |
| **IL-T2b** ★ | Single-loop synthesis: captured-arg + induction (range `mCur`/`mHi`+pred, or re-emitted While/CFor) + cross-yield-local fields; loop-entry/resume two-state switch + shared post-switch `if`; identifier→field rewrite span-by-span; `lazy_loop.bf → 6`, `lazy_take_infinite.bf → 10` | iterators-lazy | IL-T2a | SERIAL (RISKIEST IL node — R3; isolated so the loop bug bisects independently) |
| **CM-T3** | Attribute marquee (the v1 spine, no lowering change): `comptime_reflect_attr_typeid.bf → 1`, `comptime_reflect_attr_arg.bf → 42` (attribute-driven codegen headline) | comptime-metaprog-v2 | CM-T1 | PARALLEL (independent of CM-T1.5) |
| **DE-T3a** | Invoke-all `e.Invoke(args)`/`e(args)`: `try_lower_event_invoke` before `lower_method_call` (`:8326`) + loop (Count/Get + per-entry **spill** + `code`/`target` load + `call_indirect` **with arity assert**); fold minimal `+=`→`Add`; `event_multicast_two.bf → 30`, `event_empty_raise.bf → 0`, `event_add_then_invoke_arg.bf → 25` | delegates-events | DE-T0, DE-T2 | PARALLEL (R-DE1; isolate from GI-T3 in review) |

### Sprint E — Behavioral completions: GI args/is-as + GI constraint + CM methods + DE unsubscribe
*Goal: GI's distinct-args/`is`-`as` + the def-graph-independent constraint enforcement;
CM's method marquee (after CM-T1.5); DE's `-=` removal. Demonstrable:
`generic_iface_distinct_args.bf → 7`, `_is_as.bf → 1`, `_constraint_ok.bf → 11`;
`comptime_reflect_method_count.bf → 2`; `event_unsubscribe.bf → 10`.*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **GI-T4** ★ | Distinct-args independence + `is`/`as`: `generic_iface_distinct_args.bf → 7` (distinct iface NAMES, no return-type overloading) + `_is_as.bf → 1`; Seam F′ — the `Expr::Generic` arm in `type_id_of` (NEW code, R9) | generic-interfaces | GI-T3 | SERIAL after GI-T3 |
| **GI-T5** | Seam G constraint classify+enforce (`GenericInterface(name,arity)` kind, arity-aware `lookup`+`transitive_reaches`); `generic_iface_constraint_ok.bf → 11` + a verify negative fixture | generic-interfaces | — (rides def-graph, independent of T0-T4) | PARALLEL (may land first or any slot; bundle before GI-T6) |
| **CM-T2** | Method marquee (after CM-T1.5): `comptime_reflect_method_count.bf → 2` + `comptime_reflect_method_name.bf → 1` (generator on a **separate probe**, bind `MethodInfo` local) | comptime-metaprog-v2 | CM-T1, CM-T1.5 | PARALLEL |
| **DE-T3b** | `-=` unsubscribe: complete the `assign` event special-case (after `lvalue` `:12391`, before the `:12392` coerce; **reuse the slot**) for `AssignOp::Sub`→`Multicast.Remove`; `func_eq` (BOTH `$Func` fields); `event_unsubscribe.bf → 10` | delegates-events | DE-T3a | SERIAL after DE-T3a |
| **CM-T4** | (optional) corlib `String.Append` overload only if an example needs one (likely a no-op — `Append(int)`/`Append(char8*)` both landed) | comptime-metaprog-v2 | — | PARALLEL (off-path) |

### Sprint F — Tails: journals, doc cross-links, verify pins
*Goal: every feature's journal § + verify-corpus IR-shape pin + doc cross-links
(resolving the Wave-3 deferral sites this wave lifts). Demonstrable: per-feature
journal §§ past §131 (inner repo); verify count incremented + green.*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **GI-T6** | Journal + verify pin (mono itable IR shape) + cross-link `itables.md` §6/§10, `iterators-lazy.md` §7, `generic-constraints.md` §5 (the deferrals this lifts) | generic-interfaces | GI-T4, GI-T5 | SERIAL after GI tails |
| **IL-T3** | Journal (inner repo, past §131) + verify pin (synthesized `__GenN` + resume-switch shape) + cross-link | iterators-lazy | IL-T2b | PARALLEL |
| **CM-T5** | Docs: cross-link `COMPTIME.md`; resolve `comptime-reflection.md` §5 + `custom-attributes.md` §5/§8 deferrals → "landed (CMV2)"; journal (note CM-T1.5) | comptime-metaprog-v2 | CM-T0..T4 | PARALLEL |
| **DE-T4** | Journal + verify pin (`event` + `+=`/invoke IR shape) + cross-link `fn-values.md` | delegates-events | DE-T3b | PARALLEL |

---

## Per-task reference (id · title · feature · deps · seed · acceptance gate)

> Gate shorthand: **3 ratchets** = parser (100% `clean==files.len()`) + verify
> **162/162** + run-corpus **264** all-pass, under the **assertions-on** `cargo test`
> profile (R10). A task lands only when the 3 ratchets **and** its own new gate are
> green. (DE raises the parser/verify denominators via `Delegate.bf` + `event` fixtures;
> IL/CM/GI raise the run-corpus via new programs; CM-T1.5 + GI-T2 are the verify-gated
> lowering edits.)

### generic-interfaces (home doc §8) — *the foundation; the critical path*

- **GI-PRE** ★ · *Triage existing-corpus generic-iface impls (scope-gating analysis)* · deps: — ·
  *seed:* enumerate `ClassD`/`ClassE` (Interfaces.bf:229/247), `EnumeratorTest`
  (Loops.bf:17), `IndexTest`/`IndexTestExplicit` (Indexers.bf:96/112); predict per
  class whether the flip resolves a complete itable or panics `resolve_itable_impl`
  (`:1337`); classify (a) method-only/resolves, (b) property/indexer empty-`imethods`
  safe, (c) genuine gap → narrow the v1 trigger (defer in §5); document the §3.9
  deferred paths (`GetVal2`/`IDAdd`/`SGet`). · *gate:* a written triage; the v1-trigger
  scope is bounded so no corpus class panics after GI-T2. *Analysis only.*

- **GI-T0** ★ · *Lift the mono-index interface exclusion (Seam A, the first domino)* · deps: GI-PRE ·
  *seed:* in `index_generic_decls` (`:716`) remove the `td.kind != TypeKind::Interface`
  conjunct (`:737`); confirm `record_inst` mints the mono with `kinds=Interface`
  (`:1781-1782`, no code change) + `lower_ty_env` returns `Ref(mono_id)` once registered
  (no code change). · *gate:* **3 ratchets** (incl. §3.9 paths verify-clean under the
  flip, re-run per T-PRE); a dump-ir gate that `IFaceD<int16>` (with `ClassD` + Seam B
  present) resolves to a registered `Ref` id, `kinds=Interface`.

- **GI-T1** ★ · *Discover interface-base mono requests (Seam B)* · deps: GI-T0 ·
  *seed:* add ONE `td.bases` walk to `collect_insts_type` (`:2056`, before the member
  loop) via `use_in_type`, threading the in-scope visitor state, so `class ClassD :
  IFaceD<int16>` requests `IFaceD$i16` in **pass 1**. Do NOT edit `collect_insts_items`
  (it only delegates, `:2037`). · *gate:* **3 ratchets**; the `monos2.is_empty()` assert
  (`:571`) holds; a dump-ir gate that `IFaceD$i16` is a registered `Interface` id when a
  class implements it. Still no `imethods`/dispatch.

- **GI-T2** ★★ · *Env-driven `imethods` fill + dual link routing (Seams C+C′+D — the KEYSTONE)* · deps: GI-T1 ·
  *seed:* (a) extract the per-method slot body of `collect_iface_own_type`
  (`:1469-1527`) into a helper `(mono_id, template_decl, mono_env)` bypassing the
  `generic_params.is_empty()`/`by_name` gate, threading the mono env into
  `param_ir_ty`/`lower_value_ty`/`pointer_elem_env` (`:1495/1516/1500`) + `this =
  Ref(mono_id)` (`:1493`); (b) extend `fill_iface_members` (`:1405`) to iterate
  `t.monos`, run the helper per interface-kind mono, merge into `own` before
  `compose_iface_members` (`:1416`); (c) **Seam C′:** in `collect_iface_bases` (`:1588`),
  after building `iface_links` (`:1592`), insert `iface_links[mono_iface_id] = [resolved
  mono base ids]` per interface-kind mono (template bases through the mono env), so
  `add_iface_flat` (`:1677`) pulls `IA$i16` into `iface_bases[C]`; (d) Seam D matches
  `Ref(mono_id)` with no code change. · *gate:* **3 ratchets** (incl. GI-PRE triage); the
  **dump-ir gate (R1)**: `imethods[IFaceX$i16] == [("GetVal", sig)]` on a CLEAN inline
  fixture with `sig.ret==i16`, `sig.params==[Ref(IFaceX$i16)]`;
  `iface_bases[ClassD].contains(&IFaceX$i16)`; inherit `iface_bases[C].contains(&IA$i16)`
  (C′); `ClassD$vtable` carries `ClassD.GetVal` at `iface_slot_base[IFaceX$i16]`.
  *Riskiest task in the wave.*

- **GI-T3** ★ · *Mono-interface dispatch + first run-corpus (Seams E/F)* · deps: GI-T2 ·
  *seed:* confirm `emit_iface_dispatch` (`:11878`) + dispatch branch (`:12058`) +
  default-bodied methods at the mono id (`:5682-5692`) are id-keyed (no code change); add
  `generic_iface_dispatch.bf`/`_two_impls.bf`/`_param.bf`/`_inherit.bf`/`_default.bf`. ·
  *gate:* `→ 123 / 357 / 357 / 5 / 7` under the JIT/Stomp run-corpus; verify 162/162
  (Interfaces.bf verify-clean, NOT a behavioral gate). The minimal-but-correct first slice.

- **GI-T4** ★ · *Distinct-args independence + `is`/`as` (Seam F′)* · deps: GI-T3 ·
  *seed:* `generic_iface_distinct_args.bf` (distinct iface NAMES, no return-type
  overloading) + `_is_as.bf`; add the `Expr::Generic` arm to `type_id_of` (`:11521`) —
  mangle args via `lower_ty_env`+`mangle_generic`, look up `by_name` (NEW code, R9). ·
  *gate:* `→ 7 / 1`; all gates green.

- **GI-T5** · *Constraint classify + enforce (Seam G — INDEPENDENT, rides def-graph)* · deps: — ·
  *seed:* in `classify_constraint` (`constraints.rs:1057`) split the
  `segments.len()>1 || !last.args.is_empty()` arm: arity-keyed kind check
  (`index.lookup(name, last.args.len())` `:289` → `kind_by_name_arity_of` `:269` ==
  `Interface`) → new `ConstraintKind::GenericInterface(name, arity)`, else keep
  `GenericBound`; in `check_one` (`:838`) add a `GenericInterface` arm using the
  **arity-aware** `lookup(name, arity)` (NOT `lookup_arity0`) + `transitive_reaches`
  (`:954`). Add `generic_iface_constraint_ok.bf` + a verify negative. · *gate:*
  `→ 11`; the negative's diagnostic fires (no false positive); the `IFaceD<T>`(arity 1)
  vs `IFaceD`(arity 0) coexistence does not confuse the classifier. *Enforcement is
  **arity-level**, not arg-level (R5) — be honest.* **Lifts GC §5's deferral.**

- **GI-T6** · *Journal + verify pin + doc cross-link* · deps: GI-T4, GI-T5 ·
  *seed:* journal §; verify-corpus fixture mirroring `generic_iface_dispatch.bf` (pin the
  mono itable IR shape); cross-link `itables.md` §6/§10, `iterators-lazy.md` §7,
  `generic-constraints.md` §5. · *gate:* journal present; verify count incremented +
  green; conventional commit + Co-Authored-By trailer.

### iterators-lazy (home doc §8) — *the lazy state machine; the second-hardest spine*

- **IL-T0** · *The `[Coroutine]` gate + eager stays default* · deps: — ·
  *seed:* `has_coroutine_attr` (model on `has_comptime_attr` `:12869`); register
  `"Coroutine"` in `ATTR_BUILTIN_MARKERS` (`:5086`); widen `collect_type_generator_edits`
  (`:5421`) to bind `attributes` + thread `src`; absent ⇒ unchanged `collect_generator_edits`;
  present+unsupported-shape ⇒ eager + stderr diagnostic (never panic). · *gate:* the 3
  eager programs (`yield_eager_basic → 6`, `yield_break → 3`, `yield_empty → 0`) + 5
  foreach + `enum_manual → 6` **byte-identical** (diff-check the eager `SrcEdit` output,
  not just run values); `lazy_fallback.bf` produces the correct eager value AND a stderr
  fallback diagnostic; 3 ratchets green. *Observable.*

- **IL-T1** ★ · *Shape classifier + cross-yield liveness/induction partition (the hard analysis)* · deps: IL-T0 ·
  *seed:* `classify_generator_shape -> {StraightLine, SingleLoop(LoopForm), Unsupported}`
  enumerating the 3 loop ASTs (`Range` = `ForEach` over `Range`/`ClosedRange`; `While`;
  `CFor`); a liveness pass returning the **3-way partition** (captured args / synthesized
  induction slots `lo`/`cur`/`hi`+pred from `:7460-7464` / cross-yield-live user locals,
  declaration-precedes-yield ∧ read-after). · *gate:* unit tests assert the **span-keyed
  partition** (NOT a flat name set — so a captured arg is not double-counted); PLUS a
  **hand-written concrete `.bf` enumerator** with the exact `switch(mState){…} <shared
  if>` shape (no synthesis) runs to its expected value under the guard (pins the resume-IR
  pattern, R3, before IL-T2b synthesizes it). *Behavior-preserving.*

- **IL-T2a** · *Straight-line synthesis + the edit machinery* · deps: IL-T1 ·
  *seed:* build the 3-edit set (whole-body REPLACE = construct+seed+`return __GenN`;
  RETURN-TYPE replace to `__GenN<E>`, R4; EOF-APPEND the top-level `struct __GenN<E…>`
  with `int mState`+`mCurrent`+resume `switch` `MoveNext`+`Current`/`Dispose`/`GetEnumerator`);
  re-parse with a fresh `FileId`. Reuse the 5th branch unchanged. · *gate:*
  `lazy_straightline.bf → 6` under JIT/Stomp; a unit test that the appended `__GenN`
  re-parses to an `Item::Type` and lands in `index_generic_decls` (R11); 3 eager + 5
  foreach + `enum_manual` unchanged; verify 162/162.

- **IL-T2b** ★ · *Single-loop synthesis: cross-yield spill + range induction (RISKIEST IL)* · deps: IL-T2a ·
  *seed:* extend to `SingleLoop` — captured-arg + induction (range `mCur`/`mHi`+pred, or
  re-emitted While/CFor test/update) + cross-yield-local fields (IL-T1's partition);
  loop-entry/resume two-state switch + the shared post-switch `if` (NO `break`/`continue`
  in case bodies); identifier→field rewrite span-by-span (R3). · *gate:* `lazy_loop.bf →
  6` (range + captured `n` + `<=` pred) and `lazy_take_infinite.bf → 10` (the `while
  (true)` unbounded proof, with `taken == 4` cross-check killing off-by-state aliasing)
  under JIT/Stomp; 3 eager + 5 foreach + `enum_manual` unchanged; verify 162/162. *The
  cross-yield spill under the guard — isolated so a loop bug bisects independently.*

- **IL-T3** · *Journal + doc cross-link + verify pin* · deps: IL-T2b ·
  *seed:* journal § (inner repo `docs/journals/`, past §131); a verify-corpus fixture
  mirroring `lazy_loop.bf` (pin `__GenN` + resume-switch IR shape); cross-link. · *gate:*
  journal present; verify `clean==files.len()` stays 100% (new fixture raises the floor);
  conventional commit + trailer.

### comptime-metaprogramming-v2 (home doc §8) — *a leaf (no GI dep); one real sema edit*

- **CM-T0** · *Audit + confirmation ("is it really already built?")* · deps: — ·
  *seed:* confirm (don't change) `emit_metadata` emits MethodInfo (`:515-539`) +
  AttributeInfo (`:551-612`) into a sandbox-shaped `from_ir` module; corlib
  `GetMethod`/`GetCustomAttribute` lower/JIT-resolve; record the `(name,symbol)` method
  sort + the FIELDS-gates-attributes fact; **reproduce the method-count inflation**
  (`GetMethodCount()` on a `[Reflect(.Methods)]` class carrying a generator counts the
  generator — motivates CM-T1.5). · *gate:* a dump/assert shows non-null `mMethods`/
  `mAttributes`; the inflation is reproduced; all corpora unchanged. *Behavior-preserving.*

- **CM-T1** ★ · *Sandbox method/attr value-struct-return pin (HARD; attr half net-new)* · deps: CM-T0 ·
  *seed:* in `emit.rs` tests — method half mirrors `from_ir_sandbox_..._fieldinfo_return`
  (`:909`) for `MethodInfo` (index `mMethods`@7, bind a local, `GetName()`); **attribute
  half (net new)** extend the hand-built `%struct.Type` to the full **10-field** layout
  (`mAttrCount`@8/`mAttributes`@9, matching `:413-428`), build `TypeMeta` struct-literally
  (not `::new`, which hard-codes empty attributes `module.rs:169`) with non-empty
  `attributes`, index `mAttributes`@9, bind an `AttributeInfo` local, read
  `GetTypeId()`/`GetIntArg(0)`; all inside `$ct_emit_run`; assert corlib accessors survive
  the strip + generator/shim gone. · *gate:* both pass — pins struct-by-value method/attr
  reflection in the sandbox, not just the app JIT. *Risk: medium-high (R8).*

- **CM-T1.5** · *Exclude `[Comptime]` methods from `MethodMeta` (the one real lowering edit)* · deps: CM-T0 ·
  *seed:* in `assign_type_ids_and_meta` (`:5153-5168`) build a `HashSet<&str>` from
  `m.comptime` (populated `:6304`) and `continue` over any `sig` whose `full_name` is in
  it before pushing `MethodMeta` (no `MethodSig` change; `&mut Module` in scope). · *gate:*
  a unit test that a `[Reflect(.Methods)]` class **carrying** a generator reports
  `GetMethodCount()` = non-comptime count and `GetMethod(i)` never names the generator;
  `reflect_method_count.bf → 2` (the generator-free control) green; verify + run clean.
  *The genuine sema lowering change — why "no sema edit for v1" is retracted for the method
  axis.*

- **CM-T2** · *Method marquee (run-corpus)* · deps: CM-T1, CM-T1.5 ·
  *seed:* `comptime_reflect_method_count.bf → 2` + `comptime_reflect_method_name.bf → 1`;
  both put the generator on a **separate probe class** (R6) so neither it nor its emitted
  member enters the reflected target's set; the name program binds a `MethodInfo` local in
  the emitted text (R7) + uses `Append(char8*)`. · *gate:* both pass under JIT/Stomp; final
  module JIT+AOT-links clean; an integration test asserts no double-free under Stomp (R13).

- **CM-T3** · *Attribute marquee + attribute-driven codegen (the v1 spine; no lowering change)* · deps: CM-T1 ·
  *seed:* `comptime_reflect_attr_typeid.bf → 1` + `comptime_reflect_attr_arg.bf → 42` (the
  *attribute-driven codegen* headline — read `GetIntArg(0)` in the sandbox, emit a member
  returning it); attribute classes are **classes**, annotated types `[Reflect]`; optional
  `comptime_reflect_attr_strarg.bf → 1`. · *gate:* pass under Stomp; `attr_int_arg.bf → 42`/
  `attr_str_arg.bf → 1` stay green; no double-free. *Independent of CM-T1.5 — parallel.*

- **CM-T4** · *(optional) corlib `String.Append` overload* · deps: — ·
  *seed:* confirm §4 examples need no new `Append` (use `Append(int)`/`Append(char8*)`,
  both landed); add one only if a chosen variant needs it. · *gate:* a smoke if added;
  `append_overload.bf`/`string_append_int.bf` stay green. *Likely a no-op.*

- **CM-T5** · *Docs + journal* · deps: CM-T0..T4 ·
  *seed:* cross-link `COMPTIME.md`; resolve `comptime-reflection.md` §5 +
  `custom-attributes.md` §5/§8 deferrals → "landed (CMV2)"; journal § noting CM-T1.5 +
  stating generic-T as a v3 item. · *gate:* docs build; journal references `2`/`1`/`42` +
  the T1.5 filter.

### delegates-events (home doc §8) — *the only parser-ratchet mover; no GI dep*

- **DE-T0** · *`Multicast` corlib value-struct + the 16-byte `$Func` buffer (the rep core)* · deps: — ·
  *seed:* add `newbf-corlib/bf/Delegate.bf` `struct Multicast { function void()* mItems;
  int mCount; int mCap; … }` + register in `prelude()`; **hand-emit** `Add`/`Get`/`Grow` in
  sema (like the auto-getter `:6036-6047`) plumbing `elem = Struct(func_struct)` at the
  `alloc_array`/`elem_addr` seams (16-byte stride, R-DE2); `Multicast.Dispose()` free +
  `DisposeHook` for a `scope`-local (`:7003-7015`, R-DE4); layout unit test `{Ptr,i64,i64}`. ·
  *gate:* `mcast_manual.bf → 30` under JIT/Stomp using **existing `function void()`
  subscribers** (T1-independent — two 16-byte entries, no aliasing); `event_scope_dispose.bf
  → 0` (free-once, no double-free); the layout pin; verify 162/162. *Proves the rep in
  isolation, before events/named-delegates.*

- **DE-T1** · *Named concrete delegate type → callable `$Func` local* · deps: DE-T0 ·
  *seed:* add `delegate_sigs: HashMap<String,(IrType,Vec<IrType>)>` to `StructTable` + a
  `build` pass over arity-0 `Item::Delegate` (lower `return_ty`/`params` via `lower_ty_env`);
  `lower_value_ty` (`:13180`) returns `Struct(func_struct)` for a `delegate_sigs` path; both
  `fn_sigs` sites (`:6710-6723`/`:7255-7268`) pull `(ret,ptys)` from it (R-DE5). · *gate:*
  `delegate_concrete_call.bf → 12`; `function_pointer.bf → 12` unchanged; verify 162/162.
  Independent of DE-T2.

- **DE-T2** · *Contextual `event` keyword + `Member::Event` + backing-field synthesis* · deps: DE-T0 ·
  *seed:* **no lexer change** — recognize `event` contextually in `member()` via
  `at_ident_text` (`parser.rs:114`); add `Member::Event` (`ast.rs`) + forced
  `Member::span()`/`print.rs::member` arms (round-trip `event T N;`); hand-edit the
  member-registration walks (`register_type_struct`/`build.rs` loop) to synthesize a
  backing `Multicast` field + record `(owner,name)` in the synthesized-event set (R12);
  add the contextual arm before the field fall-through (`:3290`). · *gate:* an
  `event`-bearing class parses + round-trips; a focused "event-registers-a-field" test;
  the existing `Platform.bf`/`Event.bf` `event` identifiers still parse clean (R-DE3);
  **parser corpus 100%** (the ratchet moves); verify 162/162. No verbs yet.

- **DE-T3a** · *Invoke-all `e.Invoke(args)` / `e(args)` (the call-shape risk)* · deps: DE-T0, DE-T2 ·
  *seed:* `try_lower_event_invoke(base, name, args, src)` intercepting **before**
  `lower_method_call` (`:8326`, alongside `try_enum_construct`) for a synthesized `event`
  field; emit the invoke-all loop (copy the foreach-List skeleton `:7567-7603` + per-entry
  **`$Func` spill** + `code`/`target` load + `call_indirect` `:8368-8392`, **with the arity
  assert** `:8385`); fold the minimal `+=`→`Add` here so the test is end-to-end. · *gate:*
  `event_multicast_two.bf → 30`, `event_empty_raise.bf → 0`, `event_add_then_invoke_arg.bf
  → 25` under JIT/Stomp; the single-target `$Func` programs (`function_pointer`/`fn_null`/
  `closure_arg`/`list_hof`/`lambda_*`/`mref_*`) unchanged; verify 162/162. *R-DE1 — the
  verify-clean arity-drift surface; the smallest possible diff for the call shape.*

- **DE-T3b** · *`-=` unsubscribe + `func_eq` structural removal* · deps: DE-T3a ·
  *seed:* complete the `assign` event special-case (`:12391`, **after** `lvalue`, **before**
  the `:12392` coerce; **reuse** the `slot`, do not recompute `field_addr`) for
  `AssignOp::Sub`→`Multicast.Remove`; add `func_eq` (compares **both** `$Func` fields, NOT a
  copy of `func_code_field` `:12554`) used by `Remove`'s scan+shift. · *gate:*
  `event_unsubscribe.bf → 10` under JIT/Stomp (the survivor fires after a removal that
  persists to the raise — the in-place field mutation); all DE-T3a gates green; verify
  162/162.

- **DE-T4** · *Journal + doc cross-link + verify pin* · deps: DE-T3b ·
  *seed:* journal §; a verify-corpus fixture exercising an `event` + `+=`/invoke (pin the IR
  shape); cross-link `fn-values.md`. · *gate:* journal present; verify count incremented +
  green; conventional commit + trailer.

---

## Recommended execution order (single reviewer, one agent at a time)

A linearization of the four-spine DAG keeping every commit behind green gates and
minimizing context-switching. Critical-path (GI) tasks marked ★; the wave's two
hardest tasks marked ★★/★. **The GI foundation goes first**; the three parallel spines
are interleaved against GI's slack, respecting the batch-isolation of GI's
ratchet-touching tasks (GI-T2/T3) and DE's parser-ratchet task (DE-T2).

1. **GI-PRE** ★ — triage the existing-corpus generic-iface impls; bound the v1 trigger
   so no class panics `resolve_itable_impl`. The ratchet-keystone analysis. **Do first.**
2. **GI-T0** ★ — lift the `:737` mono-index exclusion (the first domino), gated by the
   re-run verify corpus (incl. the §3.9 deferred paths staying verify-clean).
3. **DE-T0**, **CM-T0** — open two parallel spines at their additive/audit roots
   (DE's 16-byte buffer in isolation; CM's "is it built?" audit + the inflation repro).
4. **GI-T1** ★ — Seam B, the interface-base mono request (pass-1 `td.bases` walk).
5. **IL-T0**, **DE-T1**, **DE-T2**, **CM-T1** ★, **CM-T1.5** — the parallel spines'
   guards/skeletons (IL's `[Coroutine]` gate; DE's named-delegate callability + `event`
   parsing **[parser ratchet — isolate]**; CM's sandbox pin + the one sema filter).
6. **GI-T2** ★★ — the keystone: env-driven `imethods` fill at a minted id + the dual
   C/C′ link routing. **The wave's deepest edit — keep it ALONE among ratchet-touchers
   this batch.** Gated by the R1 dump-ir pin (on a clean inline fixture) before any
   dispatch task.
7. **IL-T1** ★, **IL-T2a** — IL's liveness partition + the hand-written resume-IR pin,
   then the straight-line synthesis + edit machinery.
8. **GI-T3** ★ — mono-interface dispatch + the 5 `generic_iface_*.bf` programs
   (`123/357/357/5/7`). The GI behavioral core.
9. **IL-T2b** ★ — the cross-yield spill + range-induction synthesis (`lazy_loop → 6`,
   `lazy_take_infinite → 10`). **The other genuinely-hard task — isolated so the loop
   bug bisects independently.**
10. **CM-T3**, **DE-T3a** — CM's attribute marquee (the v1 spine, `attr_arg → 42`) and
    DE's invoke-all (`event_multicast_two → 30`). **Review separately** (both run-corpus
    behavior-changers, distinct features).
11. **GI-T4** ★, **GI-T5**, **CM-T2**, **DE-T3b** — GI's distinct-args/`is`-`as` +
    Seam F′ (R9); GI's def-graph-independent constraint enforcement (`→ 11`, lifts GC
    §5); CM's method marquee (after T1.5); DE's `-=` unsubscribe (`→ 10`).
12. **GI-T6**, **IL-T3**, **CM-T5**, **DE-T4** — journals + verify pins + doc
    cross-links (resolving the Wave-3 deferral sites this wave lifts).

> **Earliest demoable states:** after step 8 you have **dynamic dispatch through a
> monomorphized generic interface** (`generic_iface_dispatch.bf → 123`) — the marquee
> Wave-4 capability and the foundation three next-wave follow-ons build on — plus DE's
> 16-byte buffer + CM's sandbox method/attr reflection proven. After step 9 you have
> **genuinely lazy `yield`** (`lazy_take_infinite.bf → 10`, an unbounded sequence the
> eager path could never run). After step 11 each feature is full: GI constraint
> enforcement + `is`/`as`, CM method+attribute-driven codegen, DE events with `+=`/`-=`/
> invoke.

---

## What was NOT sequenced (deferred / next-wave)

Each home doc's explicit deferrals carry no tasks here, by design. The **REAL cross-
feature edges** (the GI → deferred-follow-on edges the dependency analysis identified)
are the natural next-wave merge points — recorded here so the sequencer knows what GI
unblocks:

- **generic-interfaces** (GI §5): generic interface **extensions** (`extension
  IFaceD<T>`, `GetVal2`), generic-interface **properties/indexers** (`IIndexable<T>`,
  empty-`imethods`-safe but non-dispatching), explicit impl of a generic interface,
  **variance** (`IEnumerator<out T>` — the genuinely-hard miscompile-trap deferral),
  **arg-level** constraint enforcement (v1 is arity-level, R5), method-generic
  interface methods (`T Add<T2>`), static-virtual interface methods, a **generic class
  implementing a generic interface** (`Foo<U> : IFaceD<U>` — needs the class's own
  env), abstract-`T` constraint dispatch (unreachable, `targ_is_abstract` `:1973`),
  boxing value structs to a generic interface, `delete`/`GetType` through a
  generic-iface-typed value, multi-type-param generic interfaces.
- **iterators-lazy** (IL §5): **nested loops** (resume = a tuple of counters),
  `try`/`finally` around a `yield` (finally on mid-iteration `Dispose`), `yield` in a
  `defer`/`switch` arm, list-`foreach` as the loop (all need a **general
  CFG-to-state-machine transform + real dataflow liveness** — the codebase has none),
  the **heap (`Ref`) lazy enumerator** + auto-`delete` ownership + large-aggregate
  captures, generator return-type inference, **and the interface-typed
  `IEnumerator<T>`/`IEnumerable<T>` half** (the GI → IL edge: needs the mono iface id +
  itable from GI-T0…T2 PLUS an `IEnumerator`/`IEnumerable` corlib pair (none exist) +
  the 5th branch dispatching through `emit_iface_dispatch`).
- **comptime-metaprogramming-v2** (CM §5): **generic-T reflection** (a `[Comptime]`
  generic generator reflecting `typeof(T)` — blocked by the `record_method_inst`
  `[Comptime]`+generic guard `:1851-1857` + `typeof(generic-T)`, **NOT** by GI — a
  comptime-metaprogramming-v3 item), the **general same-set emit-and-reflect** (R6,
  CM-T1.5 only partially solves it), **signature-driven** codegen (extend `MethodMeta`
  with param/return types), value-struct attribute types (no dense id), constructed
  attribute instances (`GetCustomAttribute<T>()` — sandbox can't return a struct on the
  value-fold path), member/parameter-level attribute reflection, float attribute args
  (the JIT `__real@` MEMORY gap), a dedicated `ReflectPolicy::ATTRIBUTES` bit.
- **delegates-events** (DE §5): **value-struct field-dtor chaining in `emit_destroy`**
  (until it lands, `event` fields of heap classes **leak** the buffer — R-DE4),
  unqualified `Name(args)` invoke inside the class, **generic delegates** (`Action<T>`
  — needs a *delegate monomorph path* mirroring `record_inst`/`register_mono`, **NOT**
  GI, since `$Func` is signature-agnostic), the **full upstream `Event<T> where T :
  Delegate`** (the GI → DE edge: its `Enumerator : IEnumerator<T>` is the exact excluded
  construct + `rettype(T)`/`params T`/`as List<T>`/bit-packed `mData`), delegate as a
  heap GC object with identity, a general `operator+=`, value-returning multicast,
  delegate variance/`is`/`as`/`async`, enumeration-safe add/remove during invoke,
  bound method-ref of a value/`mut`/`ref` receiver.

**The natural next-wave merge points (what GI unblocks):**
- **Interface-typed generic enumerators** (`IEnumerator<T>`/`IEnumerable<T>`) — needs
  GI's mono iface id + itable; unlocks BOTH IL's interface-typed `foreach` half AND any
  consumer of a dynamically-dispatched generic enumerator at once.
- **Generic-interface `where`-constraints** (`T : IEnumerator<TElement>`) — **already
  delivered** by GI-T5 (Seam G, off the def-graph) for the *enforcement diagnostic*;
  the *dispatch* through such a constraint works by erasure today.
- **The full `Event<T>`** (DE) — consumes GI's monomorphized generic-interface itable,
  on the same footing as IL's interface-typed half.
- **Generic delegates** (`Action<T>`) and **generic-T comptime reflection** — recorded
  here precisely because they are **NOT** GI edges (a delegate monomorph path / the
  comptime-generic guard lift respectively); the sequencer must not mis-attribute them
  to GI.
