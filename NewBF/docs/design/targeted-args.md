# Target-Typed Call Arguments (Two-Phase Overload Resolution)

## 1. Problem & goal

NewBF already supports *target-typed* expression forms — a leading-dot syntax that omits a type name the compiler can infer from context:

- `.(args)` — value-struct constructor shorthand (`try_target_typed_ctor`, lower.rs:5426).
- `.{ field = v, … }` — object/collection initializer (`try_target_typed_initializer`, lower.rs:5496).
- `.Case` / `.Case(payload)` — bare payload-enum case (`try_target_typed_enum`, lower.rs:4512; `try_enum_construct_dot`, lower.rs:4543).
- `(a, b)` — tuple literal against a tuple struct (`try_target_typed_tuple`, lower.rs:5366).

These work when a **target type is already known**: local-init with a declared type (lower.rs:2918–2927, the §57/§101/§102 path), assignment RHS to a resolved place (lower.rs:5907–5915), and return statements. In every one of those sites the target type is in hand *before* the pending expression is lowered, so the `try_target_typed_*` family can be tried in order and the dot-form is constructed against the known type.

They do **not** work as **call arguments**. In `lower_method_call` (lower.rs:5786) and the bare/static/generic call paths in `expr` (lower.rs:3772–3902), arguments are lowered eagerly *before* overload resolution:

```rust
// lower.rs:5797
let arg_vals: Vec<(Value, IrType)> = args.iter().map(|a| self.arg_value(a, src)).collect();
let arg_tys:  Vec<IrType>          = arg_vals.iter().map(|(_, t)| *t).collect();
```

`arg_value` (lower.rs:5595) for an unrecognized dot-form falls through to `self.expr(a, src)`, and `expr` has no target type, so:

- `.(args)` is an `Expr::Call` with a `DotIdent { name: "." }` callee → not caught by `try_enum_construct_dot` (which keys on a *case* name) → falls to the generic `_` arm → `(undef(I64), I64)` (lower.rs:3901).
- `.{ … }` is an `Expr::Initializer { base: DotIdent }` → `lower_initializer(target = None)` → the `_ => return (undef(I64), I64)` arm (lower.rs:5537).
- bare payloadless `.Case`, or a `.Case(payload)` whose owning enum is one of several monomorphs, → `(undef(I64), I64)`.

So the argument lowers to `undef` *and* poisons overload resolution: `arg_tys` contains `I64` for a slot that should have been (say) `Struct(Point)`, and `pick_overload` (lower.rs:2102) ranks against the wrong type.

**Scope clarification (corrected from earlier framing).** Two named-dot enum forms already work as call args today and are explicitly *out of the broken surface*:

- A **qualified** `Enum.Case(payload)` arg routes through `try_enum_construct` (lower.rs:3811) *before* any eager arg loop and constructs concretely.
- An **unambiguous** target-typed `.Case(payload)` arg routes through `try_enum_construct_dot` (lower.rs:3817) *before* the eager `arg_vals` loop and constructs concretely.

Only these dot-forms are actually broken as args: `.(args)`, `.{ … }`, bare payloadless `.Case`, and **ambiguous** `.Case(payload)` (a case owned by multiple monos, where the param type is needed to pick the right one). The design must (a) not regress the two already-working enum paths, and (b) capture the ambiguous `.Case(payload)` case, which today silently fails.

**Concrete failing example** (does not work today):

```beef
struct Vec2 { public float x; public float y;
  public this(float x, float y) { this.x = x; this.y = y; } }

static class Program {
  static float Dot(Vec2 a, Vec2 b) => a.x*b.x + a.y*b.y;
  public static int Main() {
    // `.(...)` as an argument — wants to target-type to Vec2, but lowers to undef.
    float d = Dot(.(3f, 4f), .(3f, 4f));   // expect 25; today: undef args, wrong/garbage result
    return (int)d;
  }
}
```

**Goal.** Make a target-typed-pending argument resolve against its **resolved parameter type**. The path: pick the callee/overload using arity plus the types of the *non*-pending args **and a syntactic shape check on the pending args**, then lower each pending argument against the parameter type the chosen signature assigns it, reusing the existing `try_target_typed_*` machinery. **Left-to-right evaluation order of side effects is preserved**, and a pending arg that fails to target-type against its resolved param is a **diagnosed error**, never a silent `undef`. The common all-non-pending path must stay exactly as cheap as today.

## 2. Current state

Grounded in the code (line numbers verified):

- **Every call site evaluates args once, eagerly, in source order, then resolves.** `lower_method_call` lower.rs:5797–5798; the bare/static/generic-method paths in `expr`'s `Expr::Call` arm at lower.rs:3772 (generic), 3822–3823 (bare-name `arg_vals`), and inside `lower_method_call` for `base.M`, `Type.M`, `obj.M`. Constructors do their own eager arg loops: `new T(args)` (per-arg `self.expr`), value-struct `.(args)` via `construct_value_struct` at lower.rs:5478–5491 (`ctor_for(id, args.len())` then per-arg `self.expr`).
- **The eager loops emit each arg's side effects in strict source order.** This is the observable ordering the two-phase path must preserve.
- **Overload resolution is purely type-driven.** `pick_overload` (lower.rs:2102) needs concrete `arg_tys`; `type_affinity` (lower.rs:2144) scores exact (2) / same-category (1) / unrelated (0). An `undef`-typed pending arg shows up as `I64` and corrupts the score. Arity logic: variadic matches when `arg_tys.len()+1 >= formal.len()` (with a flat penalty 1); non-variadic matches on exact count (lower.rs:2121–2126). Ties keep the first-registered candidate (lower.rs:2133).
- **The target-typed family is already factored** as `try_target_typed_{enum,tuple,ctor,initializer}` (lower.rs:4512, 5366, 5426, 5496) plus `try_enum_construct_dot` (lower.rs:4543), each `(target: IrType, e: &Expr, src) -> Option<(Value, IrType)>`. **Each guards on a distinct `Expr` shape** (see §3.5), so they are mutually exclusive per expression. The canonical try-order appears twice already and the two sites **disagree**: local-init (lower.rs:2922–2925) is enum→tuple→ctor→initializer; assign-RHS (lower.rs:5911–5914) is enum→**ctor→tuple**→initializer. Because the forms are mutually exclusive per-expr the disagreement is currently harmless; §3.5 makes this explicit and §9 Task 1 unifies it.
- **Critical asymmetry between ctor and initializer guards:**
  - `try_target_typed_ctor` (lower.rs:5432) returns `None` unless `target` is `IrType::Struct(id)` *and* `e` is `Call(DotIdent ".")`. For a `Ref(id)` (class) target it correctly declines and the chain falls through.
  - `try_target_typed_initializer` (lower.rs:5502) returns `Some(lower_initializer(..., Some(target), ...))` **unconditionally** for any `Expr::Initializer`. For a `Ref(id)` target, `lower_initializer` hits lower.rs:5532–5535 and returns a **silent `(undef(Ref(id)), Ref(id))`**, which the `.or_else` chain accepts (it is `Some`), never reaching the fallback. This is a real silent-undef hole that the arg feature would expose (a `.{…}` arg to a class-typed param) and must be fixed (§3.4, Task 1).
- **`arg_value`** (lower.rs:5595) handles `ref`/`out` by taking the operand's lvalue and passing `Ptr`; otherwise `self.expr`. A pending dot-form is never an lvalue, so `ref`/`out` never wrap one.
- **Constructors:** `ctor_for(id, arity)` (lower.rs:238) selects purely by arity (`params.len() == arity + 1`), **first match, no type ranking**. `new T(args)` resolves the class id by name, allocates, runs base-ctor chain, then the arity-matched ctor. Value-struct `.(args)` (`construct_value_struct`, lower.rs:5478) is the same, on a stack slot, and **recurses into inner args via `self.expr`** (lower.rs:5485) — so a nested `.(…)` inside an outer `.(…)` is itself a pending site.
- **Generic methods:** call site at lower.rs:3772–3795 mangles `Name<Args>` to a symbol, looks up a pre-built `gen_method_sigs` entry (built with `is_instance: false`, `params` = declared params with **no leading `this`**), checks `sig.params.len() == args.len()`, and coerces each arg to `sig.params[i]` (un-offset). No type ranking.
- **Variadic / `params T[]`:** `pack_variadic_args` (lower.rs:5051) takes a `formal` slice **excluding `this`** plus the already-lowered `Vec<(Value,IrType)>`; it coerces fixed leading args, then packs the rest into a fresh `T[]` (elements coerced to `elem`). Call sites pass `&sig.params` for static/same-type and `&sig.params[1..]` for instance.
- **Sixth construction site:** `try_enum_construct` (lower.rs:3811, qualified `Enum.Case(payload)`) and `try_enum_construct_dot` (lower.rs:3817/4543, `.Case(payload)`) both lower payload args via `build_enum_value` → plain `self.expr` (no target typing). So a *pending payload arg* to an enum-case constructor (`Enum.Case(.(1f,2f))` or `.Case(.(1f,2f))`) is a genuine site the original "five sites" enumeration missed. It is added below.
- **Null-conditional calls** `a?.M(args)` route through `lower_conditional_call` (lower.rs:3806) which calls `lower_method_call` inside a freshly-created `nonnull` block. Wiring the fork into `lower_method_call` covers this for free, but "current block" in the SSA argument (§5.4) means the `nonnull` block, not the caller's.

**Gates today:** verify corpus 152/152 (corpus.rs:148), parser corpus 152/152, run-corpus ~160 (run_corpus.rs:50). All must stay green at every task boundary. (Per MEMORY: an AOT exe's exit code truncates to 8 bits; value checks > 255 must use the JIT run-corpus harness.)

## 3. Approach

### 3.1 The core idea: classify, resolve with shape, then single-pass emit in source order

Introduce a cheap, pure **classification** of each argument expression into two kinds, *without lowering it*:

- **Concrete** — anything that yields a known `IrType` by ordinary evaluation, *including* qualified `Enum.Case(...)` and unambiguous `.Case(...)` (these already resolve concretely before any arg loop). This is everything that works today.
- **Pending** — a target-typed dot-form whose type is not yet known and that is *not* already handled by the pre-resolution fast-paths: `Expr::DotIdent { .. }` (bare `.Case`), `Expr::Call { callee: DotIdent, .. }` (covers both `.(args)` and `.Case(args)` — see classification note below), and `Expr::Initializer { base: DotIdent, .. }` (the `.{ }` form). `Expr::Tuple` is classified **concrete** for the first slice (decision fixed in §3.6).

A free function `arg_is_pending(e: &Expr, src) -> bool` (pure, no `self`, no emission, O(1) per arg) drives this. **Classification note (correctness fix):** the `Expr::Call` arm matches a `DotIdent` callee **regardless of whether its name is `"."` or a case name**, so an *ambiguous* `.Case(payload)` is classified pending and gets the param type it needs. (An unambiguous `.Case(payload)` is *also* classified pending under this rule, which is fine and in fact more correct — see §3.3 routing.)

Two-phase resolution then runs per call site, but with a single in-order emission pass (the ordering fix):

1. **Phase 1 — lower concrete args, in source order, into positional holes.** Iterate args left-to-right; lower each **concrete** arg eagerly via `arg_value` (exactly as today, same side-effect order), caching `Some((Value, IrType))` at its index; leave each **pending** arg as a `None` hole. Build a *sparse* arg-type vector where pending slots are `None`. This emits concrete args' side effects in source order and computes the types resolution needs.

2. **Resolve.** Pick the overload/ctor using **arity + partial-type + shape** (§3.2): arity must fit (or variadic); concrete slots are scored by `type_affinity`; pending slots are scored by a *shape* check against the formal param type (compatible → small bonus; incompatible → disqualify the candidate). For the single-overload-per-arity case (the common one — ctors, generic methods, most user methods) arity alone resolves it, as today.

3. **Phase 2 — emit in source order, interleaved.** Walk arg indices `0..n` once more, left-to-right. For a **concrete** slot, take the cached value from Phase 1 and coerce it to the resolved param type. For a **pending** slot, lower it *now* against its resolved param type via `lower_arg_targeted`, then coerce. Pack variadics via `pack_variadic_args`.

**Why this preserves order.** Concrete args are emitted in Phase 1 in source order; pending args are emitted in Phase 2 in source order. The *only* reordering is that all concrete args are emitted before all pending args. To eliminate even that, Phase 2 is the single place values are *finalized into `call_args`* — but a pending arg whose source position is *after* a concrete arg with side effects would still observe its construction after that concrete arg, which is correct, while a pending arg *before* a concrete arg is the problem case. We resolve this fully below.

**Ordering — the real fix (resolves correctness blocker #1).** The naive "all concrete in Phase 1, all pending in Phase 2" *does* reorder `M(.(f()), g())` (g before f). We avoid this as follows:

- Phase 1 lowers concrete args **only for the types/values needed to resolve**, into positional holes, in source order. This is unavoidable: sema is a direct lowerer with no pure type oracle, so concrete arg *types* require lowering.
- However, **a pending arg never depends on a later concrete arg, and we must not let a later concrete arg's effects precede an earlier pending arg's effects.** The guarantee we make is precise and enforced by construction: *concrete args are lowered in Phase 1 in source order; pending args are lowered in Phase 2 in source order; and the feature documents that a pending arg's construction side effects are observed **after** all concrete args of the same call.* For the realistic surface (pending forms are constructions of literals, locals, and small expressions) this is a bounded, documented caveat. **To make the common dangerous case correct rather than caveated, the first slice additionally requires:** when a call mixes pending and concrete args, the implementation lowers concrete args in Phase 1 *and* records, per concrete arg, whether it had observable side effects (it always emits them in order); since pending args are emitted strictly after, the only observable difference from the eager path is pending-after-concrete. We accept this as a documented limitation **and** add an evaluation-order run-corpus test (`targ_eval_order.bf`) that pins the *concrete-args-in-source-order* guarantee and documents the pending-after-concrete rule. See §7 and §10.

> Decision: we explicitly **document** the "pending args observed after concrete args" ordering rule rather than claim full equivalence. This is the honest, minimal-risk position; the earlier draft's "order equivalent" claim is withdrawn. A future refinement (§10) can do a true single left-to-right emission pass by resolving with a type-only probe.

### 3.2 Resolution with a pending-shape check (resolves correctness blocker #2)

A pending slot must not score 0 for all candidates (that silently picks a wrong overload and back-fills against the wrong param type). Instead, `pick_overload_partial` and `ctor_for_partial` apply a **syntactic shape gate** per pending slot:

For a pending arg of shape S against a formal param type `F`:

- `.(args)` (ctor shorthand, `Call(DotIdent ".")`): compatible **only** with `F = Struct(id)` (a value struct). Incompatible with `Ref`, int/float/ptr/void → **disqualify** the candidate.
- `.{ … }` (`Initializer(DotIdent base)`): compatible with `F = Struct(id)` **or** `F = Ref(id)` (after the Task-1 fix that makes `.{}` work for class params; until then, `Struct(id)` only). Incompatible with primitives → disqualify.
- `.Case` / `.Case(payload)` (`DotIdent` / `Call(DotIdent case)`): compatible **only** with `F` being a payload-enum `Struct(id)` whose case set contains the named case. Incompatible otherwise → disqualify.

A compatible pending slot contributes a small fixed bonus (e.g. +1, below an exact concrete match of +2) so it breaks ties toward the candidate it can actually target, but does not outrank a genuinely better concrete match elsewhere. An incompatible pending slot **removes the candidate from consideration entirely**. This makes `M(Vec2,int)` vs `M(Vec3,int)` called as `M(.(1f,2f),5)` resolve to whichever struct has a 2-arg ctor; if both do and both are structs, it remains a documented first-registered tie (§7), but the back-fill then targets a *struct* param either way (no primitive miscompile).

**Defense in depth:** even after resolution, `lower_arg_targeted` (§3.4) returns a sentinel on failure and the call site **diagnoses** a pending arg that did not target-type, so a wrong pick that slips through shape-gating cannot become a silent `undef`.

### 3.3 Routing the already-working enum fast-paths (resolves correctness major #3)

The fork must not regress qualified `Enum.Case(payload)` or unambiguous `.Case(payload)` args. Two safeguards:

- **Order preserved at the `expr` Call arm:** `try_enum_construct` (lower.rs:3811) and `try_enum_construct_dot` (lower.rs:3817) remain *ahead of* everything; they handle the *callee being a named enum case at the top level of the call expression*. They are unrelated to args.
- **For pending *args* that are unambiguous `.Case(payload)`:** routing them through Phase 2 → `lower_arg_targeted` → `try_target_typed_enum` resolves the same case **by the now-known param type**, which is at least as correct (it disambiguates monos). We accept this routing and add a regression test asserting `M(.Some(40))` (unambiguous) yields identical behavior whether reached through the old fast-path-as-callee or the new arg path. The §1 text no longer claims `.Case(payload)` "universally lowers to undef."

### 3.4 The back-fill helper and the silent-undef fix

`lower_arg_targeted(param_ty, e, src)` runs the canonical try-order and **distinguishes "no match" (fall back to `arg_value`) from "matched but produced undef" (a diagnosed error)**:

```rust
fn lower_arg_targeted(&mut self, target: IrType, e: &Expr, src: &str)
    -> Option<(Value, IrType)>
{
    self.try_target_typed_enum(target, e, src)
        .or_else(|| self.try_target_typed_tuple(target, e, src))
        .or_else(|| self.try_target_typed_ctor(target, e, src))
        .or_else(|| self.try_target_typed_initializer(target, e, src))
    // returns None if e is not a recognized dot-form for `target`
}
```

**Task-1 fix to `try_target_typed_initializer`:** change it to *decline* (return `None`) when it would otherwise produce an undef. Concretely, make `lower_initializer`'s `Some(IrType::Ref(id))` arm **either** (a) actually `new`-allocate the class, run field inits, and assign the `.{ }` entries (the natural meaning of `.{ }` against a class) — preferred long-term — **or** (b) for the minimal first slice, gate `try_target_typed_initializer` to fire only for `Struct(id)` targets, mirroring `try_target_typed_ctor`, so `.{ }` against a class falls through to a diagnosed error. The first slice ships (b); (a) is a follow-up. Either way, `.{ }` against a class param is no longer a silent undef.

At the call site, the pending branch does:

```rust
let (v, t) = self.lower_arg_targeted(param_ty, e, src)
    .unwrap_or_else(|| {
        self.diag_pending_arg_untargetable(e, param_ty, src); // emit diagnostic
        (undef(param_ty), param_ty) // recover with the param type, not I64
    });
```

So a recognized-pending arg that cannot target-type to the resolved param produces a **diagnostic** (not a silent `undef(I64)` coerced into a struct slot), and the recovery value carries the *param* type so coercion does not further corrupt downstream IR. This directly satisfies the "no silent miscompile" claim the earlier draft made falsely.

### 3.5 The try-order is immaterial (resolves planning blocker)

`try_target_typed_enum` fires only on `DotIdent` / `Call(DotIdent case)`; `try_target_typed_tuple` only on `Expr::Tuple`; `try_target_typed_ctor` only on `Call(DotIdent ".")`; `try_target_typed_initializer` only on `Expr::Initializer`. These four `Expr` shapes are **pairwise disjoint** for any single expression, so the order among them never changes behavior for one expression. The two existing sites' disagreement (local-init tuple-before-ctor vs assign-RHS ctor-before-tuple) is therefore a no-op. **Decision:** the canonical order is enum→tuple→ctor→initializer (the local-init order), and Task 1 unifies all three sites onto `lower_arg_targeted` with this order, with a comment recording the disjointness rationale. This is moved *out* of Task 9 (cleanup) into Task 1 so no intermediate task ships a third arbitrary order.

### 3.6 `Expr::Tuple` classification — decided, not deferred

A bare tuple `(a,b)` is classified **concrete** in `arg_is_pending` for the first slice (keeps classification purely syntactic and O(1); a tuple already evaluates via `build_tuple`). **Hard acceptance criterion of Task 3:** add `targ_tuple_arg.bf` passing `(1, 2)` to a tuple-struct param and verify it resolves to the correct tuple struct via the existing `build_tuple(None, …)` inference. If that inference picks the wrong/absent tuple struct (it infers element widths, so `(3,4)` infers `(i64,i64)` and may miss a registered `(int32,int32)` tuple), **promote `Expr::Tuple` to pending** with a param-type-aware `try_target_typed_tuple`, in Task 3, behind the same shape gate (`.tuple` compatible only with a tuple-struct `Struct(id)`). This is not left open: Task 3 either proves the concrete path works or implements the promotion.

### 3.7 Keeping the common path cheap

The hot path is "no pending args." We gate the whole two-phase machinery behind a single `args.iter().any(|a| arg_is_pending(a, src))` check. When false, every existing call path runs **unchanged** — same eager `arg_value` loop, same `pick_overload`, same `ctor_for`, same coercion. The new code is a *fork*, not a rewrite: zero added cost (beyond one cheap syntactic scan) for the 99% case and no behavioral risk to the corpus. **Constructors specifically keep the original arity-only `ctor_for` on the non-pending path**; `ctor_for_partial` (which ranks + shape-gates) is used *only* under the `has_pending` fork, so non-pending `new T(args)` resolution is byte-identical to today.

### 3.8 Where it plugs in

Six call sites need the fork (the original five plus the enum-payload site). A small reusable core is shared:

- `arg_is_pending(e, src) -> bool` — free fn (§5.3a).
- `Lowerer::lower_args_phase1(&mut self, args, src) -> (Vec<Option<(Value,IrType)>>, Vec<Option<IrType>>)` — lowers concrete args in source order into holes; returns the cached partial values and the sparse type vector. Runs **exactly once** per call site.
- `Lowerer::lower_arg_targeted(&mut self, param_ty, e, src) -> Option<(Value,IrType)>` — §3.4.
- `Lowerer::finish_args(&mut self, formal: &[IrType], variadic: Option<IrType>, partial, args, src) -> Vec<Value>` — the single Phase-2 in-source-order emission pass. **`formal` is the already-sliced param list excluding `this`** (caller passes `&sig.params[1..]` for instance, `&sig.params[..]` for static/generic/ctor-after-this), matching the `pack_variadic_args` contract. Coerces concrete cached values; lowers pending slots via `lower_arg_targeted` against `formal[i]` (or `elem` for the variadic tail) with the diagnostic recovery from §3.4; packs variadics. **Asserts** `args.len()` is within `[fixed, fixed+variadic]` of `formal` and recovers gracefully (no unguarded `formal[i]` OOB).

The six sites:

1. **Instance/static/base call** — `lower_method_call` (lower.rs:5786). `lower_args_phase1` runs **once** before the base/static/instance dispatch; the sparse type vector is shared by all three `pick_overload_partial` calls; `finish_args` runs **once** inside the single resolving sub-path. Pending args are therefore lowered exactly once (never during a non-taken sub-path's resolution).
2. **Bare-name / free-fn / fn-ptr / same-type-overload call** — `expr`'s `Expr::Call` `Ident`-callee path (lower.rs:3824–3899).
3. **Generic method call** — `expr` lower.rs:3772–3795 (`gen_method_sigs`); `formal = &sig.params[..]` (no `this`).
4. **`new T(args)`** — `lower_new`, using `ctor_for_partial` then `finish_args(&ctor.params[1..], …)`.
5. **Value-struct `.(args)`** — `construct_value_struct` (lower.rs:5478), same ctor selection; **recurse**: inner args run their own two-phase pass so nested `.(…)` works.
6. **Enum-case payload** — `try_enum_construct` / `try_enum_construct_dot` payload args (`build_enum_value`): back-fill each payload arg against the case's declared payload types (`ptys`).

### 3.9 Alternatives considered & rejected

- **Lazy/thunked arguments (defer *all* arg lowering until after resolution).** Rejected: inverts the eager model everywhere, risks reordering side effects across *all* calls, and would re-lower or thread `&Expr` through the resolver — invasive, taxes the hot path.
- **A real bidirectional type-inference pass.** Rejected for now: NewBF's sema is a direct AST→IR lowerer; adding bidirectional inference is a multi-month rearchitecture. Two-phase resolution is the minimal local change for the same observable behavior.
- **A `Pending` `IrType` variant.** Rejected: `IrType` is `Copy` and backend-shared; a `Pending` variant leaks into IR/LLVM and every `match IrType`, violating "every Value has a concrete IrType." Pending-ness is *syntactic*; it belongs in a sparse `Option<IrType>` at the resolver.
- **No shape disambiguation (score pending slots 0).** Rejected (was a deferral in the earlier draft): it silently picks wrong overloads and back-fills against wrong param types. Shape-gating (§3.2) is now in the first slice.
- **Require pending args to be trailing.** Rejected as a user-facing restriction; positional back-fill handles any position. The ordering consequence is handled by §3.1's documented rule plus the eval-order test, not by a restriction.
- **Single left-to-right emission via a type-only probe.** A clean future refinement (it would make pending-before-concrete fully order-correct), but the probe requires a pure type oracle sema doesn't have. Deferred (§10).

## 4. Representation & IR changes

**No IR changes.** `IrType` stays `Copy` and unchanged; no new instructions, no mangling changes, no ABI changes. By the time a `Call`/`CallIndirect` is emitted, every operand is a concrete `Value` of a concrete `IrType`, as today.

**Sema-only transient data, all owned (StructTable has no lifetime).** Function-local only:

- Sparse arg-type vector `Vec<Option<IrType>>` (pending = `None`) — stack-lived during one call's resolution; never stored in `StructTable`.
- Cached partial-value vector `Vec<Option<(Value, IrType)>>` — same lifetime.

These are reentrant: a nested `.(…)` arg builds its own local vectors, so `construct_value_struct` recursion is safe (it only mutates `self.fb`, which is the intended emission target).

**Resolution helper signatures (new/changed):**

```rust
// NEW free fn — pure, syntactic, O(1) per arg.
fn arg_is_pending(e: &Expr, src: &str) -> bool;

// NEW: shape-gated partial resolution. Pending slots: compatible -> +1 bonus,
// incompatible -> candidate disqualified. Concrete slots: type_affinity.
// Arity counts every slot. Backward-compatible: pick_overload delegates.
fn pick_overload_partial<'s>(
    cands: &'s [MethodSig],
    arg_shapes: &[ArgShape],     // per slot: Concrete(IrType) | Pending(PendingKind)
    members: bool,
) -> Option<&'s MethodSig>;

// existing pick_overload becomes a thin wrapper over the all-Concrete case.
fn pick_overload<'s>(cands, arg_tys: &[IrType], members) -> Option<&'s MethodSig>;

// NEW on StructTable: ctor pick that ranks by concrete arg types AND shape-gates
// pending slots; arity-only fallback on a tie. Used ONLY under the has_pending
// fork; non-pending new T(args) keeps the original arity-only ctor_for.
fn ctor_for_partial(&self, id: StructId, arg_shapes: &[ArgShape]) -> Option<MethodSig>;

// NEW on Lowerer: back-fill one pending arg; None = not a dot-form for `target`.
fn lower_arg_targeted(&mut self, param_ty: IrType, e: &Expr, src: &str)
    -> Option<(Value, IrType)>;
```

`ArgShape` and `PendingKind` are sema-local enums (not IR types):

```rust
enum ArgShape { Concrete(IrType), Pending(PendingKind) }
enum PendingKind { Ctor /* .(...) */, Initializer /* .{ } */, EnumCase /* .X / .X(p) */ }
```

## 5. Sema / parser / codegen changes

### 5.1 Parser / AST

**None.** Every pending form already parses and lowers in the working local-init/return/assign sites. No `ast.rs`/`parser.rs`/`print.rs` change. (When `Expr::Named` / default params land, `arg_is_pending` and `finish_args` must look through `Named` and skip omitted trailing params — see §10. `arg_is_pending` is the single place to update for `Named`.)

### 5.2 newbf-llvm / codegen

**None.** Output is ordinary `Call`/`CallIndirect` with concrete operands. `pack_variadic_args`, virtual dispatch, and the ctor protocol are reused verbatim. ABI note: a value-struct `.(args)` produces a `Struct(id)` **by value** (`construct_value_struct` loads the slot, lower.rs:5491); this is the first call site passing a *freshly-constructed value struct by value* as an arg. No IR change is needed (it is an ordinary by-value `Struct(id)` operand), but Task 3's `targ_ctor_arg.bf` is the explicit ABI check that the LLVM struct-passing convention accepts it.

### 5.3 newbf-sema (`lower.rs`) — the whole change

**(a) `arg_is_pending`** (free fn near `arg_value`):

```rust
fn arg_is_pending(e: &Expr, src: &str) -> bool {
    match e {
        Expr::DotIdent { .. } => true,                       // bare .Case
        Expr::Initializer { base, .. } => matches!(&**base, Expr::DotIdent { .. }), // .{ }
        // BOTH .(args) (name == ".") and ambiguous .Case(args) (name == a case):
        Expr::Call { callee, .. } => matches!(&**callee, Expr::DotIdent { .. }),
        // Expr::Tuple is CONCRETE for the first slice (see §3.6).
        // ref/out never wrap a pending form (no lvalue), so Prefix is ignored.
        // Expr::Named is not lowered yet; update HERE when it lands (§10).
        _ => false,
    }
}
```

A small companion `pending_kind(e, src) -> PendingKind` maps the shape for `pick_overload_partial`'s shape gate.

**(b) `lower_arg_targeted`** — §3.4 (returns `Option`; never silently undef).

**(c) `lower_args_phase1` + `finish_args`** — the shared two-phase core. `lower_args_phase1` lowers concrete args in source order into holes and returns `(partial, sparse_shapes)`. `finish_args` consumes the chosen `formal` slice (caller pre-slices off `this`), emits in source order (concrete = coerce cached value; pending = `lower_arg_targeted` against `formal[i]`/`elem` + diagnostic recovery), and packs variadics. **`finish_args` runs exactly once per call.**

**(d) `try_target_typed_initializer` fix** — §3.4 (decline-or-construct for `Ref` target; no silent undef).

**(e) Wire the six sites.** Each becomes:

```rust
let has_pending = args.iter().any(|a| arg_is_pending(a, src));
if !has_pending {
    /* EXISTING eager path, verbatim — incl. arity-only ctor_for */
} else {
    let (partial, shapes) = self.lower_args_phase1(args, src); // Phase 1, once
    // resolve via pick_overload_partial / ctor_for_partial against `shapes`
    // then call_args = self.finish_args(formal, variadic, partial, args, src); // once
}
```

For `lower_method_call`, `lower_args_phase1` runs once at the top of the `has_pending` branch; each sub-path swaps `pick_overload` → `pick_overload_partial(.., &shapes, ..)` and its arg assembly → `finish_args`; `finish_args` runs only in the sub-path that returns. The receiver prepend (`body_ptr`) and vtable dispatch (lower.rs:5884–5895) are unchanged.

### 5.4 SSA-dominance correctness

This is the named trap. Safeguards:

- **All emission stays in the current block** — where "current block" is `self.fb.current_block()` at the moment `lower_method_call`/the site runs (for a null-conditional call that is the `nonnull` block, which dominates the call there). Both Phase 1 (concrete args) and Phase 2 (pending args via `try_target_typed_*`) emit into that block; none create or branch to other blocks (`construct_value_struct` allocas+stores in place; enum/tuple/initializer likewise). Every emitted `Value` is defined before the single terminal `Call` that uses it.
- **The `call_args` vector is fully assembled before the single `Call`/`CallIndirect` is emitted last.** No value is used before its definition.
- **No phi/branch interplay** in the arg machinery, so the scope-alloc class of bug is avoided. The verify-corpus gate catches regressions; targeted run-corpus programs place pending args inside `if`/ternary arms and inside a `a?.M(.(…))` null-conditional to exercise block boundaries.

## 6. Interactions

- **Vtables / virtual dispatch.** Orthogonal. Resolution picks the static signature/slot as today (lower.rs:5884); pending only changes argument production. A virtual call with a `.(args)` arg dispatches identically.
- **Null-conditional `a?.M(.(…))`.** `lower_method_call` is the single choke point for instance/static/base **and** null-conditional calls, so wiring the fork there covers `a?.M(.(…))` for free. Pending args emit in the `nonnull` block and dominate the call there. Covered by `targ_block_boundary.bf`'s null-conditional variant.
- **Monomorphization.** Two touchpoints. (1) **Generic method calls** resolve the mono by `mangle_generic(Name<Args>)` from the *explicit* type args, not value args; a pending value arg is back-filled against the pre-built `gen_method_sigs` param type. (2) **Instantiation collection** (`collect_insts_expr`, lower.rs:845) walks AST arg expressions *independently of lowering*, so the two-phase split does not change which expressions are walked. **But** a pending `.(…)`/`.{ }` body can name *additional* generic uses not in the callee signature (e.g. `M(.( Identity<int>(3) ))` or `.{ items = new List<float>() }`). This requires `collect_insts_expr` to recurse into the pending form's sub-expressions (the `DotIdent`-callee `Call` args and `Initializer` entries). **Acceptance:** add `targ_generic_nested_arg.bf` whose pending arg body instantiates a mono *not referenced anywhere else*, and assert (i) verify corpus stays 152/152 and (ii) the program runs (so the mono was collected). If `collect_insts_expr` lacks the recursion arms, adding them is part of Task 7.
- **Target-typing (the existing feature).** This *is* the same machinery, now invoked from a new context. Task 1 refactors local-init/assign/return to call `lower_arg_targeted`, giving a single source of truth for the try-order (§3.5).
- **Comptime.** `[Comptime]` functions are JIT-evaluated and folded; their args must be constant-foldable. A pending construction reduces to stores of constants into a stack slot — foldable in principle, but **out of scope for the first slice**: comptime call args remain concrete-only (§10).
- **AOT vs JIT.** No difference — output is plain IR. The MEMORY note about JIT FP constant pools is irrelevant (no new float-constant globals).
- **`ref`/`out` params.** A pending form is never an lvalue, so `ref`/`out` args are always concrete (handled by `arg_value`'s existing branch in Phase 1). Mixed calls like `M(ref x, .(1,2))` work: `ref x` is concrete (`Ptr`), the `.(…)` is pending and back-filled. **Ordering note:** a `ref`/`out` arg's address-of side effects are emitted in Phase 1; the existing eager path likewise emits args (even on an ultimately-unresolved call). The fork preserves this — Phase 1 emission is not skipped on resolution failure (stays byte-compatible with lower.rs:5899's "args already evaluated for effects" behavior).
- **`params T[]` / variadics.** `finish_args` back-fills **every** pending slot first (fixed slots against their declared param type, tail slots against `elem`) into a fully-concrete `Vec<(Value,IrType)>`, **then** delegates to the unchanged `pack_variadic_args` with the correct pre-sliced `formal` (with/without leading `this` per site). A pending arg can appear in a fixed leading position *and* the variadic tail of the same call (covered by `targ_variadic_arg.bf`).
- **Qualified `Enum.Case(payload)` / `obj.M(...)` receivers.** Qualified enum/ctor forms *as the call expression itself* are concrete and untouched (the pending surface is only leading-dot *argument* forms). The new enum-payload site (§3.8 #6) covers a pending arg *inside* an enum-case payload.
- **Unresolved external (Win32/CRT) calls.** When a bare-name call has a pending arg but resolves to **no** method (the external fallback, lower.rs:3893–3898), there is no param type to back-fill against. The first slice **diagnoses** this: a pending arg to an unresolved external call is an error (it cannot be target-typed), rather than silently becoming `undef`. Documented in Task 4.

## 7. Risks & mitigations

- **LLVM "instruction does not dominate all uses."** Mitigated by keeping all pending-arg emission in the current block and assembling `call_args` before the single terminal `Call` (§5.4). Gate: verify-corpus 152/152 + block-boundary + null-conditional run-corpus tests.
- **Evaluation-order reordering (pending before concrete).** The documented rule: concrete args emit in source order (Phase 1), pending args emit in source order (Phase 2), so a pending arg's construction is observed *after* all concrete args of the same call. This is a bounded, **documented** caveat, pinned by `targ_eval_order.bf` (asserts concrete-arg source order is preserved). Full single-pass order-correctness is a deferred refinement (§10). The earlier "order equivalent" claim is withdrawn.
- **Wrong-overload pick with a pending slot.** Mitigated by shape-gating (§3.2): an incompatible pending slot disqualifies a candidate, so a `.(…)` cannot route to a primitive param, and a struct-vs-struct genuine tie back-fills against a *struct* either way. A remaining all-struct tie is documented first-registered, and `lower_arg_targeted`'s diagnostic recovery (§3.4) prevents any surviving wrong pick from becoming a silent undef.
- **Silent `undef` from a pending arg.** Eliminated: `try_target_typed_initializer` no longer returns undef for a `Ref` target (Task 1 fix), and a pending arg that fails to target-type is **diagnosed** with recovery carrying the param type (§3.4).
- **Monomorph explosion / missing collection.** No new monos synthesized; collection runs over the AST independently. The nested-generic-in-pending-body case is explicitly tested (`targ_generic_nested_arg.bf`).
- **Double-emission across non-taken sub-paths in `lower_method_call`.** Prevented by structure: `lower_args_phase1` and `finish_args` each run exactly once (§3.8 #1). Acceptance for Task 3 includes an "exactly one alloca for the pending construction" check (behavioral: a counter that would double if constructed twice).
- **ABI mismatch / wrong coercion.** Back-filled values go through the same `coerce` against the same `formal[i]`. A value-struct `.(args)` produces a `Struct(id)` by value, matching a by-value struct param; a `Ref(id)` (class) param is shape-disqualified for `.(…)` (and Task-1-fixed for `.{ }`). Gate: run-corpus value checks via the JIT harness (full i32).
- **sema→llvm dependency rule.** Untouched. All logic in `lower.rs` using existing sema/IR types; no new dependency on `newbf-llvm`.
- **Hot-path regression.** The `args.iter().any(arg_is_pending)` scan is O(args) syntactic; for zero pending args the existing code (incl. arity-only `ctor_for`) runs verbatim. Mitigation: the concrete branch is literally the old code; parser/verify/run corpora (no pending args today) stay green, proving no hot-path change.

## 8. Testing strategy

**Existing gates (must stay green at every task boundary):**
- Verify corpus 152/152 (corpus.rs:148) — catches dominance/type regressions on the hot path.
- Parser corpus 152/152 — unchanged (no parser edits).
- Run corpus ~160 (run_corpus.rs:50) — behavioral; existing programs have no pending args, so they prove the hot path is untouched.

**New unit tests (`lower.rs`):**
- `arg_is_pending`: true for `.X`, `.{…}`, `.(…)`, **and `.Some(40)`** (ambiguous-or-not `.Case(payload)`); false for ordinary exprs, `ref x`, **and a bare `Expr::Tuple`** (pinning the §3.6 decision) and qualified `Enum.Case(40)`.
- `pick_overload_partial`: (a) arity-unique candidate resolves regardless of pending slots; (b) two candidates differing only in a pending slot return the first-registered deterministically (pins the tie rule); (c) **`M(Vec2,int)` vs `M(Vec3,int)` called with `M(.(1f,2f),5)`** — shape gate selects the struct that has a 2-arg ctor (the wrong-pick hazard); (d) a `.(…)` pending slot against a primitive param **disqualifies** that candidate.

**New run-corpus programs** (each `Program.Main()->int32`, `// expect: N`, JIT-run; values >255 via JIT harness):

1. `targ_ctor_arg.bf` — value-struct `.(args)` as args; `Dot(.(3f,4f),.(3f,4f))`. `// expect: 25`. (Also the by-value-struct-arg ABI check.)
2. `targ_enum_arg.bf` — bare `.Case`/`.Case(p)` arg to a payload-enum param; payload sum.
3. `targ_initializer_arg.bf` — `.{ a=1, b=2 }` arg to **both** a value-struct param **and** a class param (the class case verifies the Task-1 `Ref`-target fix: either constructs, or diagnoses — never silent undef). Field sum.
4. `targ_overload_pick.bf` — `M(int, Vec2)` vs `M(int, String)`; `M(7, .(1f,2f))` resolves to the `Vec2` one by concrete arg + shape. Checks the Vec2 branch ran.
5. `targ_ctor_new_arg.bf` — `new C(.(args))` (pending arg into a `new` ctor). Reads a constructed field.
6. `targ_generic_arg.bf` — `Identity<Vec2>(.(3f,4f))`. `// expect: 7` (x+y).
7. `targ_generic_nested_arg.bf` — pending arg whose body instantiates a mono used nowhere else (e.g. `M(.( Identity<int>(3) ))` into an int param, or `.{ items = new List<float>() }`); verify mono collected and program runs.
8. `targ_variadic_arg.bf` — `Sum(.(1f,1f), .(2f,2f))` into `params Vec2[]`; sum components. Exercises fixed+tail pending back-fill.
9. `targ_ref_mixed_arg.bf` — `M(ref total, .(1,2))` — mixed ref + pending; post-mutation total.
10. `targ_block_boundary.bf` — pending arg in both arms of `if`/ternary **and** inside a `a?.M(.(…))` null-conditional; SSA dominance across blocks. `// expect: 1`.
11. `targ_tuple_arg.bf` — `(1,2)` to a tuple-struct param (the §3.6 decision test; promotes Tuple to pending if concrete inference fails).
12. `targ_nested_ctor_arg.bf` — `.( .(1f,2f), .(3f,4f) )` into a struct-of-structs (nested `.(…)` via `construct_value_struct` recursion).
13. `targ_eval_order.bf` — `M(.(g()), h())` where `g`/`h` bump a shared counter; asserts the documented order (concrete args in source order). Catches the reordering hazard at the first wiring site.
14. `targ_enum_payload_pending.bf` — `Enum.Case(.(1f,2f))` / `.Case(.(1f,2f))` (pending payload to an enum-case constructor — the sixth site).
15. `targ_no_pending_regression.bf` — a call with only concrete args (sanity that the fork doesn't perturb the common path); value check.

Each new program goes in `beef-tests/run-corpus/`. A task is "done" only when its programs return the expected values under the JIT harness **and** all three corpora remain green.

## 9. Task breakdown (ordered, agent-assignable)

Each task lands behind green gates (verify 152/152, parser 152/152, run-corpus all-pass) before the next starts. Tasks 1–2 add infrastructure with no behavior change; 3–8 enable one site each; 9 cleans up.

**Task 1 — Classification, `lower_arg_targeted`, the `Ref`-initializer fix, and try-order unification (no new feature behavior).**
Scope: `lower.rs`. Add `arg_is_pending` (free fn), `pending_kind`, `ArgShape`/`PendingKind`, and `Lowerer::lower_arg_targeted` (returns `Option`). **Fix `try_target_typed_initializer`/`lower_initializer` so a `Ref(id)` target no longer yields a silent undef** (first slice: gate to `Struct(id)`; record the `new`-class follow-up). **Unify the canonical try-order** (enum→tuple→ctor→initializer) and refactor local-init (lower.rs:2922), assign-RHS (lower.rs:5911), and return to call `lower_arg_targeted`, with a comment on per-expr disjointness (§3.5).
Deps: none.
Accept: workspace builds; all three corpora unchanged (152/152, all run-corpus pass — this proves the refactor preserved behavior). Unit test on `arg_is_pending` covering `.X`/`.{…}`/`.(…)`/`.Some(40)` true and ordinary/`ref x`/bare-`Tuple`/qualified-`Enum.Case` false.

**Task 2 — Shape-gated partial resolution + helpers (no call site wired).**
Scope: `lower.rs`. Add `pick_overload_partial(cands, &[ArgShape], members)` with the shape gate (§3.2); make `pick_overload` a thin wrapper. Add `StructTable::ctor_for_partial` (ranks concrete + shape-gates pending; **used only under the has_pending fork**, non-pending keeps `ctor_for`). Add `lower_args_phase1`/`finish_args` (single in-order Phase-2 emission; pre-sliced `formal`; arity-bounds assertion + graceful recovery; diagnostic recovery for untargetable pending).
Deps: 1.
Accept: builds; all corpora unchanged. Unit tests (a)–(d) from §8, **including the `M(Vec2,int)` vs `M(Vec3,int)` wrong-pick case** and the primitive-disqualify case — the shape-gate fix is forced into Task 2, not deferred.

**Task 3 — Wire instance/static/base calls (`lower_method_call`); decide Tuple.**
Scope: `lower.rs` (lower.rs:5786). Add the `has_pending` fork; concrete branch is the existing code verbatim. `lower_args_phase1` once; `pick_overload_partial` across all three sub-paths sharing one sparse vector; `finish_args` once. **Resolve the `Expr::Tuple` classification (§3.6)**: prove `targ_tuple_arg.bf` works concretely or promote Tuple to pending here.
Deps: 1, 2.
Accept: `targ_ctor_arg.bf` (25), `targ_enum_arg.bf`, `targ_initializer_arg.bf` (value **and** class param), `targ_overload_pick.bf`, `targ_block_boundary.bf` (1, incl. null-conditional), `targ_tuple_arg.bf`, `targ_eval_order.bf` (documented order), and the "exactly one construction emitted" check all pass; existing gates green.

**Task 4 — Wire bare-name / free-fn / fn-ptr / same-type-overload calls.**
Scope: `lower.rs` (lower.rs:3824–3899). Add the fork to the `Expr::Ident`-callee path (the `self.methods` overload branch; local-fn and fn-ptr/closure sub-paths back-fill against their `ptys[i]`). **A pending arg to an unresolved external call is diagnosed** (no param type to target).
Deps: 1, 2.
Accept: a run-corpus program calling a bare free function with a `.(…)` arg passes; a test/assert that a pending arg to an unresolved external is diagnosed (not silent undef); existing gates green.

**Task 5 — Wire `new T(args)` constructor calls.**
Scope: `lower.rs` `lower_new`. Use `ctor_for_partial` + `finish_args(&ctor.params[1..], …)` **only under the has_pending fork**; the non-pending path keeps arity-only `ctor_for` (zero hot-path change). Base-ctor chain unchanged.
Deps: 1, 2.
Accept: `targ_ctor_new_arg.bf` passes; existing gates green.

**Task 6 — Wire value-struct `.(args)` (`construct_value_struct`), incl. nesting.**
Scope: `lower.rs` (lower.rs:5478). Same fork as Task 5 on the stack-slot ctor path: the per-arg loop (lower.rs:5484–5488) becomes `ctor_for_partial` + `finish_args`. **Nested**: inner args run their own two-phase pass (helpers are reentrant; vectors are stack-local). 
Deps: 1, 2, 5 (shares `ctor_for_partial`).
Accept: `targ_nested_ctor_arg.bf` (`.( .(1f,2f), .(3f,4f) )` into a struct-of-structs) passes; existing gates green.

**Task 7 — Wire generic method calls + nested-mono collection.**
Scope: `lower.rs` (lower.rs:3772–3795). Add the fork; back-fill pending args against `gen_method_sigs[..].params[i]` (un-offset, no `this`). **Confirm/extend `collect_insts_expr`** to recurse into pending-arg sub-expressions so a mono used only inside a pending body is collected.
Deps: 1, 2.
Accept: `targ_generic_arg.bf` (7) and `targ_generic_nested_arg.bf` pass; verify corpus still 152/152 (mono collection intact).

**Task 8 — Wire enum-case payload args; variadic + ref/out coverage.**
Scope: `lower.rs` — the `try_enum_construct`/`try_enum_construct_dot` payload loops (`build_enum_value`) back-fill payload args against the case `ptys`; confirm `finish_args` variadic branch (fixed + tail back-fill, then `pack_variadic_args`); confirm `arg_value` ref/out fallthrough in Phase 1.
Deps: 1, 2, 3.
Accept: `targ_enum_payload_pending.bf`, `targ_variadic_arg.bf` (pending in fixed **and** tail position), and `targ_ref_mixed_arg.bf` pass; existing gates green.

**Task 9 — Journal + docs (try-order already unified in Task 1).**
Scope: `docs/journals/2026-05-31.md` (new §), this design doc cross-link, optional `docs/COMPTIME.md` note for the deferred comptime-arg case.
Deps: 3–8.
Accept: journal entry added with rationale and the §57/§101/§102 lineage; all gates green. (The try-order single-source-of-truth refactor moved into Task 1, so Task 9 is documentation only.)

## 10. Open questions / decisions deferred

- **Full single-pass evaluation order.** The first slice documents "pending args observed after concrete args" (§3.1). A future refinement could resolve via a type-only probe and emit every arg in one left-to-right pass, making pending-before-concrete fully order-correct. Deferred; pinned by `targ_eval_order.bf` so the current behavior is explicit.
- **`.{ }` against a class (`Ref`) param — full semantics.** Task 1 ships the safe slice (decline → diagnose). The natural meaning (`new`-allocate the class, run field inits, assign entries) is a follow-up that would make `.{ }` work for class params too.
- **All-struct overload tie on a pending slot.** Two struct candidates identical except in a pending position tie → first-registered (documented). Shape-gating already prevents the dangerous primitive case; full shape-vs-which-struct disambiguation (e.g. matching the case set / ctor arity per candidate) is a future refinement.
- **Comptime call arguments.** Constant-foldable pending constructions as args to `[Comptime]` functions are out of the first slice; comptime args stay concrete-only until the folder can reduce a stack-slot construction to a constant.
- **Default parameter values.** When defaults land, Phase-2 back-fill must skip omitted trailing params; `pick_overload_partial`'s current exact-count arity already rejects default-arg calls (consistent). `finish_args` asserts `args.len()` within `[fixed, fixed+variadic]`; a defaulted call would need that relaxed.
- **Named arguments** (`M(x: .(1,2))`). `Expr::Named` (ast.rs:396) isn't lowered. When added, `arg_is_pending` (the single classification point) must look through `Named` to its value, and `finish_args`' positional indexing must map named→positional. Deferred.
- **Positional-coupling invariant.** `arg_is_pending` and `finish_args` index args **positionally** against `formal`. This is an invariant `finish_args` asserts; it must be revisited if named args or defaults change the positional mapping.
