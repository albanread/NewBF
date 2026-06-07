# NewBF — Cross-Feature Sprint Plan (Wave 3: generic-constraints · iterators · comptime-reflection · custom-attributes)

*Drafted 2026-06-07. Sequences the agent-assignable tasks from the four
Wave-3 design docs in [`docs/design/`](.) —
[`generic-constraints.md`](generic-constraints.md),
[`iterators.md`](iterators.md),
[`comptime-reflection.md`](comptime-reflection.md),
[`custom-attributes.md`](custom-attributes.md) — into one schedule. Companion
to [`SPRINT-PLAN-2.md`](SPRINT-PLAN-2.md) (Wave 2, now **landed** — git `dcaf2d7`
"§129 WAVE 2 COMPLETE"), [`SPRINT-PLAN.md`](SPRINT-PLAN.md) (Wave 1), the
12-phase [`PLAN.md`](../../../PLAN.md), and the original
[`SPRINTS.md`](../../../SPRINTS.md).*

*Wave 3 is materially **less** runtime/ABI-risky than Wave 2: Wave 2 built the
substrate (the Stomp guard + JIT absolute-symbol seam, the `%ClassVData` header,
the reflection metadata pipeline, the comptime emission fixpoint, the mixin
splice). Wave 3 **consumes** that substrate. Three of the four features add
**zero** new IR instruction and **zero** new runtime symbol; the riskiest ABI
move (one `%struct.Type` extension for custom-attributes) is a known,
already-rehearsed pattern (Wave 2's `mFields`/`mMethods` adds). The dominant
risk shifts from "new runtime/link seam" to "**verify-clean-but-wrong**" — a
slot-shift, a ratchet false-positive, a value-struct `this`-aliasing miscompile,
a double-evaluated comptime arg — all caught only by the authoritative run-corpus
gate or a deterministic layout pin, never by the verify ratchet alone.*

## Preamble — cadence and invariants

Cadence is unchanged from Wave 2: **one developer, one agent at a time,
review/test/commit per task.** A "sprint" here is a *review batch* — a set of
tasks reviewed and merged as a group because they share a gate or co-land
atomically. "PARALLEL" means *no dependency forces an order* (interleave them
however the review queue prefers); "SERIAL" means they must land in the listed
order.

**The standing gates, green at every task boundary:**

- **Parser corpus** — `newbf-parser/tests/corpus.rs`, **160/160**
  zero-diagnostic (a 100% ratchet: `clean == files.len()`, so adding a
  feature-suite file raises the denominator). Verified: only iterators touches
  the parser (the `yield` arm), so the parser ratchet moves only in IT.
- **Verify corpus** — `newbf-sema/tests/corpus.rs`, **160/160** LLVM-clean
  (same `clean == files.len()` ratchet; lowers each file **standalone**, never
  calls `run_emission`/`fold_comptime`). This is the ratchet generic-constraints
  must hold against (every false-positive diagnostic breaks it) and the one a
  `%struct.Type` slot-shift (custom-attributes) **cannot** detect — see R-A.
- **Run corpus** — `tests/newbf-tests/tests/run_corpus.rs`, **245** programs,
  JIT-run, full-i32 value check under the **Stomp memory guard**. **The
  authoritative behavioral gate.** Every Wave-3 feature produces at least one
  *verify-clean miscompile class* (a constraint false-positive that only the
  driver/AOT path surfaces; an enumerator `this`-aliasing bug; a comptime
  double-emit; a `%struct.Type` slot-shift) that the static corpus cannot catch
  — for those, run-corpus (or a deterministic layout/IR-shape unit pin) is the
  only real gate.

**8-bit exit-code caveat (MEMORY):** AOT exit codes truncate to 8 bits; all
value-checks use the JIT run-corpus harness, which reads the full i32. Keep AOT
probe values ≤255; corpus value checks may exceed it (JIT-only). Wave 3 adds no
new fault/abort harness — unlike Wave 2, no feature here aborts the process, so
the existing value-checking run-corpus harness suffices (the comptime double-free
guard hazard in CR is checked by the *already-existing* Stomp harness, which runs
`run_emission`'s sandbox under `GuardMode::Stomp`).

**Naming convention.** Each task keeps its *home-doc id* with a feature prefix
so it is traceable: `GC-T3` = generic-constraints task 3, `IT-T1` = iterators
task 1, `CR-T0` = comptime-reflection task 0, `CA-T2` = custom-attributes task 2.
(The home docs use bare `T0/T1/…` for iterators/generic-constraints and `CA-`/
the §-style for the others; this plan normalizes all four to the
`<PREFIX>-T<n>` form. iterators' merged task is `IT-T2+3`.)

**The Wave-2 substrate every Wave-3 feature stands on (verified landed):**

| Substrate (Wave 2) | Lives at | Wave-3 consumer |
| ------------------ | -------- | --------------- |
| `%ClassVData = {i32 mType, [N×ptr]}` header, `classvdata_name` (RF-T2) | `lower.rs:1066`, every `StructKind::Ref` (`:9982`) | GC Appendix-A note; CR/CA `typeof` is a `GlobalAddr` over it |
| Reflection metadata (`TypeMeta`, `Type`/`FieldInfo`/`MethodInfo`, `__newbf_type_by_id`) (RF-T0..T7) | `module.rs:97` `TypeMeta`, `reflectable`=`StructKind::Ref` (`lower.rs:4972`) | **CR reads it in the sandbox**; **CA appends to it** |
| comptime emission fixpoint (`run_emission`, `__newbf_ct_emit`, `add_absolute_symbol`, strip) (CB/MS-T0) | `emit.rs:204`, the literal wall `lower.rs:9891` | **CR relaxes the wall**; CA defers comptime composition |
| Monomorphization `(name, arity)` keying, member-blind `index_generic_decls` | `lower.rs:686` (keys `(name, arity)`); member-blind (no `Member` descent) | **IT forces a top-level generic enumerator**; GC keys its index `(name, arity)` |
| Stomp guard live in JIT **and** AOT (MS-T0..T7, MS-T3b) | `run_corpus.rs` under `GuardMode::Stomp`; AOT staticlib | CR's sandbox-String double-free hazard; IT's value-struct-under-guard |
| `check_delete_flow` analyze-phase body re-walk (MS-T5/6) | `ownership.rs:112`, signature `(files, &DefGraph, &Interner)` | **GC mirrors it exactly** (its whole pass structure) |

Everything Wave 3 needs already exists and is green. There is **no** new
cross-crate Rust edge (no new `newbf-llvm→newbf-runtime` or
`newbf-comptime→newbf-runtime`), **no** new JIT symbol-resolution seam, and the
HARD invariant (sema ⊥ llvm; `IrType: Copy`; `StructTable` owns its data) is
preserved by all four (each home doc §2 re-verifies this).

**Where the risk concentrates (read this first):**

| Feature | New IR instr? | New runtime symbol / link? | `%struct.Type`/ABI? | Touches parser? | Ratchet exposure |
| ------- | ------------- | -------------------------- | ------------------- | --------------- | ---------------- |
| generic-constraints (GC) | **no** (emits only `Diagnostic`s) | no | **no** | no | **HIGH** — a false-positive diagnostic on the 4 constraint-dense files breaks verify 160/160 |
| iterators (IT) | no (reuses block/alloca/call) | no | no | **yes** (`yield` arm, parser ratchet moves) | MED — 5 pinned foreach programs + first-of-kind generic value struct under the guard |
| comptime-reflection (CR) | no (relaxes one sema rewrite) | no (sandbox already has the metadata) | no | no | MED — comptime double-emit / silent-decline; sandbox struct-by-value reflection |
| custom-attributes (CA) | **no** (all const `.rodata` data) | no | **YES** (`%struct.Type` 8→10 fields, 3 atomic sites) | no | MED — `%struct.Type` slot-shift is verify-clean-but-wrong (R-A); 5 `TypeMeta` constructors |

The two genuinely load-bearing items: **GC's ratchet exposure** (the
dominant risk of the whole wave — zero new diagnostics on four
constraint-dense files under a per-file ratchet) and **CA's `%struct.Type` ABI
extension** (three aggregate sites that must move atomically, detectable only by
a layout pin, not the verify ratchet). IT's risk is the value-struct `this`
aliasing + exactly-once-Dispose-on-every-edge under the guard. CR's risk is the
single-evaluation / no-silent-decline plumbing at the one relaxed sema seam.

---

## Cross-feature dependency analysis

Each feature is internally a near-linear chain. The cross-feature couplings,
verified against the four docs **and** the tree, are:

1. **All four features sit on the LANDED Wave-2 substrate, so there is no
   intra-wave runtime/seam dependency.** Unlike Wave 2 — where MS-T0's JIT
   absolute-symbol seam unblocked two features — Wave 3's shared machinery
   (`add_absolute_symbol`, the emission fixpoint, `%ClassVData`, the reflection
   metadata) is **already built and green** (git `dcaf2d7`). The Wave-3
   couplings are therefore about *data-shape ordering* and *ratchet
   reconciliation*, not about who lands a seam first.

2. **custom-attributes (CA) requires the reflection metadata pipeline — and it
   is landed.** CA is, verbatim from its preamble, "an **additive table
   alongside `mFields`/`mMethods`**, not a new subsystem," riding RF-T0..T7
   (verified green). CA's entire data path reuses `reflectable` =
   `StructKind::Ref` (`lower.rs:4972`), the `type_id_of` dense map (`:4979`),
   `TypeMeta` (`module.rs:97`, verified to carry `fields`/`methods` but **not yet
   `attributes`** — so CA-T0 is genuine net-new IR work), and the
   policy-gated `emit_metadata` FieldInfo block as its template. **CA has no
   dependency on the other three Wave-3 features.** It is a self-contained serial
   chain CA-T0→T1→T2→T3→T4→T5.

3. **comptime-reflection (CR) composes comptime-breadth × reflection — both
   landed — and depends on NEITHER custom-attributes NOR generic-constraints in
   v1.** CR's enabling insight (verified, CR §3.1): the emission **sandbox JIT
   already contains the full reflection metadata**, because `emit_module` calls
   `emit_metadata` unconditionally for every module including the sandbox clone.
   So the only thing CR adds is relaxing the **one literal-only wall** at
   `try_lower_emit_type_body` (verified at `lower.rs:9891`,
   `let [Expr::Str(s)] = args else { return None };`) to accept a runtime
   `Ref(String)` arg. **CR ⟂ CA in v1:** the MEMORY-prompt's intuition
   "custom-attributes feeds comptime-reflection" is the *future* coupling — CA §5
   explicitly defers "Comptime-reflection composition (`[Comptime]` reading a
   decl's attributes)" and CR §5 defers comptime *attribute* reflection (it does
   *fields* only). In v1 they touch disjoint code (CR relaxes the EmitTypeBody
   wall + reads `Type.GetFieldCount`; CA appends `mAttributes` to `%struct.Type`
   + reads `GetCustomAttribute`). **However** — see force #4 — if they land in the
   same wave there is a `%struct.Type`-layout interaction to sequence.

4. **CR and CA both read `%struct.Type` in the sandbox/runtime; CA *changes its
   shape*. SEQUENCE CA's ABI rework (CA-T2) relative to CR with care.** CR's
   generator calls `typeof(T).GetFieldCount()`/`GetField(i)` against the
   `%struct.Type` aggregate **in the sandbox** (CR §3.1). CA-T2 extends
   `%struct.Type` from 8→10 fields across **three** atomic sites (`set_body`
   `:408`, per-type init `:528`, the `__newbf_type_unknown` sentinel `:558`) and
   bumps the layout pin 8→10. The corlib `Type.bf` field offsets the CR generator
   relies on (`GetFieldCount` reads `mFieldCount`, an *existing* field whose
   index does **not** move — CA *appends*) are stable across CA-T2, **so the two
   are compatible either order** provided CA-T2 only appends (verified: CA appends
   `mAttrCount`/`mAttributes` at the *end*, indices 8/9, leaving `mFieldCount`@3
   and `mFields`@6 fixed). **Decision: no hard ordering force, but land CA-T2
   (the ABI move) and CR-T0 (the EmitTypeBody relax) in *different* review
   batches** so a `%struct.Type` regression and a comptime-plumbing regression
   never co-land and confound each other's run-corpus signal. The recommended
   order lands CR's keystone first (it is the wave's longest-pole critical path),
   then CA's ABI wall, so the CR sandbox-reflection pin (CR-T1) is green against
   the smaller 8-field Type before CA grows it — and CA-T2's "9 `reflect_*.bf`
   stay green" gate then also implicitly covers CR's emitted members.

5. **generic-constraints (GC) is pure sema (zero IR) and depends on NONE of the
   other three.** GC mirrors `check_delete_flow` (`ownership.rs:112`) exactly —
   one new `constraints.rs` pass in `analyze` after the delete-flow line,
   signature `(files, &DefGraph, &Interner)` (verified). It emits **only
   `Diagnostic`s**, no IR, no symbol, no ABI. **It does not block iterators.**
   The MEMORY-prompt's hypothesis "generic-constraints likely underpins
   iterators' `IEnumerator<T>`" is **explicitly false for v1**: iterators.md §1/§6
   *duck-types the concrete enumerator by name* and states "**no `IEnumerator<T>`
   interface type is needed** (generic interfaces stay unsupported)"; GC.md §5
   *defers* generic-interface constraints (`T : IEnumerator<TElement>`) precisely
   because generic interfaces aren't monomorphized. So the `IEnumerator<T>`
   coupling the prompt anticipated is **out of both v1s** — neither feature builds
   it, neither blocks the other. GC is the fully-independent parallel filler track
   of Wave 3 (the role mixins played in Wave 2).

6. **iterators (IT) needs a generic VALUE struct on the executable path
   (first-of-kind) — but depends on no other Wave-3 feature.** IT's load-bearing
   tree fact (verified, `lower.rs:655-692`): `index_generic_decls` is
   **member-blind** (iterates only `Item::Type`/`Item::Namespace`, never descends
   into a type's members), so the corlib enumerator **must be a top-level generic
   `struct ListEnumerator<T>`**, not a nested type, or it never monomorphizes.
   This is an *internal* IT constraint, not a cross-feature one. IT also moves the
   parser ratchet (the only feature that does) via the `yield` arm. IT's two
   internal chains are `{IT-T0 ∥ IT-T1}` then `{IT-T2+3}` then `IT-T4`.

7. **No feature changes `lower_program`'s signature.** Wave 2 flagged the staged
   MX-T8 `lower_program -> (Module, Vec<Diagnostic>)` change as a landmine for
   CB's `run_emission` call site. **Wave 3 re-confirms none of the four touch it**
   — GC explicitly *avoids* it (GC §3.2: the wide signature change is "an explicit
   non-goal"; GC runs in `analyze`, not `lower_program`); IT's generator rewrite
   runs *inside* `lower_program` but does not change its signature; CR/CA don't go
   near it. So the Wave-2 staging concern is fully retired for this wave.

**Net ordering forces:**
- **CR-T0 first among the behavior-changing keystones** — it is the root of the
  longest critical chain (CR-T0→T1→T3→T4) and the single most plumbing-dense edit
  (the single-evaluation methods-table rewrite at the relaxed EmitTypeBody wall).
- **CA is a self-contained serial spine** (CA-T0→…→T5); its only cross-feature
  interaction is the soft "land CA-T2's `%struct.Type` move in a different batch
  from CR-T0" (force #4) — a confounding-avoidance preference, not a hard dep.
- **GC is fully parallel** (pure sema, zero cross-dep) — the ideal filler track.
- **IT is fully parallel** (its only deps are internal); it alone moves the
  parser ratchet.
- There is **no single highest-fanout unblocking task** the way MS-T0 was in
  Wave 2, because the shared seams are already landed. The critical path is
  chosen by *chain length × downstream value*, which points at CR.

---

## The critical path

The longest dependency chain — and the one gating the most novel downstream value
(reflection-driven codegen, the marquee Wave-3 capability) — runs through
**comptime-reflection**, because it composes two landed subsystems and its
keystone is the wave's most bug-prone single edit:

```
CR-T0 ─→ CR-T1 ─→ CR-T3 ─→ CR-T4 ─→ CR-T5
 (relax    (sandbox  (count   (name-    (docs)
  the wall  struct-   marquee  driven
  + single- by-value  run-     emission,
  eval +    reflection corpus)  needs CR-T2)
  diagnostic)pin)         │
                          └── CR-T2 (String.Append(char8*)) feeds CR-T4 only
```

- **CR critical sub-chain = CR-T0 → CR-T1 → CR-T3 → CR-T4** (4 serial nodes; T2
  branches onto T4 only). CR-T0 is behavior-preserving (a no-op until a generator
  uses the relaxed wall, guarded by the literal fast-path), but it is the
  *keystone*: the single-evaluation + methods-table-lookup + `coerce(I64,I32)` +
  diagnostic-not-silent-decline plumbing is where all three CR adversarial reviews
  located the bugs. CR-T1 is the **hard gate** that pins struct-by-value
  reflection (`GetField(i)` returns a value-struct `FieldInfo`) works inside the
  `$ct_emit_run` sandbox wrapper — the one thing the existing app-JIT
  `reflect_field_*.bf` tests do *not* cover.

> **CRITICAL PATH: CR-T0 ★ → CR-T1 ★ → CR-T3 ★ → CR-T4 ★ → CR-T5**
> (with CR-T2 a side-branch feeding CR-T4).

The other three features hang off this spine with full slack:
- **CA** (CA-T0→T1→T2→T3→T4→T5, 6 serial) is *longer in task count* but each task
  is lower-risk (a known ABI-extension pattern), and it gates only itself — no
  Wave-3 feature waits on CA. Its length makes it the *secondary* spine to
  interleave against CR's slack.
- **GC** (GC-T0..T5) and **IT** (IT-T0..T4) are independent parallel tracks.

**The single most-unblocking task is `CR-T0`** — the relaxed `try_lower_emit_type_body`
(the EmitTypeBody literal-wall relaxation). It is the root of the longest chain,
it is behavior-preserving (proven by the unchanged-corpora gate, so it lands with
no red window), and it is the keystone every downstream CR task builds on. It is
moderately cheap (one sema function rewritten, ~40 lines, plus two unit tests) but
**bug-dense** — so doing it first, in isolation, with the single-eval/diagnostic
unit tests, de-risks the whole CR chain before any run-corpus program depends on
it.

*(Contrast with Wave 2: there the most-unblocking task was the cheap-but-load-
bearing JIT seam MS-T0. In Wave 3 the seam is already landed, so the
most-unblocking task is instead the longest-chain keystone CR-T0 — same role
(de-risk the spine first), different reason (chain length, not fanout).)*

---

## Per-feature risk table

| Feature | backend (llvm) | runtime/guard | ABI / `%struct.Type` | sandbox/comptime | ratchet (verify/run) |
| ------- | -------------- | ------------- | -------------------- | ---------------- | -------------------- |
| GC | none (emits no IR) | none | none | none (runs in `analyze`, before emission) | **HIGH** — false-positive on 4 dense files breaks verify 160/160; gated by per-file *and* multi-file pins |
| IT | none (reuses existing IR) | value-struct under Stomp (first-of-kind generic value struct executes) | none | none (generator rewrite uses the comptime *parser/FileId*, never comptime eval) | MED — 5 pinned foreach programs + parser ratchet moves; exactly-once-Dispose-on-every-edge |
| CR | none (relaxes a sema rewrite) | sandbox `new String` body double-free faults the **compiler** under Stomp | none (reads `%struct.Type`, doesn't change it) | **the headline** — single-eval, no-silent-decline, struct-by-value reflection in `$ct_emit_run` | MED — back-compat literal path must stay green; emitted member re-resolves |
| CA | emits `%struct.AttributeInfo` + `[n×i64]` arg arrays (all `.rodata`) | none (no heap, no symbol) | **YES** — `%struct.Type` 8→10 across 3 atomic sites + sentinel | none (comptime composition deferred §5) | MED — slot-shift verify-clean-but-wrong (R-A); 5 `TypeMeta` constructors; 9 `reflect_*.bf` must stay green |

## Risk register (cross-cutting, numbered with mitigations)

| # | Risk | Affected | Mitigation |
| - | ---- | -------- | ---------- |
| **R1** | **GC ratchet false-positives (DOMINANT risk of the wave).** A single over-eager constraint diagnostic on `Constraints.bf`/`Generics.bf`/`Generics2.bf`/`Interfaces.bf` breaks verify 160/160 (`clean == files.len()`, verified `corpus.rs:108-110`). These files are dense with *supported-shaped* clauses (`where T : struct`, `: class`, `: IFace`, `: new`). | GC | (i) **per-file ratchet** keeps corlib interfaces (`IDisposable`/`IHashable`) out of scope → skipped; (ii) **any-base-unresolvable ⇒ Skip** transitive rule with a `HashSet<TypeId>` cycle guard; (iii) **overloaded-decl skip** (`MethodA<T>`×4 share `(name,arity)`); (iv) **defer `T : T2`** (the `[IgnoreErrors]` `MethodG` call); (v) **defer type-decl instantiation enforcement**; (vi) **body-first classification** (operator clauses on `float`/`char8` skipped before the name is read). Pinned by **GC-T1's `constraint_diags == 0` on all four files** landed FIRST (before any diagnostic-emitting task) + the corpus ratchet itself + **GC-T4's multi-file** co-analysis pin. |
| **R2 / R-A** | **CA `%struct.Type` slot-shift is verify-clean-but-wrong.** Extending the aggregate 8→10 across `set_body`/per-type-init/**sentinel** must move atomically; a missed site is a hard LLVM build-fail, but a *wrong field order* would pass the verify ratchet yet misread every reflection field at runtime. `corpus.rs` cannot detect a physical slot-shift. | CA (+ CR, which reads `%struct.Type` in the sandbox) | The two **layout pins** (`corlib_type_layout_matches_struct_type_aggregate` 8→10; new `corlib_attributeinfo_layout_matches_*`) are the deterministic detectors (catch drift WITHOUT running the JIT). CA-T2 lands all three aggregate sites + both pins in **one atomic change**; gate = "**9 existing `reflect_*.bf` stay green**" (the runtime slot-shift detector). CA *appends* (indices 8/9), so existing field indices — incl. the ones CR reads — are stable (force #4). |
| **R3** | **CA's five `TypeMeta` constructors.** The new non-defaulted `attributes` field breaks `lower.rs:5039` + `aot.rs:223` + `llvm/lower.rs:1900`/`:1909`/`:1957` (verified `TypeMeta` has no `attributes` today). A missed one fails `cargo test -p newbf-llvm` to compile. | CA | CA-T0 patches all five (push `attributes: vec![]`) and the gate is literally "`cargo test -p newbf-llvm` compiles." Optional `TypeMeta::new` constructor centralizes future adds. |
| **R4** | **CR single-evaluation / silent-decline footgun (CR's headline bug class).** If the relaxed wall lowers the arg then `return None`, the caller (`lower.rs:7657-7659`) re-lowers it → **double-emit** (a `new String` arg leaks one copy). If a non-String/non-literal declines into the empty `Compiler.EmitTypeBody(String)` stub, the emission is **silently dropped**. | CR | CR-T0 decides the branch from the AST *before* committing to emission (literal fast-path peeked first), lowers a `Ref(String)` arg **exactly once**, and emits a **diagnostic** (never a silent `None`) for anything else. Gate: a unit test asserts a side-effecting `String` builder arg is evaluated once (no duplicate alloc in IR) AND a non-String non-literal arg yields a diagnostic. |
| **R5** | **CR value-struct method-chain trap + class-field-0-is-header.** `GetField(i)` returns a value-struct `FieldInfo` by value (`struct_base` rejects a `Struct(id)` rvalue), so `typeof(T).GetField(0).GetName()` cannot chain. `String` is a class whose field 0 is the `%ClassVData` header, so `field_addr(body, id, 0)` for `mPtr` is off-by-one. `Length()` is i64; the shim wants i32; there is no `fb.trunc`. | CR | (a) **bind a `FieldInfo` local** before `.GetName()` in BOTH generator code and emitted runtime text; (b) read `Ptr`/`Length` via the **methods-table lookup** (the `append_to_string` pattern `lower.rs:10133`), never `field_addr`; (c) narrow with `self.coerce(len64, I64, I32)`, never a nonexistent `fb.trunc`. **CR-T1** JITs a sandbox-shaped `from_ir` module that exercises a value-struct `FieldInfo` return inside `$ct_emit_run` — the only pin for struct-by-value reflection in the sandbox (existing tests only cover the app JIT). |
| **R6** | **IT value-struct `this`-aliasing (MoveNext state lost) + exactly-once-Dispose-on-every-edge.** A value-struct enumerator's `MoveNext`/`Current`/`Dispose` mutate `mIndex`; `this` MUST be the `e_slot` alloca address reused across all calls — a reloaded copy discards the increment. `Dispose` must fire exactly once on normal/`break`/**`return`** exit; the Stomp guard does NOT catch a *missed* Dispose (a skipped call, not a double-free). | IT | Pass the `e_slot` **address** as `this` for value enumerators (the `Struct(id)` lvalue body-pointer arm of `struct_base`, `lower.rs:9583`); hand-build the three calls via a new `call_instance_on_ptr` helper (the auto-getter precedent `lower.rs:6036`), NOT `lower_method_call` (no Value-receiver entry exists). Register `Dispose` as a **scope-cleanup hook** in the loop's `scope_allocs` frame so `free_all_scopes` (return), `free_scopes_down_to` (break), and normal fall-off each run it once with no double-emit. `foreach_getenumerator.bf` (aliasing), `foreach_dispose_once.bf` (break), `foreach_dispose_return.bf` (return) pin it under Stomp. |
| **R7** | **IT first-of-kind generic value struct on the executable path.** The runnable corlib has **zero** generic value structs today (only generic classes); a monomorphized generic value struct with state-mutating instance methods, returned by value, copied into an alloca under Stomp, has never run. | IT | `enum_manual.bf` (IT-T0, `expect: 6`) proves the generic-value-struct ABI in **isolation** (manual `MoveNext`/`Current`, no `foreach`) BEFORE IT-T1 layers the loop on top — so a generic-value-struct miscompile surfaces in T0, not conflated inside the loop lowering. |
| **R8** | **IT walker audit — compiler does NOT enforce it.** Only `Stmt::span()` and `print.rs::stmt` are exhaustive; every sema/ownership `Stmt` walk is wildcard-terminated (verified: `collect_insts_stmt` `:2139`, `for_each_stmt_expr` `:3718`, `collect_lambdas_stmt` `:3616`, `ownership.rs:809`, …). A missed wildcard walker miscompiles `yield` SILENTLY. | IT | **IT-T2+3 ship together** (the AST variants are inert without the rewrite); the generator rewrite runs in `lower_program` BEFORE `collect_insts` + ownership, so those walks only ever see desugared `__yield.Add(...)`. T2+3 hand-edits each wildcard walker AND ships a focused `yield return (x => x)` / `new List<>()` test proving the lambda/mono collectors saw the yielded expr. The lowering `stmt` arm is a **diagnostic**, never `unreachable!`. |
| **R9** | **IT generator rewrite cannot fabricate `Span`-backed identifiers or mutate the borrowed AST in place.** `__yield`/`Add`/`new List<E>()` need `Span`s whose `.text(src)` equals those strings; the AST is borrowed immutably at lower time. | IT | Adopt the **comptime emission precedent** (`emit.rs:429-432`): re-emit each generator body as owned `String` source, re-parse with a fresh `FileId`, keep the `String` alive so spans stay valid; replace the method decl before `collect_insts`. A recursive statement walk preserves control flow (yield-in-loop stays in the loop). |
| **R10** | **CR comptime sandbox String double-free faults the COMPILER (not a leak).** The generator's `new String` object body routes through `newbf_alloc` → the Stomp ledger *during compilation* (`run_corpus.rs` under `GuardMode::Stomp`). A double-free/UAF in a generator faults the compiler (quarantine, no SEH recovery). A pure leak does NOT abort (run-corpus tolerates leaks, never calls `report_leaks`). | CR | §4 generators `delete s` **exactly once**; the buffer is freed by the dtor (`emit_destroy` runs the dtor despite the stale comment). Acceptance pins **"no double-free under Stomp"**, NOT "allocations balance." (The char buffer uses CRT malloc/free, invisible to the guard — only the object body is ledgered.) |
| **R11** | **CR fixpoint determinism / non-termination.** Emitted text must be byte-stable round-to-round or the `seen` dedup never converges and trips the round cap. | CR | Generators build text from a *stable* reflection iteration (declaration order, sorted by type-id); §4 emits a single idempotent member. The existing `MAX_EMIT_ROUNDS`(16)+byte-cap+dedup guard (CB-T5, landed `emit.rs:53`) is inherited unchanged. Monomorph generators deferred (CR §5). |
| **R12** | **GC termination (self-referential bounds).** `Generics.bf:79` `class Singleton<T> where T : Singleton<T>` and mutually-recursive bases would hang an unguarded transitive walk; termination is a HARD invariant the ratchet proves. | GC | Every transitive base/iface walk carries a `HashSet<TypeId>` visited guard and bails on revisit (GC §3.2, mandatory). |
| **R13** | **SSA dominance ("instruction does not dominate all uses").** IT's value-struct `this` slot + block structure, CR's `Ptr()`/`Length()`/`coerce` at the relaxed wall. (GC emits no IR → zero SSA surface; CA is `GlobalAddr` + const loads → trivially safe.) | IT, CR | IT copies the proven Count/Get head/body/cont/exit skeleton (`lower.rs:7058-7103`, verifies clean today); no value crosses a block edge except through allocas. CR emits straight-line IR inline at the use site (the receiver dominates); `typeof` is a constant `GlobalAddr`. Each gated by verify-corpus (the bug is a verifier failure). |
| **R14** | **sema ⊥ llvm HARD invariant.** | all four | Verified preserved by every home doc §2: GC emits only `Diagnostic`s; IT is pure sema emitting IR + named symbols + a parser/reparse; CR names only `__newbf_ct_emit`/`String`/`Ptr`/`Length` by name; CA emits owned IR `AttrMeta` data + names globals by convention. **No new `use newbf_llvm` in sema.** No new cross-crate Rust edge anywhere in the wave. |

**Tasks that need the riskiest review attention:** GC-T3 (the high-value
`Use<int32>` instantiation check — R1, the ratchet wall), CA-T2 (the atomic
3-site `%struct.Type` extension — R2/R-A), CR-T0 (the single-eval relaxed wall —
R4/R5) and CR-T1 (sandbox struct-by-value reflection — R5), IT-T1 (value-struct
`this` + exactly-once-Dispose under the guard — R6). **Lowest-risk
(behavior-preserving plumbing):** GC-T0/T1, IT-T0, CR-T0 (no-op until used),
CA-T0/T1.

---

## Sprint schedule

Six review-batches. Within a sprint, PARALLEL tasks have no ordering force;
SERIAL tasks must land in the listed order. The cadence remains one agent at a
time; a "sprint" groups tasks that share a gate or co-land.

### Sprint A — Open all four tracks (the behavior-preserving roots)
*Goal: land the keystone CR relax (no-op behind the literal fast-path), the IR
plumbing for CA, the pure-sema skeletons for GC, and IT's corlib/ABI root. All
behavior-preserving; all corpora unchanged. Demonstrable: existing corpora green;
CR's single-eval/diagnostic unit tests pass; `enum_manual.bf → 6`.*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **CR-T0** ★ | Relax `try_lower_emit_type_body` (`:9891`): literal fast-path + `Ref(String)` single-eval (methods-table `Ptr()`/`Length()`, `coerce(I64,I32)`) + diagnostic-not-silent-decline | comptime-reflection | — | SERIAL (do first — the critical-path keystone) |
| **CA-T0** | IR: `AttrMeta` + `TypeMeta.attributes` + patch all **5** `TypeMeta` constructors + `format_ir` | custom-attributes | — | PARALLEL with CR-T0 |
| **GC-T0** | Pin the concrete constrained-`T` dispatch as a non-regression (`constraint_iface_use`/`_class_bound`/`_struct_bound`) | generic-constraints | — | PARALLEL |
| **IT-T0** | Corlib top-level `ListEnumerator<T>` + `List<T>.GetEnumerator()` + `enum_manual.bf` (generic-value-struct ABI in isolation) | iterators | — | PARALLEL |

All four are roots touching disjoint regions (the EmitTypeBody seam / `newbf-ir`
`TypeMeta` / a new run-corpus dispatch pin / `newbf-corlib/bf/List.bf`). CR-T0 is
behavior-preserving (literal path untouched); CA-T0 is pure plumbing; GC-T0 pins
already-working monomorph dispatch; IT-T0 is additive corlib.

### Sprint B — Hard gates + skeletons (the substrate confirmations)
*Goal: pin CR's sandbox struct-by-value reflection (the hard gate), land GC's
skip-all classifier + the four-file ratchet pins, IT's foreach branch, CA's
collector. Demonstrable: CR-T1 sandbox test green; GC `constraint_diags == 0` on
4 files; `foreach_getenumerator.bf → 60`; CA collection compiles.*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **CR-T1** ★ | Sandbox-reflection confirmation: JIT a `from_ir` sandbox-shaped module, value-struct `FieldInfo` return inside `$ct_emit_run`; strip keeps corlib reflection, drops generator | comptime-reflection | CR-T0 | SERIAL after CR-T0 |
| **GC-T1** | Skeleton: `constraints.rs` pass in `analyze` after `check_delete_flow`; `type_by_name_arity` `(name,arity)` index; classify-all (no diagnostic yet); `constraint_diags` helper + `==0` pins on the 4 files | generic-constraints | GC-T0 | PARALLEL |
| **IT-T1** ★ | The fifth `ForEach` branch (GetEnumerator/MoveNext/Current/optional Dispose) + `call_instance_on_ptr` helper + value-struct `this` + exactly-once-Dispose scope hook | iterators | — (inline `Bag`, indep of IT-T0) | PARALLEL |
| **CA-T1** | Sema: `StructTable.type_attr_data` parallel vector (lockstep at 4 push-sites) + `attr_arg_const` collector (raw simple-name + folded args) | custom-attributes | CA-T0 | PARALLEL |

### Sprint C — First behavior: the CA ABI wall + GC/IT first diagnostics & slices
*Goal: the load-bearing `%struct.Type` extension (atomic), GC's first
diagnostics, IT's `yield` end-to-end. Demonstrable: CA layout pins 8→10 green + 9
`reflect_*.bf` still green; `violate_decl_contradiction.bf` diagnoses;
`yield_eager_basic.bf → 6`.*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **CA-T2** | ABI (atomic): `%struct.Type` 8→10 across **3** sites + sentinel; `%struct.AttributeInfo`; complete `AttributeInfo.bf`/`Attribute.bf`/`Type` accessors; 2 layout pins | custom-attributes | CA-T0 | SERIAL (the ABI wall; land in a different batch from CR-T0 per force #4) |
| **GC-T2** | Declaration-level enforcement: clause-internal `class`∧`struct` contradiction only | generic-constraints | GC-T1 | PARALLEL |
| **IT-T2+3** | `yield` AST variants + `Keyword::Yield` arm + forced `span()`/`print.rs` arms + **every** wildcard walker hand-edit + `rewrite_generators` eager-materialization (re-emit + re-parse, recursive) | iterators | — (lands together) | PARALLEL (moves the parser ratchet) |

### Sprint D — Feature mid-bodies: the high-value checks + CA data population
*Goal: GC's flagship `Use<int32>` instantiation check, CA resolve+densify into
`TypeMeta`, CR's count marquee. Demonstrable: `violate_iface.bf → 1` diagnostic;
CA `module.type_meta` populated; `comptime_reflect_field_count.bf → 2`.*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **CR-T3** ★ | Count marquee: `comptime_reflect_field_count.bf → 2` + `comptime_reflect_count_zero.bf → 7` (no T2 dep; `Append(int)` + literal auto-wrap) | comptime-reflection | CR-T0, CR-T1 | PARALLEL |
| **GC-T3** | Method-call instantiation enforcement (the `Use<int32>` check): re-walk bodies, primitive table, transitive implements/base walk + cycle guard, overloaded-decl skip | generic-constraints | GC-T2 | SERIAL after GC-T2 (RISKIEST GC node) |
| **CA-T3** | Sema: resolve simple-names → `StructId` + dense id in `assign_type_ids_and_meta`; skip markers; gate on FIELDS; push `AttrMeta` | custom-attributes | CA-T1, CA-T2 | SERIAL after CA-T2 |
| **CR-T2** | Corlib `String.Append(char8*)` overload (feeds CR-T4 only) | comptime-reflection | — | PARALLEL (before CR-T4) |
| **IT-T4** | IT journal + verify-corpus IR-shape pin + doc cross-link | iterators | IT-T2+3 | PARALLEL |

### Sprint E — Behavioral completions: emit the tables, name-driven emission, GC multi-file pin
*Goal: CA emits the AttributeInfo table (typeof surfaces it), CR's name-driven
emission, GC's configuration-dependence guard. Demonstrable:
`attr_present_typeid.bf → 1`, `attr_count_multi.bf → 2`;
`comptime_reflect_field_name.bf → 1`; GC multi-file co-analysis 0 diagnostics.*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **CR-T4** ★ | Name-driven emission: `comptime_reflect_field_name.bf → 1` (binds a `FieldInfo` local in emitted text; uses CR-T2's `Append(char8*)`) | comptime-reflection | CR-T0, CR-T1, **CR-T2** | SERIAL after CR-T2/T3 |
| **CA-T4** | LLVM: emit `[n×i64]` arg arrays + `AttributeInfo` consts (policy-gated) + set `mAttrCount`/`mAttributes` | custom-attributes | CA-T3 | SERIAL after CA-T3 |
| **GC-T4** | Multi-file ratchet-safety pin (co-analyze corlib-slice + `Constraints.bf`, assert 0) + wire positives/negatives into the suite | generic-constraints | GC-T3 | PARALLEL |

### Sprint F — Tails: args, docs, journals
*Goal: CA's scalar-arg surfacing, every feature's journal + doc cross-link.
Demonstrable: `attr_int_arg.bf → 42`, `attr_str_arg.bf → 1`; CR/CA/GC journal §§.*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **CA-T5** | Surface primitive + string ctor args (`GetIntArg`/`GetStrArg` end-to-end + a `ptrtoint`-of-cstr emission pin) | custom-attributes | CA-T4 | SERIAL after CA-T4 |
| **CR-T5** | Docs (`COMPTIME.md` + resolve reflection.md §10 "comptime reflection deferred" → "v1 landed, fields") + journal | comptime-reflection | CR-T0..T4 | PARALLEL |
| **GC-T5** | GC journal + doc cross-link | generic-constraints | GC-T4 | PARALLEL |
| **journals** | Per-feature journal §§ + verify-corpus pins + doc cross-links (CA) | all | feature tails | SERIAL after each feature tail |

---

## Per-task reference (id · title · feature · deps · seed · acceptance gate)

> Gate shorthand: **3 ratchets** = parser 160/160 + verify 160/160 + run-corpus
> 245 all-pass. A task lands only when the 3 ratchets **and** its own new gate
> are green. (IT raises the parser denominator; CA raises verify via the two new
> corlib structs; every behavior-changing task adds run-corpus programs.)

### comptime-reflection (home doc §7) — *the critical path; composes two landed subsystems*

- **CR-T0** ★ · *Relax `try_lower_emit_type_body` (the keystone, plumbing-heavy)* · deps: — ·
  *seed:* in `lower.rs` (the `:9891` `[Expr::Str(s)]` wall), keep the literal
  fast-path decided from the AST; else lower the arg **exactly once**, require
  `Ref(String)`, read `Ptr()`/`Length()` via the methods-table lookup (the
  `append_to_string` pattern `:10133`), narrow with `coerce(I64,I32)`, emit
  `__newbf_ct_emit(<owner>, ptr, i32 len)`; **diagnose** (don't silently decline
  into the stub) anything neither literal nor `String`. ·
  *gate:* (a) a sema unit test: a non-literal `String` lowers to `call
  __newbf_ct_emit(i32, ptr, i32)` with no residual `EmitTypeBody`; (b) a
  side-effecting builder arg is evaluated **once** (no dup alloc in IR) and a
  non-String/non-literal arg yields a **diagnostic**; (c) **all corpora unchanged**
  incl. `comptime_emit_member.bf → 42`. *Behavior-preserving (no-op until used).*

- **CR-T1** ★ · *Sandbox-reflection confirmation (a HARD gate)* · deps: CR-T0 ·
  *seed:* `emit.rs` integration test driving `run_emission` over a generator that
  calls `typeof(T).GetFieldCount()` **and binds a `FieldInfo` local** for
  `GetField(0).GetName()`; assert final module JIT-links clean, corlib
  `Type`/`FieldInfo` survive the strip, generator + `__newbf_ct_emit` are gone.
  Companion unit test JITs a `from_ir` **sandbox-shaped** module: look up
  `__newbf_type_by_id` + a `Type` global AND run a wrapper exercising a
  value-struct `FieldInfo` return inside `$ct_emit_run`. ·
  *gate:* both pass — pins struct-by-value reflection present+callable in the
  sandbox (R5), not just the app JIT.

- **CR-T2** · *Corlib `String.Append(char8*)` (feeds CR-T4 only)* · deps: — ·
  *seed:* add `public void Append(char8* s)` to `String.bf` (NUL-terminated copy,
  mirroring the `String(char8*)` ctor). ·
  *gate:* `string_append_cstr.bf → 1`; existing `append_overload.bf → 5427` +
  `string_append_int.bf → 1591` stay green; verify clean.

- **CR-T3** ★ · *Count marquee (run-corpus)* · deps: CR-T0, CR-T1 ·
  *seed:* land `comptime_reflect_field_count.bf` (**expect: 2**) +
  `comptime_reflect_count_zero.bf` (**expect: 7**); both use `Append(int)` +
  literal auto-wrap (NO CR-T2 dep). ·
  *gate:* both pass under JIT/Stomp; final module JIT+AOT-links clean; an
  integration test asserts the generator runs under Stomp with **no double-free**
  (R10 — not "balance").

- **CR-T4** ★ · *Name-driven emission (run-corpus)* · deps: CR-T0, CR-T1, **CR-T2** ·
  *seed:* land `comptime_reflect_field_name.bf` (**expect: 1**) using
  `Append(char8*)`; emitted runtime text **binds a `FieldInfo` local** before
  `.GetName()` (R5). ·
  *gate:* passes under Stomp; a `dump-ir` golden shows the emitted predicate
  member present, generator + `__newbf_ct_emit` absent.

- **CR-T5** · *Docs + journal* · deps: CR-T0..T4 ·
  *seed:* cross-link from `COMPTIME.md` + resolve `reflection.md` §10 "comptime
  reflection deferred" → "v1 landed, fields"; journal §. ·
  *gate:* docs build; journal references the T3/T4 corpus values.

### custom-attributes (home doc §7) — *additive table on the landed reflection pipeline*

- **CA-T0** · *IR: `AttrMeta` + `TypeMeta.attributes`* · deps: — ·
  *seed:* add `AttrMeta { attr_type_id: u32, args: Vec<Const> }` +
  `TypeMeta.attributes: Vec<AttrMeta>` (`module.rs:97`); patch **all five**
  `TypeMeta` constructors (`lower.rs:5039`, `aot.rs:223`, `llvm/lower.rs:1900`/
  `:1909`/`:1957`) to push `attributes: vec![]`; extend `format_ir` (NOT
  `format_reflection` — §4 golden note). Optional `TypeMeta::new`. ·
  *gate:* `cargo build` + `cargo test -p newbf-llvm` **compile** (proves all 5
  patched); verify+run **unchanged**; IR golden updated, reflection golden
  untouched.

- **CA-T1** · *Sema: `type_attr_data` + `attr_arg_const` collector* · deps: CA-T0 ·
  *seed:* parallel `StructTable.type_attr_data: Vec<Vec<AttrDataRaw>>` (template
  `policies` `:277`), empty-push at the 3 synthetic minters (`:865`/`:835`/`:2829`)
  + collected-push **inside the dedup guard** at `register_type_struct`
  (`:2514`/`:2538`) + extend the lockstep assert (`:574`); `attr_arg_const` (reuse
  `const_field_init` `:12446`, nested `Unary{Neg}` shape, + a `Str` arm via
  `decode_string_literal`, `None` outside Int/Bool/Char/Str). ·
  *gate:* compiles; verify+run unchanged (collected, unread). Content assertion
  deferred to CA-T3.

- **CA-T2** · *ABI: `%struct.Type` (3 sites) + `AttributeInfo` + corlib (atomic, largest)* · deps: CA-T0 ·
  *seed:* extend `%struct.Type` 8→10 at **all three** sites — `set_body`
  (`:408`), per-type init (`:528`), **sentinel `unknown_init` (`:558`)** — with
  `i32 mAttrCount, ptr mAttributes`; bump `corlib_type_layout_matches_*` 8→10
  (`:13954`); add `Type.bf` fields; define `%struct.AttributeInfo`; add complete
  `AttributeInfo.bf` (struct + all accessors) registered **before** `Type.bf`; add
  `Attribute.bf` empty base **class**; add `Type.GetCustomAttributeCount`/
  `GetCustomAttribute`; add the AttributeInfo layout pin. ·
  *gate (the ABI wall, R2/R-A):* both layout pins green (Type **10** fields,
  AttributeInfo `{i32,i32,ptr}`); `Attribute.bf`/`AttributeInfo.bf` verify
  standalone; **verify 160/160** and **all 9 `reflect_*.bf` green** (`mAttributes`
  null everywhere — no behavior yet).

- **CA-T3** · *Sema: resolve + densify into `TypeMeta.attributes`* · deps: CA-T1, CA-T2 ·
  *seed:* in `assign_type_ids_and_meta` (`:4967`), per reflectable type read
  `type_attr_data`, skip markers (`Reflect`/`AlwaysInclude`/`Comptime`/
  `EmitGenerator`/`Intrinsic`/`LinkName`), resolve via `by_name`+`type_id_of`
  (skip unresolved/value-struct attrs), gate on `policy.has(FIELDS)` (`:5003`),
  push `AttrMeta`. ·
  *gate:* a sema unit test: `[Reflect, MyAttr] class C` (MyAttr a class) →
  `attributes == [AttrMeta{attr_type_id: <dense>, args: []}]`; unmarked → `[]`.
  Goldens unchanged (data exists, not yet emitted).

- **CA-T4** · *LLVM: emit the AttributeInfo table* · deps: CA-T3 ·
  *seed:* in `emit_metadata`, per `AttrMeta` emit `[n×i64]` arg array (Int/Bool →
  widened i64; Str → `ptrtoint`/`const_to_int` of `emit_cstr`; `unreachable!`
  else) + `AttributeInfo` const, policy-gated `[k×%struct.AttributeInfo]` global,
  set `mAttrCount`/`mAttributes` in the per-type Type init. ·
  *gate:* `attr_present_typeid.bf → 1`, `attr_strip_vs_marked.bf → 1`,
  `attr_count_multi.bf → 2` pass; the strip pin (no array for unmarked) passes;
  verify + 9 `reflect_*.bf` green.

- **CA-T5** · *Args: surface primitive + string ctor args* · deps: CA-T4 ·
  *seed:* validate end-to-end: `attr_arg_const` folds ctor args, `[n×i64]` lands
  them, `GetIntArg(i)` reads i64, `GetStrArg(i)` `inttoptr`s; add a
  `ptrtoint`-of-cstr emission pin. ·
  *gate:* `attr_int_arg.bf → 42`, `attr_str_arg.bf → 1` (`GetStrArg` +
  `Internal.StrEq`).

### generic-constraints (home doc §7) — *pure sema, zero IR (the safe parallel track); HIGH ratchet exposure*

- **GC-T0** · *Pin concrete constrained-`T` dispatch (non-regression)* · deps: — ·
  *seed:* add/confirm `constraint_iface_use.bf` (or reuse `interface_constraint.bf`)
  + `constraint_class_bound.bf` (**7**) + `constraint_struct_bound.bf` (**9**).
  **No `new T()` program** (verified `new T()` on a bare type-param doesn't lower). ·
  *gate:* the 3 run-corpus pass; `interface_constraint.bf → 100`; 3 ratchets
  green. *Behavior-preserving.*

- **GC-T1** · *Skeleton (skip-all classifier) + the ratchet-safety pin FIRST* · deps: GC-T0 ·
  *seed:* `constraints.rs` + `check_generic_constraints(files, &graph, &interner)`
  wired into `analyze` after `check_delete_flow` (`ownership.rs:112` mirror); build
  `type_by_name_arity` `(name,arity)` from `graph.types`; **recognize** every form
  (supported+deferred), emit **no** diagnostic (body-first classification); land
  the root-parameterized `constraint_diags` helper + assert `== 0` on
  `Constraints.bf`/`Generics.bf`/`Generics2.bf`/`Interfaces.bf`. ·
  *gate:* verify 160/160 (no-op); classifier-label unit test; the four `== 0`
  pins. *Behavior-preserving. (The pin lands BEFORE any diagnostic-emitting task —
  guards GC-T2/T3 from their first diagnostic with a precise per-file signal.)*

- **GC-T2** · *Declaration-level enforcement (kind contradiction only)* · deps: GC-T1 ·
  *seed:* diagnose a parameter constrained both `class` and `struct` across one
  decl's clauses (bare keyword paths only); skip everything else. **No
  "non-generic-parameter name" check** (would fire on `where float : operator …`). ·
  *gate:* verify 160/160 (the 4 `== 0` pins hold); `violate_decl_contradiction.bf`
  once; `satisfied_no_diag.bf → 0`.

- **GC-T3** · *Method-call instantiation enforcement (the `Use<int32>` check) — RISKIEST* · deps: GC-T2 ·
  *seed:* re-walk bodies (like `check_delete_flow`) recording
  `(decl_name, arity, [arg_type_names], span)` for `Name<Args>(…)`/`Recv.Name<Args>(…)`;
  add the primitive-fact table; validate each supported constraint via the
  transitive implements/base walk **with a `HashSet<TypeId>` cycle guard**; **skip**
  any call whose `(name,arity)` matches >1 decl (overloads), any unresolvable
  arg/constraint/base, all deferred forms; one diagnostic per provable violation.
  Type-position instantiations NOT walked. ·
  *gate:* `violate_iface.bf` (primitive `int32`), `violate_class_constraint.bf`,
  `violate_struct_constraint.bf` each diagnose **once** (via `constraint_diags`,
  direct `analyze` — NOT run-corpus); `satisfied_no_diag.bf → 0`; **all 4 ratchet
  files → 0** (incl. `MethodA`×4 overloads, `[IgnoreErrors]` `MethodG`, `int*`,
  operator/const/array/generic-iface clauses); verify 160/160.

- **GC-T4** · *Multi-file ratchet-safety pin (configuration-dependence guard)* · deps: GC-T3 ·
  *seed:* co-analyze `corlib-slice/*.bf` **with** `Constraints.bf` in one `analyze`
  call, assert 0 constraint diagnostics (covers `IDisposable`/`IHashable` becoming
  in-program); wire the positives + negatives into the suite. ·
  *gate:* multi-file 0 diagnostics; the 3 positives + 5 negatives assert their
  exact counts; full 3 ratchets green. *Behavior-preserving (test-only).*

- **GC-T5** · *Journal + doc cross-link* · deps: GC-T4 ·
  *gate:* journal entry present; gates green.

### iterators (home doc §7) — *the only feature that touches the parser; first-of-kind generic value struct*

- **IT-T0** · *Corlib `ListEnumerator<T>` + `List<T>.GetEnumerator()` + manual proof* · deps: — ·
  *seed:* add the **top-level** generic `struct ListEnumerator<T>` (NOT nested —
  `index_generic_decls` is member-blind, verified `:655-692`) + `GetEnumerator()`
  to `List.bf`; do **not** change `ForEach`; add `enum_manual.bf` (manual
  `MoveNext`/`Current`, no foreach). ·
  *gate:* `enum_manual.bf → 6` under JIT/Stomp; verify+run green with the new
  corlib members. *Proves the generic-value-struct ABI in isolation (R7). Additive.*

- **IT-T1** ★ · *The fifth `ForEach` branch — RISKIEST* · deps: — (inline `Bag`, indep of IT-T0) ·
  *seed:* in `Stmt::ForEach` (the lowering arm), after the Count/Get probe fails
  (the `if let Some(...) = sigs` has no `else`; `coll`/`coll_ty` in scope), probe
  `Ref(id)`/`Struct(id)` for `GetEnumerator()` (`.cloned()`), take `eid` from
  `ge.ret`, probe `MoveNext`/`get_Current`/optional `Dispose`; add
  `call_instance_on_ptr` helper; materialize a value-struct/rvalue receiver into an
  alloca for `GetEnumerator`'s `this`; emit head/body/cont/exit passing the
  `e_slot` **address** as `this` for value enumerators; register `Dispose` as a
  scope-cleanup hook (normal/`break`/`return`). ·
  *gate:* `foreach_getenumerator.bf → 60`, `foreach_enum_break.bf → 30`,
  `foreach_dispose_once.bf → 1`, `foreach_dispose_return.bf → 1` under JIT/Stomp;
  the **5 existing foreach programs unchanged** (named); verify 160/160. *R6 — the
  value-struct `this`-aliasing + exactly-once-Dispose under the guard.*

- **IT-T2+3** · *`yield` AST + walker edits + eager-materialization rewrite (landed together)* · deps: — ·
  *seed (T2):* `Stmt::YieldReturn`/`YieldBreak` in `ast.rs`, a `Keyword::Yield` arm
  in `stmt()`, the forced `span()`/`print.rs` arms, hand-edited arms in **every**
  wildcard sema/ownership walker (`for_each_stmt_expr` `:3718`, `collect_insts_stmt`
  `:2139`, `collect_lambdas_stmt`, `register_tuples_in_stmt` `:951`,
  `collect_mixins_stmt`, `caps_stmt`, `ownership.rs` `:809`), a **diagnostic** (not
  `unreachable!`) lowering arm.
  *seed (T3):* `rewrite_generators` in `lower_program` **before** `collect_insts` +
  ownership: detect yield-bearing `List<E>`-returning methods, **re-emit the body
  as owned source + re-parse with a fresh `FileId`** (`emit.rs:429-432` precedent),
  recursive rules (prepend `List<E> __yield = new List<E>();`; `yield return e` →
  `__yield.Add(e);` in situ; `yield break` → `return __yield;`; trailing `return
  __yield;`). ·
  *gate:* `yield_eager_basic.bf → 6`, `yield_break.bf → 3`, `yield_empty.bf → 0`;
  a parser-corpus `yield` fixture round-trips (**parser ratchet moves**); a focused
  test proving a lambda/generic call inside `yield return …` is collected;
  non-generators + verify 160/160 unchanged. *R8/R9 — merged because the AST
  variants are inert without the rewrite.*

- **IT-T4** · *Journal + doc cross-link + verify pin* · deps: IT-T2+3 ·
  *seed:* journal entry; a verify-corpus fixture mirroring `foreach_getenumerator.bf`
  (pin the loop IR shape); cross-link. ·
  *gate:* journal present; verify count incremented + green.

---

## Recommended execution order (single reviewer, one agent at a time)

A linearization of the DAG keeping every commit behind green gates and minimizing
context-switching. Critical-path tasks marked ★.

1. **CR-T0** ★ — relax the EmitTypeBody wall (single-eval + diagnostic). The
   wave's keystone: longest chain, most bug-dense, behavior-preserving (no red
   window). **Do this first.**
2. **CA-T0**, **GC-T0**, **IT-T0** — open the three other tracks (CA IR plumbing,
   GC dispatch pin, IT corlib enumerator). All behavior-preserving, easy to review
   in isolation.
3. **CR-T1** ★ — the sandbox struct-by-value reflection hard gate (the one thing
   the existing app-JIT tests don't cover).
4. **GC-T1**, **IT-T1** ★, **CA-T1** — GC skip-all classifier + the four-file `==0`
   pins (lands the ratchet guard before any diagnostic), the IT foreach branch (the
   behavioral core), CA's collector.
5. **CA-T2** — the `%struct.Type` 8→10 ABI wall (atomic 3 sites + sentinel + 2
   layout pins). **Land in a different review batch from CR-T0** (force #4) — by now
   CR-T0 is several batches back, so the run-corpus signals never confound. Gate:
   9 `reflect_*.bf` green (the slot-shift detector).
6. **GC-T2** — declaration-level contradiction diagnostic.
7. **IT-T2+3** — the `yield` slice (the only parser-ratchet move).
8. **CR-T2** — `String.Append(char8*)` (feeds CR-T4).
9. **CR-T3** ★ — the count marquee (`comptime_reflect_field_count → 2`; no T2 dep).
10. **GC-T3** ★(for GC) — the high-value `Use<int32>` instantiation check (RISKIEST
    GC node — the ratchet wall). **CA-T3** — resolve+densify into `TypeMeta`.
11. **CA-T4** — emit the AttributeInfo table (`attr_present_typeid → 1`,
    `attr_count_multi → 2`). **CR-T4** ★ — name-driven emission
    (`comptime_reflect_field_name → 1`; needs CR-T2). **GC-T4** — the multi-file
    ratchet-safety pin.
12. **CA-T5** — surface scalar args (`attr_int_arg → 42`, `attr_str_arg → 1`).
13. **IT-T4**, **CR-T5**, **GC-T5** — journals + verify pins + doc cross-links.

> **Earliest demoable state:** after step 5 you have a `%struct.Type` that can
> carry attributes (no behavior yet) AND a comptime generator that can reflect in
> the sandbox. After step 9 you have the first reflection-driven codegen
> (`comptime_reflect_field_count → 2`) — the marquee Wave-3 capability — plus
> `foreach` over user types and the eager `yield`. Steps 10–12 turn each into a
> full feature (the `Use<int32>` diagnostic, attribute args end-to-end,
> name-driven emission).

---

## What was NOT sequenced (deferred / next-wave)

Each home doc's explicit deferrals carry no tasks here, by design:

- **generic-constraints** (GC §5): operator constraints, `const T`/const-generic,
  `delete`, array/sized (`T : StringView[C]`), pointer-suffixed kinds (`T : struct*`),
  **generic-interface** constraints (`T : IEnumerator<TElement>` — the very coupling
  the prompt anticipated with iterators, out of *both* v1s), primitive-name bounds
  (`T : float`), `T : T2` (the `[IgnoreErrors]` `MethodG` case), type-declaration-level
  `where` instantiation enforcement, and **Appendix-A abstract-`T` interface dispatch**
  (currently *unreachable* — `targ_is_abstract` `:1899` refuses abstract type-args).
- **iterators** (IT §5): the **lazy/coroutine state-machine `yield`** (the genuinely
  hard cross-yield-local-spill transform, the SSA-dominance trap class — v1's eager
  path *changes semantics*: no laziness, no infinite sequences),
  `IEnumerator<T>`/`IEnumerable<T>` interface-typed enumerators + dynamic dispatch
  (blocked on generic-interface monomorphization), heap-enumerator auto-`delete`,
  typed/pattern `foreach` bindings, generator return-type inference.
- **comptime-reflection** (CR §5): comptime **method**/attribute reflection +
  `GetMethods()`-driven dispatch generation, **generic-T** reflection (lift the
  generic-comptime guard), field-*value* serialization by offset, reflecting
  emitted-this-round members, `typeof` on the value-fold path.
- **custom-attributes** (CA §5/§8): the **value-`struct` attribute type** (the
  headline follow-on — every idiomatic Beef attribute is a `struct`, but only
  `StructKind::Ref` classes get a dense id, `:4972`; extending `reflectable` churns
  the name-sorted dense-id space + the reflection golden), generic annotated types
  (`register_mono` drops attributes + hardcodes `TYPE` policy, `:865`),
  `GetCustomAttribute<T>()` returning a *constructed* instance (sandbox can't return
  a struct, `eval.rs:107`), **comptime-reflection composition** (`[Comptime]` reading
  attributes — blocked by BOTH the struct-return wall AND the no-`Type`-in-comptime
  boundary), float/non-scalar args, `: Attribute` base + `AttributeUsage`/`AttributeTargets`
  enforcement, member/parameter-level attribute reflection, a dedicated
  `ReflectPolicy::ATTRIBUTES` bit, an `attrs=` column in `format_reflection`.

**The natural next-wave merge points** — each becomes tractable once Wave 3's
substrates exist:
- **`[Comptime]` reflecting a decl's user attributes → attribute-driven codegen**
  (custom-attributes × comptime-reflection) — CA lands the `mAttributes` table; CR
  lands reflection-driven emission; the merge needs comptime *attribute* reads
  (lifting CA §5's deferral) + a sandbox attribute view. **This is the coupling the
  MEMORY-prompt anticipated; it is explicitly out of both v1s and is the single most
  obvious Wave-4 target.**
- **Generic-interface enumerators** (`IEnumerator<T>`) — needs generic-interface
  monomorphization (out of both iterators and generic-constraints v1); unlocks both
  GC's generic-iface constraints AND IT's interface-typed enumerators at once.
- **Comptime *method*/attribute reflection** (CR §5) composed with custom-attributes
  → `[Reflect(.Methods)]`-driven dispatch generation.
- **Value-`struct` reflection** (CA §8) — extend `reflectable` to mint dense ids for
  attribute-typed (and general value-) structs; unblocks idiomatic Beef attributes.
