# NewBF — Cross-Feature Sprint Plan (4-Feature Wave)

*Drafted 2026-06-06. Sequences the agent-assignable tasks from the four
design docs in [`docs/design/`](.) — [`itables.md`](itables.md),
[`targeted-args.md`](targeted-args.md),
[`generic-methods.md`](generic-methods.md),
[`fn-values.md`](fn-values.md) — into one schedule. Companion to the
12-phase [`PLAN.md`](../../../PLAN.md) and the original
[`SPRINTS.md`](../../../SPRINTS.md). This wave lives inside PLAN.md phases
5–9 (types/dispatch, generics, comptime breadth): all four features are
dispatch/representation refinements on the existing JIT pipeline.*

## Preamble — cadence and invariants

Cadence is unchanged from `SPRINTS.md`: **one developer (you), one agent
at a time, review/test/commit per task.** Each task is a single
agent-assignable unit that lands behind green gates. A sprint here is a
*review batch* — a small set of tasks that should be reviewed and merged
as a group because they share a gate or co-land atomically.

**The three standing gates, green at every task boundary** (from MEMORY /
the design docs):

- **Verify corpus** — `newbf-sema/tests/corpus.rs`, **152/152** LLVM-clean
  (a 100% ratchet: adding a feature-suite file raises the denominator).
- **Parser corpus** — `newbf-parser/tests/corpus.rs`, **152/152**
  zero-diagnostic (same ratchet).
- **Run corpus** — `tests/newbf-tests/tests/run_corpus.rs`, ~160 programs,
  JIT-run, full-i32 value check. **This is the authoritative behavioral
  gate.** Two features (fn-values, itables) produce *verify-clean
  miscompiles* the verify corpus cannot catch — for those, run-corpus is
  the only real gate.

**8-bit exit-code caveat:** AOT exit codes truncate to 8 bits; all
value-checks use the JIT run-corpus harness, which reads the full i32.

**Naming convention in this plan.** Each task keeps its *home-doc id* as a
suffix so it is traceable: `GM-A1` = generic-methods task A1, `TA-3` =
targeted-args task 3, `FV-T3` = fn-values task T3, `IT-T1` = itables task
T1.

---

## Cross-feature dependency analysis

Each feature is internally a near-linear chain (its tasks depend on the
prior task). The cross-feature couplings, verified against the design
docs, are:

1. **generic-methods owner-mangling unblocks the most.** generic-methods
   §6 explicitly names itself the prerequisite for the other three:
   - *fn-values*: "a method-ref to `obj.Map<R>` resolves to the
     owner-mangled symbol — owner-mangling is the prerequisite."
   - *itables*: "generic interface methods need per-(class,interface)
     symbols; owner-mangling provides uniqueness."
   - *targeted-args*: "argument-position generic-method refs resolve a
     unique signature via the composite key."
   - Crucially it adds: **"None block v1."** The couplings are
     *enablers for the advanced corners*, not hard compile-time
     dependencies. So **GM-A1/A2 (the mechanical re-key + owner
     determination) should land first** — it is the cheapest, highest-fanout
     change and removes the §107 collision class — but the *v1 slices* of
     the other three do not import generic-method symbols and can proceed
     in parallel once GM-A1/A2 is in.

2. **fn-values ↔ generic-methods share the corlib HOF surface.**
   fn-values' marquee test (`closure_arg.bf`) drives the **generic** `Map`
   (fn-values §5.3: "must be verified by `closure_arg.bf` driving the
   *generic* `Map`"). generic-methods B2 migrates `List<T>` HOF to
   *instance* generic methods (`xs.Map<R>(f)`). These touch the same code.
   **Sequence fn-values' Slice A (the §49 fix) before generic-methods B1/B2**
   so the HOF migration lands on a correct function-value convention. GM
   B1/B2 is the *last* thing in the wave.

3. **targeted-args ↔ fn-values overlap on inline-lambda-as-arg.**
   targeted-args is about *target-typed dot-forms* as args; fn-values
   T6a/T6b is about *inline lambdas* as args. Both extend the call-arg
   lowering path and both touch `collect_*` pre-passes
   (targeted-args §6 `collect_insts_expr`; fn-values §6
   `collect_lambdas_stmt`). They are **independent in mechanism** (dot-form
   classification vs lambda-param target-typing) but **collide in the same
   files/functions**. To avoid two agents editing `lower_method_call`'s arg
   loop simultaneously: land **targeted-args' core (TA-1..TA-3, which
   restructures the arg loop) before fn-values' inline-lambda slice
   (FV-T6a/b)**. fn-values Slice A (T1–T4) does *not* touch the arg-loop
   structure (it changes the call-through-a-name path and the producer/coerce
   path), so it is safe to run in parallel with targeted-args.

4. **itables is the most independent feature.** itables §10 defers generic
   interface methods and the default-via-constraint path; its v1 touches
   `StructKind`, vtable composition, and dispatch — **no overlap** with the
   arg-loop (targeted-args) or the function-value representation
   (fn-values). Its only soft tie is "generic interface methods need
   owner-mangling," which is explicitly out of v1. **itables can run as a
   fully parallel track start-to-finish**, gated only by its own chain.

**Net ordering forces:**
- GM-A1/A2 first (highest fanout, cheapest, unblocks the collision class).
- fn-values Slice A (the §49 segfault fix) early — it is a *correctness*
  fix and a prerequisite for the HOF migration.
- targeted-args core before fn-values inline-lambda slice (shared arg-loop).
- generic-methods B1/B2 last (depends on fn-values correctness + GM-A3).
- itables in parallel throughout.

---

## The critical path

```
GM-A1 → GM-A2 → GM-A3a → GM-A3b → GM-A4 → GM-B1 → GM-B2
                                          ↑
                          (FV Slice A: FV-T1→T2→T3+T4 must be green
                           before GM-B1/B2 — HOF migration needs a
                           correct function-value convention)
```

The **longest dependency chain** is **generic-methods**:
`GM-A1 → A2 → A3a → A3b → A4 → B1 → B2` (7 serial tasks), and **GM-B1/B2
additionally wait on fn-values Slice A** (4 serial tasks: FV-T1 → T2 →
T3+T4). So the critical path is:

> **GM-A1 → GM-A2 → GM-A3a → GM-A3b → GM-A4 → (join with FV-T1→FV-T2→FV-T3+T4) → GM-B1 → GM-B2**

Everything else (itables T1–T8, targeted-args 1–9, fn-values T5–T8) hangs
off this spine and has slack. **The single most-unblocking task is
`GM-A1`** (mechanical re-key + `cur_type` plumbing): it is a verified
no-op, it is the root of the longest chain, and the owner-mangling it
introduces is the named prerequisite for the advanced corners of all three
other features.

---

## Sprint schedule

Eight review-batches. "PARALLELIZABLE" within a sprint = independent tasks
assignable to different agents at once; "SERIAL" = must land in the listed
order. Since you review one agent at a time, *parallelizable* means "no
dependency forces an order — interleave them however your review queue
prefers," not "run two agents literally at once."

### Sprint A — Foundations: open the two independent tracks
*Goal: land the cheapest highest-fanout change (owner-mangling no-op) and
open the fully-independent itables track. Demonstrable: the §107 collision
disappears once A2 lands; itables registration is verify-clean.*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **GM-A1** | Mechanical re-key (no-op) + `cur_type` plumbing | generic-methods | — | SERIAL (do first) |
| **IT-T1** | Register interfaces as `StructKind::Interface` + base-routing guard | itables | — | PARALLEL with GM-A1 |
| **FV-T1** | Register `$Func` first + `lower_value_ty` + position gating | fn-values | — | PARALLEL with GM-A1 |
| **TA-1** | Arg classification + `lower_arg_targeted` + `Ref`-initializer fix + try-order unification | targeted-args | — | PARALLEL with GM-A1 |

All four are roots (no deps). They touch disjoint regions (`mangle_*` /
`StructKind` / `$Func` registration / `arg_is_pending`+try-order), so any
review order is safe.

### Sprint B — First behavior: collisions fixed, infra in place
*Goal: GM owner-mangling becomes real (collision fixed); the per-feature
resolution infrastructure lands. Demonstrable:
`generic_method_collision.bf → 42`.*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **GM-A2** | Owner determination: static/bare/qualified + collision fix | generic-methods | GM-A1 | SERIAL after GM-A1 |
| **IT-T2** | Capture interface bases; populate imethods/idefaults/explicit_impls | itables | IT-T1 | PARALLEL |
| **TA-2** | Shape-gated `pick_overload_partial`/`ctor_for_partial` + phase-1/finish helpers | targeted-args | TA-1 | PARALLEL |
| **FV-T2** | Single `$self`-leading lambda convention (env layout unchanged) | fn-values | FV-T1 | PARALLEL |

### Sprint C — Dispatch composition + first call-site wiring
*Goal: each feature reaches the point just before (or at) its first real
dispatch. Demonstrable: `targ_ctor_arg.bf → 25` (targeted-args' first
working dot-form arg).*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **IT-T3** | Compose itables into class vtables (`apply_itables`, slot-base, padding, diagnostics) | itables | IT-T2 | PARALLEL |
| **GM-A3a** | Collector local/field type scope + instance-receiver resolution | generic-methods | GM-A2 | PARALLEL |
| **TA-3** | Wire instance/static/base `lower_method_call`; decide Tuple | targeted-args | TA-1, TA-2 | PARALLEL |

### Sprint D — The two atomic correctness landings
*Goal: the two "must co-land" slices — fn-values Slice A (the §49 segfault
fix) and itables dispatch — go in. Demonstrable: `closure_arg.bf` (the §49
crash) now returns its expected value; `iface_dispatch_basic.bf → 9`.*

> **FV-T3 + FV-T4 ARE A SINGLE ATOMIC COMMIT** (fn-values §9: T3 without
> T4 breaks `function_pointer.bf`; the call-site rewrite + env re-layout
> must be simultaneous or `closure_basic` breaks). Review them as one unit.

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **FV-T3+T4** | `Func$` producers/consumers/uniform call site + env re-layout **and** static method-ref thunks (ONE commit) | fn-values | FV-T1, FV-T2 | SERIAL (atomic pair) |
| **IT-T4** | Interface receivers reach `struct_base`; upcast confirmed free | itables | IT-T3 | PARALLEL |
| **GM-A3b** | Instance generic-method dispatch (call + emission) | generic-methods | GM-A3a | PARALLEL |

### Sprint E — Feature mid-bodies
*Goal: each feature's main behavioral payload. Demonstrable: 8 new itable
run-corpus programs pass; bound method-refs work; targeted-args covers
new-expr / value-struct / generic / enum-payload args.*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **IT-T5** | Itable dispatch at call site + first 8 run-corpus programs | itables | IT-T4 | PARALLEL |
| **GM-A4** | Guards & negatives hardening (virtual/comptime/inherited diagnostics) | generic-methods | GM-A3b | PARALLEL |
| **FV-T5** | Bound instance method-ref thunks | fn-values | FV-T4 | PARALLEL |
| **TA-4** | Wire bare-name / free-fn / fn-ptr calls; diagnose pending→external | targeted-args | TA-1, TA-2 | PARALLEL |
| **TA-5** | Wire `new T(args)` | targeted-args | TA-1, TA-2 | PARALLEL |

### Sprint F — Feature completions (defaults, is/as, nested, inline lambdas)
*Goal: finish itables (defaults + is/as), finish the targeted-args wiring,
and land fn-values' inline-lambda slice (now that the arg loop is settled
by TA-3). Demonstrable: `iface_default_method.bf → 100`, `iface_is_as.bf`,
`lambda_direct_arg.bf`.*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **IT-T6** | Default interface methods | itables | IT-T5 | PARALLEL |
| **IT-T7** | is/as/inheritance against interfaces | itables | IT-T6 | SERIAL after IT-T6 |
| **TA-6** | Wire value-struct `.(args)` incl. nesting | targeted-args | TA-1, TA-2, TA-5 | PARALLEL |
| **TA-7** | Wire generic method calls + nested-mono collection | targeted-args | TA-1, TA-2 | PARALLEL (needs GM-A3b landed) |
| **FV-T6a** | Collect inline lambdas in call-arg position | fn-values | FV-T3 (+ TA-3 landed) | SERIAL before FV-T6b |
| **FV-T6b** | Target-type inline-lambda params from resolved sig | fn-values | FV-T6a | SERIAL after FV-T6a |

> TA-7 (generic-method args) needs GM-A3b on the branch so the
> generic-method call path exists; FV-T6a/b should follow TA-3 so the
> arg-loop is already restructured (see dependency analysis #3).

### Sprint G — Tails, returns, variadics, generic-on-generic enabler
*Goal: the last per-feature behavior + the GM-B1 enabler that powers the
corlib payoff. Demonstrable: `closure_returns_fn.bf`,
`targ_variadic_arg.bf`, `generic_method_on_generic.bf`.*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **TA-8** | Wire enum-payload args + variadic + ref/out | targeted-args | TA-1, TA-2, TA-3 | PARALLEL |
| **FV-T7** | `closure_returns_fn` + by-value lifetime/semantics docs | fn-values | FV-T3 | PARALLEL |
| **GM-B1** | Generic methods on generic owners | generic-methods | GM-A3b, GM-A4, **FV Slice A** | SERIAL (critical path) |

> **GM-B1 is the only place the fn-values↔generic-methods coupling
> bites:** B1 emits instance-generic monomorphs on generic owners
> (`List<int64>.Map<R>`), and B2 then migrates the corlib HOF to call them
> with function-value args. Both must sit on a correct `Func$` convention,
> so GM-B1 waits until fn-values Slice A (FV-T3+T4) is green.

### Sprint H — The marquee payoff + journals + pins
*Goal: the corlib HOF instance-syntax migration, plus every feature's
journal entry, verify-corpus pin, and doc cross-link. Demonstrable:
`xs.Map<R>(f)` on a real `List<T>` returns the expected value; all four
journal sections written.*

| Task | Title | Feature | Deps | Parallel? |
| ---- | ----- | ------- | ---- | --------- |
| **GM-B2** | Corlib `List<T>` HOF migration to `xs.Map<R>(f)` | generic-methods | GM-B1 | SERIAL (critical-path tail) |
| **GM-A5a** | Corlib comment update (no behavior change) | generic-methods | GM-A3b | PARALLEL (do any time after Sprint D) |
| **FV-T8** | Delegate/Event bridge groundwork (optional) | fn-values | FV-T5 + Delegate stdlib | PARALLEL / optional |
| **IT-T8** | Journal + verify-corpus pin + doc cross-link | itables | IT-T7 | PARALLEL |
| **TA-9** | Journal + docs | targeted-args | TA-3..TA-8 | PARALLEL |
| **GM-journal** | generic-methods journal + verify-corpus pin + cross-link | generic-methods | GM-B2 | SERIAL after GM-B2 |

> FV-T8 is optional (design-only until `System.Delegate` exists) and
> GM-A5a is comment-only — both can be slotted into any earlier review gap.

---

## Per-task reference (id · title · feature · deps · agent-prompt seed · acceptance gate)

> Gate shorthand: **3 gates** = verify 152/152 + parser 152/152 +
> run-corpus all-pass. Each task additionally lists its *new* pinning
> test(s). A task lands only when 3 gates **and** its new gate are green.

### generic-methods (home doc §9)

- **GM-A1** · *Mechanical re-key + `cur_type` plumbing* · generic-methods · deps: — ·
  *seed:* Add `mangle_generic_method`, `GenMKey`=`(Option<StructId>,String,String)`, `GenMethodMono` struct; swap `gen_method_sigs`/`gen_method_monos`/`GenMethodDecls` to keyed forms with `owner=None` hardcoded; add `Lowerer.cur_type`, populated (threaded from `lower_type_at` owner_id) but not yet read. ·
  *gate:* 3 gates unchanged; `generic_method.bf→12`, `generic_method_qualified.bf→49`, `list_hof.bf→18` unchanged; symbol grep finds only `lower.rs` (symbols byte-identical to today).

- **GM-A2** · *Owner determination: static/bare/qualified + collision fix* · generic-methods · deps: GM-A1 ·
  *seed:* `index_generic_methods` resolves enclosing `TypeDecl`→`StructId`, inserts both `(Some(owner),name)` and `(None,name)` Vec entries; `record_method_inst` gains `owner`; thread `cur_owner` through `collect_insts_*`; call site reads `cur_type` with `None` fallback. ·
  *gate:* 3 gates; **new `generic_method_collision.bf→42`**; `list_hof.bf→18` (retained `None` bucket); `generic_method.bf→12`, `generic_method_qualified.bf→49`.

- **GM-A3a** · *Collector local/field type scope + instance-receiver resolution* · generic-methods · deps: GM-A2 ·
  *seed:* Add `locals: Vec<(String,IrType)>` to `collect_insts_stmt/_expr` (from params, typed `Stmt::Local`s, `this`, `this`-fields); resolve value-receiver owners for declared-typed local/param/`this`/`this`-field/`new T()`; diagnose any other value receiver. ·
  *gate:* 3 gates; collection unit/snapshot test: supported shapes record `Some(owner)`, unsupported receiver emits the diagnostic (no silent skip).

- **GM-A3b** · *Instance generic-method dispatch (call + emission)* · generic-methods · deps: GM-A3a ·
  *seed:* Rework `Expr::Generic`-callee branch to match base shapes inline, `struct_base` for value receivers, mangle `Some(owner_id)`, prepend `body_ptr`, is_instance-aware arity guard, hard-assert `call_args.len()==sig.params.len()`, diagnose absent keys; emission loop sets `this_ty=Some(Ref(owner))`, passes `structs.methods[owner]` as `sigs`. Add the feature-suite `.bf`. ·
  *gate:* 3 gates (incl. new feature-suite file verifying clean, count→153); **`generic_method_instance.bf→42`, `generic_method_instance_this.bf→7`, `generic_method_two_owners_instance.bf→6`, `generic_method_field_receiver.bf→N`**; inherited/virtual/comptime/unresolvable-receiver negatives are clean diagnostics.

- **GM-A4** · *Guards & negatives hardening* · generic-methods · deps: GM-A3b ·
  *seed:* Implement real sema diagnostics (not debug asserts) for virtual/override+generic, `[Comptime]`+generic, inherited generic instance method, abstract-type-arg inner generic call. ·
  *gate:* 3 gates; each negative case is a clean diagnostic in the corpus harness; no dangling symbols/garbage values.

- **GM-A5a** · *Corlib comment update (no behavior change)* · generic-methods · deps: GM-A3b ·
  *seed:* Update `newbf-corlib/bf/List.bf` `Functional.Map/Filter/Fold` comment noting concrete-owner instance generics work; generic-owner await B1. ·
  *gate:* corlib slice verifies clean; no run-corpus value change.

- **GM-B1** · *Generic methods on generic owners* · generic-methods · deps: GM-A3b, GM-A4, **FV Slice A (FV-T3+T4)** ·
  *seed:* Emit instance generic-method monomorphs whose owner is a *type* monomorph (`List<int64>.Map<R>`) at the mono's id/prefix with combined env (owner T ++ method R); resolve owner-mono prefixes only after the full type-mono table exists. ·
  *gate:* 3 gates; **`generic_method_on_generic.bf` (concrete expect value) passes**.

- **GM-B2** · *Corlib `List<T>` HOF migration* · generic-methods · deps: GM-B1 ·
  *seed:* Move `Map`/`Filter`/`Fold` onto `List<T>` as instance generic methods; update/add `list_hof_instance.bf` to call `xs.Map<R>(f)`. ·
  *gate:* 3 gates; **instance-syntax HOF program returns expected value**; corlib slice verifies clean.

- **GM-journal** · *Journal + verify pin + cross-link* · generic-methods · deps: GM-B2 ·
  *seed:* New numbered journal § (design + outcome); cross-link this doc; ensure verify-corpus count reflects the added feature-suite file. ·
  *gate:* journal entry present; 3 gates; commit pairs with entry (conventional + Co-Authored-By).

### targeted-args (home doc §9)

- **TA-1** · *Classification + `lower_arg_targeted` + `Ref`-initializer-undef fix + try-order unification* · targeted-args · deps: — ·
  *seed:* Add `arg_is_pending`, `pending_kind`, `ArgShape`/`PendingKind`, `Lowerer::lower_arg_targeted`; fix `try_target_typed_initializer`/`lower_initializer` so a `Ref(id)` target no longer yields silent undef (gate to `Struct(id)`); unify canonical try-order enum→tuple→ctor→initializer; refactor local-init/assign-RHS/return onto `lower_arg_targeted`. ·
  *gate:* 3 gates unchanged (proves refactor preserved behavior); unit test on `arg_is_pending` (`.X`/`.{…}`/`.(…)`/`.Some(40)` true; ordinary/`ref x`/bare-Tuple/qualified-`Enum.Case` false).

- **TA-2** · *Shape-gated `pick_overload_partial`/`ctor_for_partial` + phase-1/finish helpers* · targeted-args · deps: TA-1 ·
  *seed:* `pick_overload_partial(cands,&[ArgShape],members)` with shape gate; `pick_overload` becomes thin wrapper; `StructTable::ctor_for_partial` (used only under has_pending fork); `lower_args_phase1`/`finish_args` (single in-order phase-2 emission, pre-sliced `formal`, arity-bounds assert, diagnostic recovery). ·
  *gate:* 3 gates unchanged; unit tests (a)–(d) incl. `M(Vec2,int)` vs `M(Vec3,int)` wrong-pick and primitive-disqualify.

- **TA-3** · *Wire instance/static/base `lower_method_call`; decide Tuple* · targeted-args · deps: TA-1, TA-2 ·
  *seed:* Add `has_pending` fork (concrete branch verbatim); `lower_args_phase1` once; `pick_overload_partial` across three sub-paths sharing one sparse vector; `finish_args` once; resolve `Expr::Tuple` classification (prove concrete or promote to pending here). ·
  *gate:* 3 gates; **`targ_ctor_arg.bf→25`, `targ_enum_arg.bf`, `targ_initializer_arg.bf` (value+class), `targ_overload_pick.bf`, `targ_block_boundary.bf→1` (incl. null-conditional), `targ_tuple_arg.bf`, `targ_eval_order.bf`** + "exactly one construction emitted" check.

- **TA-4** · *Wire bare-name / free-fn / fn-ptr calls; diagnose pending→external* · targeted-args · deps: TA-1, TA-2 ·
  *seed:* Add fork to the `Expr::Ident`-callee path (overload branch; local-fn and fn-ptr/closure sub-paths back-fill against `ptys[i]`); diagnose a pending arg to an unresolved external call. ·
  *gate:* 3 gates; run-corpus program calling a bare free fn with a `.(…)` arg passes; assert pending→unresolved-external is diagnosed (not silent undef).

- **TA-5** · *Wire `new T(args)`* · targeted-args · deps: TA-1, TA-2 ·
  *seed:* `lower_new` uses `ctor_for_partial`+`finish_args(&ctor.params[1..],…)` only under has_pending; non-pending keeps arity-only `ctor_for`. ·
  *gate:* 3 gates; **`targ_ctor_new_arg.bf` passes**.

- **TA-6** · *Wire value-struct `.(args)` incl. nesting* · targeted-args · deps: TA-1, TA-2, TA-5 ·
  *seed:* Same fork on `construct_value_struct`'s stack-slot ctor path; inner args run their own two-phase pass (reentrant). ·
  *gate:* 3 gates; **`targ_nested_ctor_arg.bf` (`.( .(1f,2f), .(3f,4f) )` into struct-of-structs) passes**.

- **TA-7** · *Wire generic method calls + nested-mono collection* · targeted-args · deps: TA-1, TA-2 (needs GM-A3b on branch) ·
  *seed:* Add fork; back-fill pending args against `gen_method_sigs[..].params[i]` (un-offset); extend `collect_insts_expr` to recurse into pending-arg sub-expressions so a mono used only inside a pending body is collected. ·
  *gate:* 3 gates (verify still 152/152 — mono collection intact); **`targ_generic_arg.bf→7`, `targ_generic_nested_arg.bf` pass**.

- **TA-8** · *Wire enum-payload args + variadic + ref/out* · targeted-args · deps: TA-1, TA-2, TA-3 ·
  *seed:* `try_enum_construct`/`_dot` payload loops back-fill against case `ptys`; confirm `finish_args` variadic branch (fixed+tail) and `arg_value` ref/out fallthrough in phase 1. ·
  *gate:* 3 gates; **`targ_enum_payload_pending.bf`, `targ_variadic_arg.bf`, `targ_ref_mixed_arg.bf` pass**.

- **TA-9** · *Journal + docs* · targeted-args · deps: TA-3..TA-8 ·
  *seed:* New journal § (rationale + §57/§101/§102 lineage); cross-link this doc; optional COMPTIME.md note for deferred comptime-arg case. ·
  *gate:* journal entry; 3 gates.

### fn-values (home doc §9)

- **FV-T1** · *Register `$Func` first + `lower_value_ty` + position gating* · fn-values · deps: — ·
  *seed:* Add `StructTable.func_struct`; register `$Func` as `StructId(0)` first in `build()` with default-id assertion; add `lower_value_ty` (Func→`Struct(func_struct)` only at param/local/return); keep `lower_ty_env(Function)=Ptr`. ·
  *gate:* unit test `func_struct==StructId(0)` fields `[code:Ptr,target:Ptr]`; **`BfRtCallbacks` keeps 8-byte-per-field layout**; 3 gates unchanged (no behavior change).

- **FV-T2** · *Single `$self`-leading lambda convention (env layout unchanged)* · fn-values · deps: FV-T1 ·
  *seed:* Route all lambdas (capturing + non-capturing) through `emit_closure` so every `$lambdaN` is `$self`-leading; non-capturing ignores `$self`. Do NOT yet drop slot-0 code pointer or re-index captures. ·
  *gate:* verify 152/152; slot-0 env layout + old call site still agree, so **`closure_basic→57`, `list_hof→18`** still pass.

- **FV-T3+T4** · *`Func$` producers/consumers/uniform call site + env re-layout **and** static method-ref thunks (ONE atomic commit)* · fn-values · deps: FV-T1, FV-T2 ·
  *seed (T3):* `lower_value_ty` at param/local/return (delete per-site overrides + the entire `closures` field/init/detection/branch in one commit); capturing `Expr::Lambda` builds `Func$`, env holds only captures at index `i`; `emit_closure` reads `$self[i]`; uniform call site = code/target load + arity assert; `coerce` gains `Ptr↔Func$` + null. *seed (T4):* `try_method_ref` emits de-duplicated `$mref$<full>($self,P…){return <full>(P…);}`, returns bare `Ptr`. ·
  *gate (run-corpus is authoritative):* **`closure_arg.bf` (the §49 fix), `closure_capture_two.bf`, `fn_null.bf`, `mref_static_arg.bf` pass; `closure_basic`, `list_hof`, `lambda_basic`, `lambda_params`, `function_pointer.bf→12` still pass**; verify 152/152; `cargo build` clean (no dead `closures`); no `use newbf_llvm` in sema.

- **FV-T5** · *Bound instance method-ref thunks* · fn-values · deps: FV-T4 ·
  *seed:* New `Expr::Member` value-position path → `$mrefb$<full>($self,…){ ((T)$self).M(…) }`, `target=receiver` (class receivers only; value-struct/`mut`/`ref` flagged unsupported). ·
  *gate:* 3 gates; **`mref_bound_arg.bf` (non-virtual method) passes**; journal note on virtual/value-receiver deferrals.

- **FV-T6a** · *Collect inline lambdas in call-arg position* · fn-values · deps: FV-T3 (and TA-3 landed) ·
  *seed:* Extend `collect_lambdas_stmt` to walk into `Expr::Call`/`Expr::Generic` args and assign `$lambdaN` symbols to inline lambdas. ·
  *gate:* 3 gates; an inline-lambda program lowers without `undef`.

- **FV-T6b** · *Target-type inline-lambda params from resolved sig* · fn-values · deps: FV-T6a ·
  *seed:* Supply inline lambda's param types from `pick_overload`'s resolved `ptys` (not a declared local type). ·
  *gate:* 3 gates; **`lambda_direct_arg.bf` passes**.

- **FV-T7** · *`closure_returns_fn` + by-value lifetime/semantics docs* · fn-values · deps: FV-T3 ·
  *seed:* Add `closure_returns_fn.bf` (needs `Func$` return type); journal note: by-value capture survives only via env leak; observable by-ref divergence from Beef; ≤8-byte-capture limit. ·
  *gate:* 3 gates; **`closure_returns_fn.bf` passes**; journal §-entry added.

- **FV-T8** · *Delegate/Event bridge groundwork (optional)* · fn-values · deps: FV-T5 + Delegate stdlib ·
  *seed:* When `System.Delegate` is added, make it layout-compatible with `Func$` (two pointer fields) so a function value is assignable to a `Delegate` local; do NOT sweep `delegate`-typed fields/params into `Func$`. ·
  *gate:* 3 gates; a `delegate`-typed local holds a function value and is callable. (Design-only until Delegate lands.)

### itables (home doc §9)

- **IT-T1** · *Register interfaces as `StructKind::Interface` + base-routing guard* · itables · deps: — ·
  *seed:* Add `Interface` to `StructKind`/`struct_kind`/`ty_of`; audit every `match`/`matches!` on `StructKind`; register interfaces in `register_type_struct` (empty `StructDef`); add the five new `StructTable` fields (`#[derive(Default)]`); guard the base-recording loop with `matches!(kinds[bid],Ref)`. ·
  *gate:* **verify 152/152 (Interfaces.bf named regression: interface types lower as `Ref`, receiver still hits undef fallback without ill-typed IR)**, parser 152/152, run-corpus, `interface_constraint.bf→100`. (Type-flip lands here, made safe by the base guard — not "no behavior change.")

- **IT-T2** · *Capture interface bases; populate imethods/idefaults/explicit_impls* · itables · deps: IT-T1 ·
  *seed:* `fill_iface_members` (record instance non-generic methods into `imethods`, filtering static/generic; `idefaults` per slot; abstracts recorded despite body-less skip; defaults NOT in `methods[iface]`); `collect_iface_bases` (route class interface bases into transitively-flattened `iface_bases`; skip value structs/interfaces); read `explicit_iface` into `explicit_impls`. ·
  *gate:* 3 gates; **dump-ir assertions `iface_bases[Square]==[IShape]`, `imethods[IShape]==[("Area",_)]`, empty `iface_bases` for a value struct**.

- **IT-T3** · *Compose itables into class vtables* · itables · deps: IT-T2 ·
  *seed:* `apply_itables` after `apply_vtables`; compose transitive `imethods`; `N=max over ALL ids of vimpls.len()`; assign `iface_slot_base` globally; per-class impl resolution (explicit→`pick_overload` incl. inherited→default→null+diagnostic); ABI param/return assert; grow `vimpls` with empty-string null gaps; `debug_assert` no overlap. No newbf-llvm change. ·
  *gate:* verify 152/152; run-corpus; **dump-ir assertions (impl symbol at `iface_slot_base`; interface-only class gets a vtable global; inherited-impl resolves to base symbol)**. No call-site change yet.

- **IT-T4** · *Interface receivers reach `struct_base`; upcast confirmed free* · itables · deps: IT-T3 ·
  *seed:* Confirm `struct_base`'s `Ref` arm returns `(body,iface_id)` (comment only); confirm `coerce` (6128) makes `Ref(class)→Ref(iface)` a no-op (delete draft's proposed gated arm idea); confirm `(IFaceA)expr` reinterprets unchanged. ·
  *gate:* 3 gates; an interface-typed local/param resolves to `Ref(iface_id)` (verify clean); receiver still returns undef (no new wrong-direct-call possible).

- **IT-T5** · *Itable dispatch at call site + first 8 run-corpus programs* · itables · deps: IT-T4 ·
  *seed:* Add the interface-dispatch SEPARATE branch BEFORE the methods-keyed block (source `sig` from `imethods`, raw `elem_addr(body_ptr,Ptr,0)` header GEP); add programs 1–8. ·
  *gate:* **`iface_dispatch_basic.bf→9`, `_param→42`, `_polymorphic→1`, `iface_multi→7`, `iface_field_return→12`, `iface_vtable_coexist→3`, `iface_virtual_is_impl→4`, `iface_inherited_impl→5` all pass**; verify 152/152; `interface_constraint.bf→100`.

- **IT-T6** · *Default interface methods* · itables · deps: IT-T5 ·
  *seed:* Emit default-bodied interface methods as free fns `{IFace.prefix}{Method}` with `this:Ref(iface_id)`; sibling unqualified call inside a default dispatches through `this`'s interface vtable; wire `idefaults` into `apply_itables`; reconcile emitted symbol with slot symbol (no double-emit). ·
  *gate:* 3 gates; **`iface_default_method.bf→100`, `iface_default_calls_sibling.bf→30`, `iface_default_override.bf→7` pass**.

- **IT-T7** · *is/as/inheritance against interfaces* · itables · deps: IT-T6 ·
  *seed:* `type_test` reads header via raw `elem_addr` (not `field_addr`); interface-`tid` target set = `iface_bases[c].contains(tid) && !vimpls[c].is_empty()`; keep class-`tid` path; confirm transitive flattening. ·
  *gate:* 3 gates; **`iface_is_as.bf→1` (incl. interface-typed source), `iface_inherit.bf→5` pass**.

- **IT-T8** · *Journal + verify-corpus pin + doc cross-link* · itables · deps: IT-T7 ·
  *seed:* New journal § (design + outcome); add the polymorphic fixture to the verify corpus (increment count); cross-link this doc. ·
  *gate:* journal entry; **verify-corpus count incremented and green**; commit pairs with entry.

---

## Recommended execution order (single reviewer, one agent at a time)

A linearization of the DAG that keeps every commit behind green gates and
minimizes context-switching (finish a feature's current logical chunk
before swapping). Critical-path tasks marked ★.

1. **GM-A1** ★ — the no-op re-key. Cheapest, highest fanout, root of the
   longest chain. **Do this first.**
2. **IT-T1** — opens the fully-parallel itables track (a verify regression
   guard, easy to review in isolation).
3. **FV-T1** — registers `$Func`, pure infra, asserts `BfRtCallbacks`
   layout.
4. **TA-1** — arg-classification infra + the `Ref`-initializer-undef fix
   (a latent-bug fix worth landing early).
5. **GM-A2** ★ — owner determination; lands `generic_method_collision→42`
   (first visible win, kills §107).
6. **IT-T2**, then **IT-T3** — itable data + composition (still no dispatch;
   dump-ir gated).
7. **TA-2**, then **TA-3** — partial-resolution helpers, then wire
   `lower_method_call` (lands `targ_ctor_arg→25`). **Do TA-3 before any
   fn-values inline-lambda work** to settle the arg loop.
8. **FV-T2**, then **FV-T3+T4** (atomic) — the §49 segfault fix. Review
   T3+T4 as one commit. This unblocks GM-B1.
9. **GM-A3a**, then **GM-A3b** ★ — collector scope + instance dispatch
   (lands the four `generic_method_instance*` programs).
10. **IT-T4 → IT-T5** — interface receivers + dispatch (lands the 8 itable
    programs). **First big behavioral milestone for itables.**
11. **GM-A4** ★ — negatives hardening (clean diagnostics).
12. **TA-4, TA-5, TA-6, TA-7, TA-8** — the remaining targeted-args wiring
    sites (TA-7 after GM-A3b is on the branch). Batch by call-site family.
13. **FV-T5** — bound method-refs. **FV-T6a → FV-T6b** — inline lambdas
    (after TA-3 settled the arg loop). **FV-T7** — returns + lifetime docs.
14. **IT-T6 → IT-T7** — interface defaults, then is/as.
15. **GM-B1** ★ — generic-on-generic (needs FV Slice A green — true by
    step 8).
16. **GM-B2** ★ — the corlib HOF migration (the marquee payoff).
17. **Journals + pins, in any order:** GM-A5a (anytime ≥ step 9), TA-9,
    IT-T8, FV-T7's note, GM-journal (after GM-B2), FV-T8 (optional).

> If you want each feature to reach a *demoable* state ASAP rather than
> driving the critical path hardest: after step 8 you already have (a) GM
> collisions fixed, (b) the §49 closure crash fixed, (c) targeted-args'
> first dot-form arg working, (d) itables composed. Steps 10/13/14 then
> turn each into a full feature. The marquee `xs.Map<R>(f)` (step 16) is
> intentionally last because it sits on top of all of GM + FV.

---

## Risk register (cross-cutting)

| # | Risk | Affected | Mitigation |
| - | ---- | -------- | ---------- |
| R1 | **Verify-clean miscompiles** — LLVM builds indirect-call types from arg values, so fn-values ABI drift and itable OOB slots pass `corpus.rs` yet crash at runtime. | fn-values, itables | **Run-corpus is the authoritative gate** for FV-T3+T4 and IT-T5; per-call arity asserts (`call_args.len()==ptys.len()+1`; itable `iface_slot_base>=vimpls.len()`); fn-values adds a `Module.funcs` unit test that every `$lambda*`/`$mref*` has `Ptr` param 0. |
| R2 | **Two agents editing the same arg loop** — targeted-args (TA-3) and fn-values inline lambdas (FV-T6) both restructure call-arg lowering. | targeted-args, fn-values | Ordering rule: **TA-3 lands before FV-T6a**; FV Slice A (T1–T4) avoids the arg-loop structure (it changes call-through-a-name + producer/coerce). Sequenced in Sprints C→F. |
| R3 | **Owner-mangling symbol churn** — GM-A1/A2 rename every generic-method symbol; a stale fixture/tool reference would break silently. | generic-methods (all downstream) | GM-A1 is a *verified no-op* (symbols byte-identical); GM-A2's churn verified by a workspace grep finding only `lower.rs`; run-corpus checks return values (invariant). |
| R4 | **Collection/lowering owner skew** — GM removed the lazy net; collector and lowering must resolve the *same* owner or emit a dangling call. | generic-methods | Identical owner rules in both passes (`cur_type`/`by_name`/restricted `struct_base` shapes); every unresolvable receiver **diagnosed at collection**; lowering **hard-asserts** key presence (GM-A3a/A3b/A4). |
| R5 | **C-ABI layout regression** — widening `function` fields to 16-byte `Func$` would break `BfRtCallbacks` and shift the verify corpus. | fn-values | **Position gating**: `lower_ty_env(Function)` stays `Ptr`; only `lower_value_ty` (param/local/return) yields `Func$`; FV-T1 asserts `BfRtCallbacks` 8-byte-per-field layout unchanged. |
| R6 | **Non-exhaustive `match StructKind`** + **base-routing corruption** — IT-T1's new variant breaks `match`es; registering interfaces re-routes a class's interface base into its inheritance base. | itables | IT-T1 adds the `Interface` arm to `ty_of` (compile error otherwise) **and** ships the `matches!(kinds[bid],Ref)` base guard *atomically*; Interfaces.bf named as an explicit verify regression in IT-T1's gate. |
| R7 | **Atomic-landing violations** — FV-T3 without T4 breaks `function_pointer.bf`; the env-layout + call-site rewrite must be simultaneous or `closure_basic` breaks. | fn-values | **FV-T3+T4 reviewed/committed as ONE unit** (Sprint D, explicitly flagged); FV-T2 is the genuinely layout-neutral half landed separately. |
| R8 | **SSA dominance ("instruction does not dominate all uses")** — every feature emits new value sequences; the prior scope-alloc work shows this bites. | all four | All emission stays in the *current* block, `call_args`/`Func$`/itable-dispatch assembled before the single terminal call; no new phi/cross-merge production. Each feature has block-boundary/null-conditional run-corpus coverage. |
| R9 | **Critical-path stall** — GM-B1/B2 (the marquee) depends on both the longest GM chain *and* fn-values Slice A. | generic-methods, fn-values | Front-load GM-A1 (step 1) and FV Slice A (step 8); B1/B2 are intentionally the *tail* so a slip there doesn't block the other three features' demoable milestones (all reached by step 14). |
| R10 | **sema must not depend on newbf-llvm** (hard architecture invariant). | all four | None of the four needs a backend change (fn-values §7.5, itables §5, generic-methods §5.5, targeted-args §5.2 all state "no newbf-llvm change"); each feature's gate includes "no new `use newbf_llvm` in sema." |

---

## Notes on what was *not* sequenced

- **Deferred open-questions** from each doc (boxing value structs to
  interfaces; generic interface methods; by-reference capture;
  comptime call args; full single-pass eval order; per-class itable
  offsets) are out of this wave by design — they appear in each home doc's
  §10 and are not assigned tasks here.
- **GM generic interface methods** (the one place itables and
  generic-methods would truly merge) is explicitly deferred in *both*
  docs; it is a natural next-wave item once GM owner-mangling (this wave)
  and itable layout (this wave) both exist.
