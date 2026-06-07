# Mixins (Hygienic Splices) + Try!/Result Error Handling — Design

## 1. Problem & goal

NewBF parses mixin declarations and `name!(args)` invocations but throws away the
mixin-ness: a `mixin` member becomes `Member::Method { return_ty: Type::Error, … }`,
a local mixin becomes `Stmt::LocalFunction`, and `name!(args)` becomes a plain
`Expr::Call` (the `!` is dropped). Sema then ignores all three — `lower.rs:5358-5360`
literally skips them ("not in the kernel yet … never panicking"). There is no
expansion, no hygiene, no control-flow escape. There is also no `System.Result` in the
corlib prelude (only `beef-tests/corlib-slice/Result.bf` and `run-corpus/result_generic.bf`
exercise a *bare* `enum Result<T,E>` as corpus fixtures), and therefore no `Try!`.

A **mixin** is not a function call: its body is *spliced inline* at the call site.
Identifiers resolve under hygiene rules, and — crucially — `return`/`break`/`continue`
inside the body operate on the **caller's** enclosing method/loop. That last property is
what makes `Try!` possible:

```beef
// after this feature (v1 concrete form — see §3.7 for the var-param caveat):
static Result<int32, bool> Parse(int32 x) {
    if (x < 0) return .Err(false);
    return .Ok(x * 2);
}
static Result<int32, bool> Run() {
    int32 a = Try!(Parse(10));    // a = 20
    int32 b = Try!(Parse(-1));    // expands to: if (res case .Err(let e)) return .Err(e); → Run returns .Err early
    return .Ok(a + b);            // never reached on the -1 path
}
```

`Try!(Parse(10))` is an **expression mixin** yielding `int32`; the `return` inside its
body returns from `Run`, not from any synthetic function.

### 1.1 The two hard external facts this design is pinned to (verified)

Two realities (confirmed against the tree) reshape the whole plan versus a naive
"splice during lowering" sketch. **The first slice must respect both or it cannot be
green:**

- **A `Mixins.bf` already lives in the verify corpus** (`beef-tests/feature-suite/src/Mixins.bf`,
  258 lines). It densely uses constructs v1 does **not** support: generic mixins
  (`CircularMixin<T>`, `Test<T>`, `Pop<TVal>`, `DisposeIt<T>`, `ExtendSpan<T>`),
  `var`/`out` params (`MixA(var addTo)`, `GetVal(var a)`, `GetVal2(out int a)`),
  recursion (`CircularMixin!(k)` inside `CircularMixin`), **lvalue-yielding bodies**
  (`GetRef!(b) += 200` whose body yields `ref a`; `Unwrap!(svRes)..Trim()`),
  **lambdas/local-fns inside mixin bodies** (`MixB` contains a local `void AddIt()`;
  `ToScopeCStr!()` is called inside a lambda), `scope:mixin` allocation, and a
  **const-field initializer** (`const int cVal = MixNums!(3,5)`). The verify gate
  `llvm_lowering_verifies_on_real_beef` (corpus.rs:148-196) is **100% clean-LLVM-verify**,
  not merely no-panic. The moment expansion is enabled it WILL fire on `Mixins.bf`, so
  every shape there must lower to verifiable IR.

- **`Lowerer::stmt`/`expr` take a *single* `src: &str`** and `Span::text(self, src)`
  blindly slices *that* string (`newbf-lexer/src/token.rs:33-35`). The corlib prelude is
  prepended with **distinct `FileId(10_000+i)` and a distinct `src` string per file**
  (`lower.rs:3433-3447`). So splicing a corlib mixin body while lowering a user file
  needs *two* sources at once (caller src for args, mixin src for body) — structurally
  impossible with one `src` param unless we thread per-body sources. Lambdas/local-fns
  already solve this by carrying their own `lsrc` per emitted body (`lower.rs:3502-3518`);
  mixins must do the same.

**Target v1 slice (decided), reconciled with both facts:**
- Statement mixins and expression mixins (value-yielding via `=> expr` body **or** a
  block body's trailing bare expression — both required for the real `Try!`, see §3.5).
- Control-flow escape: `return` from inside a mixin returns from the caller; `break`/
  `continue` target the caller's innermost loop (with an explicit empty-loop guard).
- Hygiene: call-site arguments bind to mixin params (evaluated once, by value or by an
  inferred-place binding for the `var` form, §3.4); free identifiers resolve in the
  **caller's** scope; `this` binds to the caller's `this` (with a static-caller guard).
- A **strict expansion gate**: v1 expands ONLY the supported shapes; every unsupported
  shape (generic, `var`/`out` param it can't bind, lvalue-yield, lambda-in-body,
  `scope:mixin`, const-init, recursion-over-depth, cross-file in v1, comptime) falls back
  to the **existing** `_ => {}` / unresolved-default behavior that already verifies on
  `Mixins.bf` today — never to a novel "graceful default" IR shape. (§3.8)
- `Result<T,E>` and `Result<T>` in corlib with `Unwrap` (**not** calling a non-existent
  `Internal.FatalError`; see §3.7), plus `Try!` proven end to end.

**Deferred (staged later):** generic mixins (`mixin M<U>`), cross-file/corlib mixins
(needs per-body src threading proven for mixins), `var`/`out`-param mixins beyond the one
form `Try!` needs, lvalue-yielding mixins, lambdas/local-fns in mixin bodies,
`scope:mixin`, comptime/const mixins, labeled-loop escape, `(.)err` cross-error casts,
`Result` `IDisposable`/operator extensions.

## 2. Current state (file:line — verified)

**Parser (frozen grammar — only AST enrichment):**
- `parser.rs:3105-3138` — member mixin → `Member::Method { return_ty: Type::Error, … }`.
- `parser.rs:1339-1381` — local mixin → `Stmt::LocalFunction { return_ty: Type::Var(name), … }`.
- `parser.rs:540-556` — `name!(args)` / `name!::(args)` → `Expr::Call` (`!`, `::` dropped).
- `parser.rs:560-568` — `name!<T>(args)` → `Expr::Generic` (the `!` is dropped — the
  **fourth** emit site, see §3.1).
- `ast.rs:263-267` `Expr::Call`; `ast.rs:480-487` `Stmt::LocalFunction`; `ast.rs:830-844`
  `Member::Method`; `ast.rs:726` `Modifier::Mixin` exists but the member-mixin path uses
  the keyword branch, not the modifier.

**Sema / lower:**
- `lower.rs:3429-3499` `lower_program` — prepends the prelude (distinct `FileId`/`src`
  per file), builds `StructTable`, runs `collect_local_fns`/`collect_lambdas` per-`src`,
  then `lower_items` per-`src`. **`StructTable` owns no source strings and has no lifetime.**
- `lower.rs:3093` `collect_lambdas_stmt`, `lower.rs:3387` `collect_local_fns_stmt`,
  `lower.rs:5415` `caps_stmt` — the three statement walkers that must learn the new
  `Stmt::MixinDecl` variant.
- `lower.rs:4498-4513` — method-body lowering: `MethodBody::Block` → `stmt`;
  `MethodBody::Expr` → `expr` then `ret`. **The only value-yielding body form today is
  `MethodBody::Expr`.** Block-trailing-expr yield does not exist yet (§3.5).
- `lower.rs:4775` `fn stmt`, `lower.rs:5548` `fn expr`; the `terminated` flag gates post-terminator emission.
- `lower.rs:4879-4911` — `Stmt::Return`: coerces to `self.ret_ty`, runs ALL defers, frees
  ALL scopes, emits `ret`. It does **not** pop the `scopes`/`defers`/`scope_allocs` Vecs
  (recursion unwinds them) — load-bearing for §3.6.
- `lower.rs:4781-4799` — `Stmt::Block` pushes `scopes`+`defers`+`scope_allocs` in lockstep
  and pops all three; trailing `Stmt::Expr` values are lowered for side-effect and discarded.
- `lower.rs:5678-5837` — `Expr::Call` dispatch; the unresolved-bare-name default
  (5827-5833) emits `call name(args) -> i64`.
- `lower.rs:5341-5360` — `Stmt::LocalFunction` predeclare + the `_ => {}` mixin skip.
- `lower.rs:80-199` `StructTable` (no lifetime; owned only). New mixin registry + a new
  owned `srcs` Vec live here.

**Result/Try!/runtime:**
- `beef-tests/run-corpus/result_generic.bf` — a **bare** top-level `enum Result<T,E>`,
  monomorphized to `Result<int32,bool>`, switch-on-a-value (not `this`), `// expect: 42`.
- `beef-tests/corlib-slice/Result.bf` — another bare `Result` fixture (verify corpus).
- `newbf-corlib/bf/Option.bf` — explicitly defers `Unwrap` "awaiting … enum-method
  lowering". **No proof exists that a generic enum *instance* method that
  `switch (this)` + returns a payload monomorphizes/runs.** This is a real prerequisite.
- `newbf-corlib/bf/Internal.bf` — defines ONLY `Malloc`/`Free`/`MemCpy`. **There is no
  `Internal.FatalError`** anywhere, and no backing runtime symbol. v1 must not depend on it.

## 3. Approach

The feature is a **pure sema (AST→IR) splice**. No new IR, no runtime change for the
mixin path, no backend change, no comptime dependency, no crate-boundary change. The
backend never learns a function was inlined; it sees ordinary IR. This holds because
expansion happens *inside* `Lowerer`, recursively reusing `stmt`/`expr` on the body AST.

### 3.1 Parser: enrich the AST (behavior-preserving)

Add explicit AST variants so sema distinguishes a mixin and can carry the type args of
the `name!<T>(…)` form:
- `Member::Mixin { span, attributes, modifiers, name, generic_params, params, body: MethodBody }`.
- `Stmt::MixinDecl { span, name, generic_params, params, body: Box<Stmt> }`.
- `Expr::MixinCall { span, callee: Box<Expr>, scope_qualifier: bool, type_args: Vec<Type>, args: Vec<Expr> }`
  — `callee` stays an `Expr` so `Outer.Inner!(…)` survives; `scope_qualifier` records `::`;
  `type_args` captures the **fourth** emit site (`name!<T>(…)`, parser.rs:560) so a generic
  mixin call routes to `MixinCall` (with non-empty `type_args`) and the gated
  "generic mixins not supported in v1" diagnostic fires correctly — closing the minor
  about that form silently degrading to the unresolved-default.

Switch all **four** parser emit sites. Behavior-preserving for every existing gate
because sema ignores the new variants until Task 3, and the existing `Mixins.bf`
verify/parser behavior is unchanged (it parsed today; the new variants just need
`span()` arms + `print.rs`).

**Task-1 walker audit (decided — exhaustive-match-enforced):** introducing a new `Stmt`
variant changes every `match` over `Stmt`. Task 1 must update or wildcard-skip, with
intent, every walker in `newbf-sema`: `collect_lambdas_stmt` (3093), `collect_local_fns_stmt`
(3387), `caps_stmt` (5415), the lowering `stmt` (4775), and `print.rs`. Exhaustive matches
fail to compile if an arm is missed; that is the regression net. (Today a local mixin is a
`Stmt::LocalFunction` these walkers already see; after Task 1 it is `Stmt::MixinDecl`, which
they must skip identically until Task 3.)

### 3.2 Sema: mixin definition collection + owned sources (new pre-pass)

Add to `StructTable` (no lifetime — owned only):

```rust
struct MixinDef {
    name: String,
    owner: Option<StructId>,        // None = free/local mixin
    generic_params: Vec<String>,    // v1: collected; expansion gated if non-empty
    params: Vec<MixinParam>,        // name, kind, optional AstType (owned clone)
    body: MethodBody,               // owned clone (Block or Expr form)
    src_file: usize,               // index into StructTable.srcs
    has_lambda_or_localfn: bool,    // set during collection — gates expansion (§3.8)
    yields_place: bool,             // body's trailing form is `ref …`/lvalue — gates (§3.8)
}
enum MixinParamKind { ByValue, ByRef, VarInfer, Out }  // v1 supports ByValue + (limited) VarInfer
struct MixinParam { name: String, kind: MixinParamKind, ty: Option<AstType> }
mixins: HashMap<String, Vec<MixinDef>>,   // name → overloads (by arity in v1)
srcs: Vec<String>,                         // NEW: file index → OWNED source string
```

**`srcs: Vec<String>`** is the resolution to the cross-src blocker. `StructTable::build`
already receives `&[SourceFile]` (which carry `file: FileId` + `src: &str`); it now also
**stores an owned `String` copy of each file's source**, indexed so `src_file` →
`&self.structs.srcs[i]`. This keeps the no-lifetime invariant (owned data) and gives the
expander the body's source at splice time. (Cost: one extra copy of each source string,
bounded and one-time.)

`MixinDef` stores owned clones of the body (`MethodBody`, small) and param types, mirroring
how `monos`/`LambdaEmit` store owned re-find data. Collection runs as a new step in
`StructTable::build` (after struct names, with monomorph collection), walking every type's
members and every method body for local mixins, recording `owner`, `src_file`, and the two
gate flags (`has_lambda_or_localfn`, `yields_place`). Generic mixins are collected (so
collection is generic-aware) but flagged for the expansion gate.

### 3.3 Sema: expansion at the call site (the work)

When `Lowerer::stmt`/`expr` hits `Expr::MixinCall`, expand inline:

1. **Resolve** the `MixinDef` by `(scope, name, arg-arity)`. Mixins are a **separate
   namespace** from methods (decided §10) — resolution checks `self.structs.mixins` only.
2. **Strict-gate check (§3.8).** If the def is generic (`!generic_params.is_empty()` or
   `MixinCall.type_args` non-empty), has a lambda/local-fn (`has_lambda_or_localfn`),
   yields a place (`yields_place`), uses an unsupported param kind, lives in a different
   `src_file` than the call site (v1 = same-file only), is in a const/comptime context, or
   would exceed `MIXIN_MAX_DEPTH` — **do not expand**: fall through to the existing
   `_ => {}` (statement) / unresolved-default (expression) path that already verifies on
   `Mixins.bf`. (No novel IR is emitted for unsupported shapes.)
3. **Push a `MixinFrame`** onto `Lowerer.mixin_stack`: `{ caller_ret_ty, caller_loops_len,
   depth }`. Enforce `depth <= MIXIN_MAX_DEPTH` (64); on overflow, graceful skip (step 2 path).
4. **Snapshot stack depths** — `scopes.len()`, `defers.len()`, `scope_allocs.len()` —
   *before* anything is pushed (§3.6 correctness).
5. **Push one mixin scope frame in lockstep** — `scopes`, `defers`, `scope_allocs` all get
   a fresh frame (mirroring `Stmt::Block` at 4781-4783), so params + body-locals don't leak.
6. **Bind params (evaluate once), in the CALLER's src.** For each arg, lower the arg expr
   with `self`'s current `src` (the caller's) to `(Value, IrType)`:
   - `ByValue`: store into a fresh `alloca`, `bind` the param name as an ordinary local in
     the mixin scope frame. Single-evaluation by construction.
   - `VarInfer` (the `var`-param form `Try!` needs): bind the param to the *inferred*
     `(Value, IrType)` of the arg with no declared-type coercion. v1 supports `var`-params
     restricted to a **simple lvalue or pure-value arg** (load-once into an alloca);
     `out` and `var`-as-out (write-back) are gated to a later task.
7. **Splice the body, in the MIXIN's src.** Set `body_src = &self.structs.srcs[def.src_file]`
   (in v1, equal to the caller src — same-file gate — but threaded correctly so Task 7 can
   relax the gate). For a **statement-context** call, splice via the body form (§3.5). For
   an **expression-context** call, allocate the result slot first (§3.5) and capture the
   yield.
8. **Truncate stacks back to the snapshot** (step 4) **unconditionally** — even when the
   body escaped and `self.terminated` is set (§3.6). This is the critical fix: a body that
   always `return`s never falls back to the expander, so the pop MUST be expressed as
   "truncate `scopes`/`defers`/`scope_allocs` to the snapshot length" rather than a paired
   pop that the escape path skips.
9. **Pop** the `MixinFrame`.

Reusing the live `Lowerer` makes SSA dominance automatic: the splice emits into the
caller's current block in program order; result slots are `alloca`s (always dominate);
terminated-after-escape loads are guarded (§3.6). No new dominance hazard beyond ordinary
statements.

### 3.4 Hygiene model (decided — NewBF v1)

NewBF v1 uses **call-site lexical hygiene**:
- Mixin **params** and **body-locals** live in the fresh lockstep frame (step 5) — they
  don't leak out, but they *can* see the caller's locals.
- **Free identifiers** in the body resolve against the **caller's scope chain** (not a
  captured defining scope) — the deliberate simplification vs. upstream `mCallerScope`.
  For the v1 mixins this is sufficient (their bodies reference only params + `return`).
  An unresolved name is a **diagnostic**, never a silent wrong bind.
- `this`: the body sees the caller's `this_slot` unchanged. **Static-caller guard
  (decided):** if a mixin body references `this` and the caller's `this_slot` is `None`
  (static context — `Expr::This` would otherwise yield `undef(Ptr)` at lower.rs:5638),
  the expander treats the mixin as unsupported for that call → graceful skip + (Task 8)
  diagnostic. No `None`-unwrap panic.
- A leak-isolation test (§8) pins that a body-local does **not** survive into the caller.

### 3.5 Expression vs statement mixins (block-trailing-expr yield is in v1)

The body form drives this:
- **Statement context** (`Stmt::Expr { expr: MixinCall }`): splice the body's statements;
  discard any trailing value; void result.
- **Expression context** (`MixinCall` as a `Local`/`Return`/arg value, or nested): the
  mixin yields a value via one of two body forms — **both required for `Try!`**:
  - `=> expr` body (`MethodBody::Expr`): the operand is the yield.
  - **block body whose final statement is a bare `Stmt::Expr`** (`MethodBody::Block`): the
    real upstream `Try!` and `Mixins.bf`'s `MixNums`/`MixC` are exactly this shape. v1 must
    implement **"splice-block-yielding-last-expr"**: lower all statements *before* the
    trailing one normally; for the trailing `Stmt::Expr`, instead of discarding its value
    (the normal 4801-4803 behavior), lower the expr and **store it into the pre-allocated
    result slot**, guarded by `!self.terminated`. This is the single hardest piece and is
    on the critical path — it is explicit Task-3 scope with its own test (a block mixin
    with leading statements *and* a trailing value), not folded into prose.

**Result slot + the no-target case (decided).** An expression expansion allocates
`alloca result_ty` before splicing; the yield stores into it; the call value is a load
after the splice. `result_ty` must be known before the body is lowered. v1 rule:
- If the call site provides a **target type** (`Local`/`Return`/typed-arg position),
  `result_ty` is that target (the common case — `int32 a = Try!(…)`; `return .Ok(…)`).
- If there is **no target** (an untargeted subexpression like `Double!(x) + 1`), v1
  **does not** speculatively size the slot: it falls back to **two-pass within the
  expander** — lower the trailing yield expr's *type* by a dry classify (the same
  classify the two-phase arg path uses), alloca that type, then re-lower for value. If
  the type cannot be classified without lowering, the call is gated (graceful skip +
  Task 8 diagnostic "untargeted expression-mixin needs a target type in v1"). A test
  covers the untargeted-subexpression position (asserting either correct value or the
  diagnostic — whichever the chosen path yields).

### 3.6 Control-flow escape + stack discipline (the subtle part)

Escape is *free* because expansion reuses the live `Lowerer`:
- `Stmt::Return` in a spliced body hits lower.rs:4879 as if written in the caller —
  coerces to the **caller's** `ret_ty`, runs the caller's defers, frees the caller's
  scopes, emits `ret`. **No special casing.**
- `break`/`continue` target `self.loops.last()` — the caller's innermost loop.
  **Empty-loop guard (decided):** if a mixin containing `break`/`continue` is spliced
  where `self.loops` is empty, the existing loop-arm code must not panic; v1 treats this
  as unsupported → graceful skip + (Task 8) diagnostic. A verify-corpus test pins
  no-panic for break-outside-loop-in-a-mixin. (Verify the existing arm's empty-`loops`
  behavior in Task 4; if it unwraps, add the guard at the splice boundary.)

**Stack discipline on escape (the load-bearing fix).** `Stmt::Return` runs all
defers/frees all scopes but does **not** pop the `scopes`/`defers`/`scope_allocs` Vecs
(4908-4909) — recursion unwinds them. The expander pushes a lockstep frame (step 5) and
regains control after the splice **only when `!self.terminated`**. Therefore the pop is
expressed as **"truncate all three Vecs to the pre-splice snapshot length"** (step 8),
run unconditionally on both the fall-through and escape paths. A body that always
escapes leaves the frame on the Vecs; truncation removes it so the caller's subsequent
statements see the correct stack depth. A test runs `Try!`-escape **followed by more
statements** in the caller to pin this.

**Terminated-after-escape result load.** After an always-escaping expression mixin,
`self.terminated` is set; the post-splice slot load is dead code in an unreachable block.
Emit the load only when `!self.terminated`; otherwise yield a default of `result_ty` (dead
code, verifier-accepted) — the same pattern as terminated `if` branches.

### 3.7 Result + Try! in corlib — without FatalError, with a proven Unwrap

**The `Internal.FatalError` dependency is removed.** It does not exist in corlib and has
no runtime symbol; a method referencing it would emit an unresolved `call FatalError`
(verifiable but unlinkable in JIT and AOT). v1 `Unwrap`'s error arm must do something the
kernel already supports. **Decision:** the v1 corlib `Unwrap` error arm returns `default`
(zeroed `T`) — no fatal path. (Upstream's `FatalError` semantics are a later task gated on
real runtime work: add `Internal.FatalError` as an extern bound to an abort symbol
exported from `newbf-runtime` and resolvable in BOTH the OrcJit process-symbol generator
AND the AOT link — scoped explicitly in Task 8, not hand-waved.)

**`Unwrap` is unproven machinery — it gets its own gate BEFORE Result/Try! (decided).**
No corpus test proves a generic enum *instance* method that `switch (this)` and returns a
payload monomorphizes/runs; `Option.bf` explicitly defers exactly this. So a precursor
task (Task 4.5) proves `Result<int32,bool>.Unwrap()` independent of mixins. Two `switch`
binding forms must be confirmed: `case .Ok(let v)` (proven by `enum_pattern`) **and**
`case .Ok(var v)` (untested — confirm `enum_pattern` handles `var`). If the
generic-enum-instance-method-on-`this` path does not lower yet, Task 4.5 fixes it there;
**and** as belt-and-suspenders, the v1 `Try!` is written to **inline the case extraction**
rather than depend on `Unwrap`, so Tasks 5/6 do not silently rest on unproven machinery:

```beef
// v1 Try! (corpus mixin, concrete, same error type both sides — no (.)err cast):
mixin Try(var res) {
    if (res case .Err(let e)) return .Err(e);   // escape: returns from the caller
    res.Value                                    // trailing bare expr → yields the Ok payload
}
```

`res.Value` reads the payload via the existing payload-accessor path the corpus already
exercises (it does not require `Unwrap`). The `var res` param uses the limited `VarInfer`
binding (§3.3 step 6). **v1 Try! requires the SAME error type on both sides** (no
`(.)err` cross-error conversion — that needs cast plumbing gated to Task 7). The
`res case .Err(let e)` / `res case .Ok` extraction uses `if (… case …)` which the corpus
proves (`enum_method.bf`). This is a **deliberate divergence** from the upstream
`mixin Try(var result) { if (result case .Err(var err)) return .Err((.)err); result.Get() }`
and is documented as such: v1 proves the *mechanism* (var-param, block-trailing-yield,
escape, same-error), the canonical upstream signature lands in Task 7.

**`Result.bf` in the corlib prelude — collision-checked first (decided).** Adding
`System.Result` to the prelude prepends it to **every** corpus file, and there are already
**bare** `Result` declarations in `result_generic.bf` (run-corpus), `corlib-slice/Result.bf`
and `corlib-slice/Platform.bf` (verify corpus). Before Task 5 ships the prelude type, an
explicit reconciliation step (in Task 5) must: (a) confirm `by_name`/payload-enum monomorph
keys include the namespace prefix so `System.Result<int32,bool>` ≠ bare `Result<int32,bool>`
(register sites lower.rs:2519/681), and (b) if they don't, namespace-qualify or migrate the
fixtures. Task 5 acceptance is **"full verify + run corpora green WITH `Result.bf` in the
prelude,"** run before Task 6. The minimal prelude:

```beef
namespace System {
    enum Result<T, TErr> {
        case Ok(T val);
        case Err(TErr err);
        public T Unwrap() {            // gated on Task 4.5 proving switch-on-this
            switch (this) {
            case .Ok(var val): return val;
            case .Err: return default;  // v1: no FatalError — zeroed T
            }
        }
        public T Value { get { switch (this) { case .Ok(var v): return v; case .Err: return default; } } }
    }
    enum Result<T> { case Ok(T val); case Err; public T Value { get { … } } }
}
```

v1 omits `operator->`/`operator?`/`IDisposable`/implicit conversions (deferred).

### 3.8 The strict expansion gate (keeps `Mixins.bf` verify-clean)

This is the integration linchpin. Task 3 enables expansion *globally*, so it fires on
`Mixins.bf`. Each construct there maps to a v1 disposition:

| Construct in `Mixins.bf` | v1 disposition |
|---|---|
| `MixNums!(3,5)` (non-generic, block-trailing yield) | **expand** (the model case) |
| `const int cVal = MixNums!(3,5)` (const-init context) | **gate → fall back to existing const-init handling** (non-constant init = "not captured yet", degrades as today) |
| `MixA(var addTo)` / `MixC(var val)` (var-param, instance/static) | `MixA` writes through `mA` (caller field) — **gate** unless the simple var-value form; `MixC` (var-value yield) **expand** if simple |
| `MixB` (local fn inside body) | **gate** (`has_lambda_or_localfn`) → existing skip |
| `GetVal(var a)` / `GetVal2(out int a)` (out/write-back) | **gate** (out write-back deferred) |
| `CircularMixin<T>` / `Test<T>` / `Pop<TVal>` / `DisposeIt<T>` / `ExtendSpan<T>` (generic) | **gate** (generic) → existing skip |
| `CircularMixin!(k)` (recursion) | **gate** via depth + generic |
| `GetRef!(b) += 200`, `Unwrap!(svRes)..Trim()` (lvalue-yield) | **gate** (`yields_place`) → existing skip |
| `scope:mixin T[…]` in body | **gate** (`scope:mixin` unsupported) → existing skip |
| lambda body calling `ToScopeCStr!()` | **gate** (lambda-in/around mixin) → existing skip |
| local mixin `AppendAndNullify!` (var write-back through `str`) | **gate** unless simple → existing skip |

**The gate's fallback is the EXISTING path** (`_ => {}` at 5360 for statement context;
the unresolved-default at 5827-5833 for expression context) which already produces the IR
`Mixins.bf` verifies with **today**. v1 emits **no novel IR** for any gated shape. The
pre-Task-3 acceptance step runs the **full verify corpus with expansion ON** and proves
**0 verify regressions on `Mixins.bf` specifically** before any new run-corpus test is
added (Task 2.5 audits this shape-by-shape).

### Alternatives considered & rejected

- **Lower mixins to real functions + `alwaysinline`.** Rejected: inlining preserves
  callee-return semantics, so it cannot express control-flow escape; would also leak
  mixin knowledge into the backend.
- **AST-level macro pre-pass (rewrite AST, then lower).** Rejected: loses the caller's
  *live* lowering state (`env`, `this_slot`, ref-binding, target-typed args), duplicating
  large parts of `lower.rs`. Splicing during lowering reuses all of it for free.
- **True Beef hygiene + mixin generics from day one.** Rejected for v1: threads a second
  scope chain + `mUseMixinGenerics` through ~50 resolution sites; high blast radius
  against green gates. Staged as Task 7.
- **`Try!` as a language primitive.** Rejected: upstream proves it's a corlib mixin;
  baking it in forecloses user error-propagation mixins (MANIFESTO).
- **`Unwrap` calling `Internal.FatalError` in v1.** Rejected: the symbol doesn't exist
  and resolves nowhere (JIT lookup fails, AOT link fails). v1 returns `default`; real
  fatal wiring is Task 8 runtime work.

## 4. Representation / IR / runtime / ABI changes

**None to IR, runtime, ABI, mangling, or the alloc/object-`$header` path.** Mixins are
zero-runtime-overhead compile-time splices; no symbol is emitted for a mixin (mirrors
upstream `BfIRBuilder.cpp:3596` skipping vtable entries for `BfMethodType_Mixin`).
`IrType: Copy` is untouched. `Result` is an ordinary payload enum reusing the tagged-union
repr (`enum_cases`/`payload_enums` at lower.rs:147-154).

**New owned data in `StructTable` (no lifetime added):**
- `mixins: HashMap<String, Vec<MixinDef>>` + `MixinDef`/`MixinParam`/`MixinParamKind`
  (owned `String`/`AstType`/`MethodBody` clones + `src_file`/gate flags).
- **`srcs: Vec<String>`** — owned per-file source copies, indexed by `src_file`. (This is
  the §1.1 cross-src resolution; added to "new owned data" per the integration review.)

**New transient state in `Lowerer` (per-method, reset in `Lowerer::new`):**
- `mixin_stack: Vec<MixinFrame>` where `MixinFrame { caller_ret_ty: IrType,
  caller_loops_len: usize, depth: usize }`. **Explicitly initialized empty in
  `Lowerer::new`** alongside the other per-method state so no frame leaks across methods.

**New AST variants** (parser crate): `Expr::MixinCall` (now with `type_args`),
`Stmt::MixinDecl`, `Member::Mixin` — owned, `Clone + PartialEq + Eq + Debug`.

## 5. Sema / parser / comptime / runtime / codegen changes

**Parser** (`ast.rs`/`parser.rs`/`print.rs`): add the three variants; switch all **four**
emit sites (incl. `name!<T>(…)` → `MixinCall` with `type_args`); add `span()` arms,
`print.rs` rendering, and the Task-1 walker audit (§3.1). No grammar change.

**Sema collection** (`StructTable::build` + a `collect_mixins` walker + `srcs` population):
index every `Member::Mixin`/`Stmt::MixinDecl` with `owner`, `src_file`, and the gate
flags; store an owned `srcs[i]` per file. Generic mixins collected, expansion gated.

**Sema expansion** (`lower.rs`): `Lowerer::expand_mixin(def, call, ctx) -> Option<(Value,
IrType)>` invoked from `stmt` (statement ctx, before the `_ => {}` skip) and from the
`Expr::MixinCall` arm in `expr` (after method/local-fn/fn-value resolution, **before** the
unresolved-default). It applies the strict gate (§3.8 — unsupported → return `None` so the
caller falls through to the existing path), pushes the `MixinFrame` + lockstep frame, binds
params once (caller src), splices the body (mixin src via `srcs[def.src_file]`), manages the
expression result slot (§3.5) and the terminated/escape stack truncation (§3.6).

**SSA-dominance correctness:** by construction — splice emits into the caller's current
block via `stmt`/`expr`; result slot is an `alloca`; terminated loads guarded. The
run-corpus JIT emit runs the verifier on every program.

**Crate boundaries:** `newbf-sema` gains **no** dependency on `newbf-llvm` or
`newbf-comptime`. Expansion is pure AST→IR. The comptime callback seam is untouched.

**Comptime / const:** v1 does **not** expand mixins inside `[Comptime]` functions or
const initializers — both are **gated** (§3.8), falling back to the existing const-init /
skip behavior (so `const int cVal = MixNums!(3,5)` degrades as a non-constant init does
today, no novel IR). When ungated later, expansion (being sema-only, pre-`fold_comptime`)
means `fold_comptime` only ever sees post-splice ordinary IR — no comptime-side change and
no re-entrancy/circular-dep risk.

**Runtime / JIT-vs-AOT:** nothing for the mixin path. The expanded IR is ordinary code.
v1 `Result.Unwrap` calls no runtime symbol (returns `default`), so there is **no** new
symbol to resolve in either path — closing the FatalError blocker. (Task 8's real
`Internal.FatalError` wiring is the only place runtime work is required, scoped there.)

**Codegen:** unchanged. The backend never sees a mixin.

## 6. Interactions

- **$Func / fn-values:** mixin detection sits **after** the fn-value-call check
  (lower.rs:5751) — `function R(P) f; f(args)` still calls through `$Func`; a same-named
  mixin is a separate namespace. **Lambdas/local-fns inside a mixin body are GATED in v1**
  (§3.8): `collect_lambdas`/`collect_local_fns` descend only into `Member::Method`/
  `Constructor`/`Destructor` block bodies, so a `Member::Mixin`/`Stmt::MixinDecl` body's
  lambda is collected **zero** times today → `lambda_names.get(span)` returns `None` →
  `undef(I64)` (a silent miscompile). Rather than rely on the false "collected once"
  claim, v1 **detects a lambda/local-fn in the body during collection
  (`has_lambda_or_localfn`) and refuses to expand** (graceful skip). The per-splice
  capture-overwrite hazard (interior-mutable `lambda_captures.borrow_mut().insert` collapsing
  distinct call-site capture sets to last-write-wins) is thereby avoided entirely in v1, not
  merely "flagged for later." Re-enabling needs Task 2's collection to walk mixin bodies
  per-splice (Task 7).
- **itables / interface dispatch:** orthogonal. A mixin in a default interface-method body
  splices into it; `this` is the `Ref(iface_id)` value; dispatch unchanged. No vtable slot.
- **Owner-mangling / generic methods:** v1 non-generic mixins need no mangling (no symbol).
  The splice lowers under the caller's `env`, so a body referencing the caller's type
  params resolves today — which is why the concrete `Try!` monomorphizes per call site.
- **Two-phase / target-typed args:** mixin args lower through the same path; a target-typed
  dot-form arg flows against the param type **when the param has one**. For a `var` param
  there is no param type, so a pending dot-form arg (`Try!(.Ok(x))`) cannot be classified
  against a param type — v1 **requires `var`-param args to be self-typed** (already-typed
  expressions, not bare dot-forms); a bare dot-form to a `var` param is gated. The
  expression-mixin **result** slot is target-typed from the call context (§3.5).
- **Other 3 wave features (safety guard, comptime-breadth, reflection):** orthogonal —
  mixins touch neither alloc/delete/scope nor reflection metadata; `Result`/`Try!` are
  pure data + control flow.
- **Diagnostics model:** `Program.diagnostics` is produced by `analyze` (resolve.rs/
  build.rs, lib.rs:58-76); **mixin collection runs in `StructTable::build`, inside
  `lower_program`, which has NO diagnostic sink today.** So depth-overflow,
  generic-gated, unresolved-name-in-body, static-`this`, break-outside-loop are detected
  in `lower_program` where no sink exists. **Decision (avoids a v1 signature change):** v1
  emits **no** mixin diagnostics from lowering — every unsupported shape is a **silent
  graceful skip** to the existing verifiable path (gates stay green: the recursion-depth
  verify test observes "build completes," not a diagnostic). The real
  `lower_program -> (Module, Vec<Diagnostic>)` plumbing (a signature change touching
  run_corpus.rs:133, the verify corpus at corpus.rs:167, and the AOT driver) is **deferred
  to Task 8** with its own behavior-preserving gate (empty vec for all existing programs).
  This is the actionable resolution of the "where a sink exists" inconsistency.

## 7. Risks & mitigations

- **`Mixins.bf` breaks the 100%-clean-verify ratchet when expansion lands.** *Mitigated by
  the strict gate (§3.8):* v1 expands only supported shapes; every unsupported shape falls
  back to the EXISTING verifiable path. Pre-Task-3 acceptance runs the full verify corpus
  with expansion ON and proves 0 regressions on `Mixins.bf`, shape-by-shape (Task 2.5).
- **Cross-src splice (corlib mixin body vs caller args).** *Mitigated by `srcs: Vec<String>`*
  (§3.2): args lower with caller src, body with `srcs[def.src_file]`. v1 gates cross-file
  mixins (same-file only) so the dormant hazard cannot fire; Task 7 relaxes the gate behind
  a corlib-mixin run-corpus test (Try! in a prelude file, called from FileId(0)) that the
  single-file v1 tests cannot catch.
- **LLVM "instruction does not dominate all uses".** *Mitigated by construction* — splice
  reuses `stmt`/`expr` (emission order = program order); result slots are `alloca`s;
  terminated loads guarded. The run-corpus JIT verifier is the immediate net.
- **Always-escaping expression mixin (`Try!` on a known-`.Err`).** *Mitigated:* emit the
  slot load only when `!self.terminated`; default in dead code otherwise; **stacks
  truncated unconditionally to the pre-splice snapshot** (§3.6) so the param frame can't
  dangle.
- **`Unwrap`/switch-on-`this` unproven.** *Mitigated:* Task 4.5 proves it independently
  with its own gate; v1 `Try!` inlines case-extraction (`res.Value`) so it does not depend
  on `Unwrap`. The `var`-binding form in `switch` is explicitly confirmed in Task 4.5.
- **`Result.bf` in the shared prelude — name collision / compile cost.** *Mitigated:* Task
  5 grep-reconciles existing bare `Result`/`Option` fixtures and gates on **full corpora
  green with the prelude type present** before Task 6.
- **Lambdas-in-mixin silent miscompile (uncollected span → `undef`).** *Mitigated:* gated
  in v1 (`has_lambda_or_localfn` → refuse to expand), with a pinning test.
- **Run-corpus-as-authoritative-gate vs verify-clean miscompiles.** *Mitigated:* the test
  plan (§8) checks observable values for escape AND happy paths; `mixin_arg_once.bf` pins
  single-evaluation; a "statements after a Try! escape" test pins stack discipline.
- **Recursion / mutual recursion.** Direct mixin-in-mixin bounded by `MIXIN_MAX_DEPTH=64`
  + graceful skip. *Stated limitation:* mutual recursion routed through a **real method**
  that re-invokes the mixin does not increment mixin depth — acceptable for v1, noted.
- **Comptime re-entrancy / circular dep.** *Mitigated by exclusion* — comptime/const
  mixins gated off; expansion never calls the backend; the sema→llvm seal is untouched.
- **`ref`/`var` param stable address.** v1 `var`/by-value params load the arg **once into
  a fresh `alloca`**, giving a stable address and single-evaluation; arbitrary-lvalue
  `ref` write-back (a spilled-temporary hazard) is gated to a later task.

## 8. Testing strategy

Gates green at every task boundary: **parser-corpus**, **sema no-panic corpus**,
**llvm_lowering_verifies_on_real_beef (100% clean-verify, the gate Task 3 most endangers)**,
**run-corpus (authoritative value checks)**, ratchet.

**New run-corpus programs** (self-contained `Program.Main`, `// expect: N`, JIT-run,
full-i32 value check; `.Err` tests avoid any fatal path — none exists now anyway):
1. `mixin_stmt_basic.bf` — statement mixin mutating a caller local; `// expect: 30`.
2. `mixin_expr_value.bf` — `=> expr` mixin yield (`int32 a = Double!(15);` → 30); `// expect: 30`.
3. `mixin_block_yield.bf` — **block** body with leading statements AND a trailing bare
   expression captured into the result slot (pins §3.5 block-yield, the hard piece);
   `// expect: <computed>`.
4. `mixin_return_escape.bf` — body `return`s from the caller on a condition; Main returns 7
   via escape, 99 otherwise; `// expect: 7`.
5. `mixin_arg_once.bf` — by-value param fed a side-effecting arg; static advances by exactly
   1 → single-evaluation; `// expect: 1`.
6. `mixin_break_loop.bf` — `break` inside a caller loop; loop exits early; `// expect: <count>`.
7. `mixin_this_field.bf` — instance mixin reading/writing a caller field via `this`; `// expect: <v>`.
8. `mixin_nested.bf` — mixin invoking another mixin (depth 2); `// expect: <composed>`.
9. `mixin_local_no_leak.bf` — caller declares a same-named local **after** the mixin call;
   asserts the caller's value is seen (body-local did not leak); pins §3.4 frame isolation.
10. `mixin_stmts_after_escape.bf` — `Try!`-style escape (non-escaping call) **followed by
    more statements** in the caller; asserts the later statements compute correctly →
    pins §3.6 stack truncation.
11. `result_try_ok.bf` — v1 `Try!` happy path (concrete, same-error, `res.Value` yield):
    `Run()` returns the combined value; `// expect: <ok>`.
12. `result_try_err_escape.bf` — `Try!` error path: the early `return .Err(code)` fires;
    `Main` matches the returned `.Err(code)` and returns `code`; `// expect: <code>`.

**Precursor gate (Task 4.5):** `generic_result_unwrap.bf` — `Result<int32,bool>.Unwrap()`
(generic enum instance method, **switch-on-`this`, `var` binding**, `.Err`→`default`)
lowers, monomorphizes, runs; `// expect: <ok-payload>`. Independent of mixins.

**Verify-corpus (no-run) gates:**
- **`Mixins.bf` regression (Task 2.5/3):** full verify corpus with expansion ON stays
  100% clean-verify; `Mixins.bf` specifically has 0 new verify failures (every gated shape
  falls back to the existing verifiable IR).
- **Recursion depth:** a self-recursive mixin file compiles (graceful skip, no panic/hang).
- **break-outside-loop-in-a-mixin:** compiles clean (empty-loop guard, no panic).
- **static-caller `this`-in-mixin:** compiles clean (static-`this` guard, no panic).
- **`.Err`/Unwrap branch lowers (Task 5):** a program with a reachable-in-IR `.Err`→Unwrap
  path lowers verifier-clean (pins that the error arm + any symbol it needs builds; v1's
  arm is `default`, so this also confirms no unresolved symbol).
- **Untargeted expression-mixin (Task 3):** the no-target subexpression position either
  computes correctly or yields the documented diagnostic/skip (no panic) (§3.5).

**No new test harness** — the existing run-corpus JIT harness (parse→analyze→lower→OrcJit
→call Main→value check) covers all behavioral cases.

## 9. Task breakdown (ordered, agent-assignable)

Each task lands behind the green gates listed in its Accept line, **including
`llvm_lowering_verifies_on_real_beef` from Task 2.5 onward** (the gate the design must not
regress).

**Task 1 — AST variants + 4-site parser rewire + walker audit (behavior-preserving).**
Scope: `ast.rs` (`Expr::MixinCall` w/ `type_args`, `Stmt::MixinDecl`, `Member::Mixin` +
`span()`), `parser.rs` (switch all **four** emit sites 3105/1339/540/560), `print.rs`, and
the `newbf-sema` walker audit (`collect_lambdas_stmt`, `collect_local_fns_stmt`,
`caps_stmt`, lowering `stmt`) — wildcard-skip the new `Stmt::MixinDecl` with intent.
Deps: none. Accept: parser-corpus + sema no-panic + verify-corpus all green (sema still
ignores the new variants). Behavior-preserving.

**Task 2 — Mixin collection registry + owned `srcs`.**
Scope: `lower.rs` — `MixinDef`/`MixinParam`/`MixinParamKind` (owned), `StructTable.mixins`,
**`StructTable.srcs: Vec<String>`** populated in `build`, `collect_mixins` walker (members +
local mixins; `owner`, `src_file`, `has_lambda_or_localfn`, `yields_place`); generics
collected + flagged.
Deps: 1. Accept: verify-corpus green (collection only, no expansion); unit assertion that a
known mixin lands in `mixins` with the right `src_file` and gate flags. Behavior-preserving.

**Task 2.5 — `Mixins.bf` shape-by-shape audit + strict-gate spec.**
Scope: enumerate every construct in `feature-suite/src/Mixins.bf` against §3.8; define the
strict gate so each unsupported shape returns `None` from `expand_mixin` and falls back to
the EXISTING verifiable path (`_ => {}` / unresolved-default). No new behavior; produces the
gate predicate + the per-shape disposition table as the contract for Task 3.
Deps: 2. Accept: documented disposition for every `Mixins.bf` construct; the gate predicate
compiles and (with expansion still off) verify-corpus stays green. Behavior-preserving.

**Task 3 — FIRST SLICE: statement + expression (incl. block-trailing-yield) expansion.**
Scope: `lower.rs` — `MixinFrame`/`mixin_stack` (reset in `Lowerer::new`), `expand_mixin`
(strict gate from 2.5; lockstep frame push; param-bind-once in caller src incl. limited
`VarInfer`; splice in `srcs[src_file]`; **block-trailing-expr → result slot**; no-target
two-pass/diagnostic; depth guard; **unconditional stack truncation to snapshot**), wired
into `stmt` (before the skip) and the `expr` `MixinCall` arm (after fn-value, before
unresolved-default).
Deps: 2.5. Accept: **full verify corpus 100% clean-verify with expansion ON, 0 new
failures on `Mixins.bf`**; run-corpus adds `mixin_stmt_basic`, `mixin_expr_value`,
`mixin_block_yield`, `mixin_arg_once`, `mixin_this_field`, `mixin_nested`,
`mixin_local_no_leak` — all pass; static-`this` + untargeted-subexpr verify files clean.
Behavior-changing.

**Task 4 — Control-flow escape + stack discipline + guards.**
Scope: `lower.rs` — confirm `return`/`break`/`continue` target the caller; add the
empty-`loops` guard (verify the existing arm, guard if it unwraps); the terminated-after-escape
result-load guard; the `caller_loops_len`/`caller_ret_ty` snapshots; verify the unconditional
stack truncation across an escaping splice.
Deps: 3. Accept: run-corpus `mixin_return_escape`, `mixin_break_loop`,
`mixin_stmts_after_escape` pass; break-outside-loop verify file clean (no panic). Behavior-changing.

**Task 4.5 — Generic enum instance method (switch-on-`this`) monomorphization.**
Scope: prove/fix `Result<int32,bool>.Unwrap()` (generic enum instance method, `switch
(this)`, **confirm `var` binding** in `enum_pattern`, `.Err`→`default`) lowers,
monomorphizes, runs — independent of mixins. Fix in the enum-method-lowering path if needed.
Deps: none (parallel with 1-4; required before 5/6). Accept: `generic_result_unwrap.bf`
passes; gates green. Behavior-changing (or no-op if already supported).

**Task 5 — `Result.bf` (corlib prelude) + collision reconciliation.**
Scope: `newbf-corlib/bf/Result.bf` (`Result<T,E>`, `Result<T>`, `Value`/`Unwrap` with
`.Err`→`default`, **no FatalError**). Grep + reconcile existing bare `Result`/`Option`
fixtures (`result_generic.bf`, `corlib-slice/Result.bf`, `corlib-slice/Platform.bf`);
confirm/namespace monomorph keys so `System.Result` ≠ bare `Result`.
Deps: 4.5. Accept: **full verify + run corpora green WITH `Result.bf` in the prelude**; a
run-corpus program constructs + `Unwrap`s a happy path; the `.Err`-branch-lowers verify
file is clean. Additive once collisions reconciled.

**Task 6 — `Try!` (corpus mixin, concrete, same-error) end to end.**
Scope: run-corpus files defining the v1 concrete `Try!` (`var res` param, block-trailing
`res.Value` yield, same-error escape — §3.7, deliberate divergence documented) +
`result_try_ok.bf`, `result_try_err_escape.bf`.
Deps: 4, 5. Accept: both run-corpus programs pass value checks; gates green. Behavior-changing
(proves var-param + block-yield + escape + same-error end to end).

**Task 7 — (Staged) Generic mixins + cross-file/corlib mixins + defining-scope hygiene +
lvalue-yield + lambda-in-body + canonical `var`-param Try! + `(.)err` cast.**
Scope: thread `generic_params` env into expansion; relax the cross-file gate behind a
corlib-mixin run-corpus test (Try! in a prelude file, called from FileId(0)); defining-scope
capture for free names; per-splice lambda/local-fn collection + capture; lvalue-yield mode;
ship canonical `mixin Try(var result){ if(result case .Err(var err)) return .Err((.)err);
result.Get() }` in `Result.bf`.
Deps: 6. Accept: generic `Try!` drives multiple monomorphized run-corpus programs; a
cross-file corlib mixin runs; gates green. Behavior-changing; out of the v1 slice.

**Task 8 — (Staged) Diagnostics plumbing + real FatalError + comptime ungating.**
Scope: `lower_program -> (Module, Vec<Diagnostic>)` (update run_corpus.rs, verify corpus,
driver) merged into `Program.diagnostics`; add real `Internal.FatalError` (extern → a
`newbf-runtime` abort symbol resolvable in BOTH OrcJit process-search and AOT link) and
switch `Unwrap`'s `.Err` arm to it; ungate comptime/const mixins once `fold_comptime` is
confirmed to see only post-splice IR.
Deps: 7. Accept: diagnostic snapshot tests; a fatal-path program aborts as expected in both
JIT and AOT; comptime-mixin run-corpus folds correctly. Behavior-changing.

**Minimal-but-correct first deliverable = Tasks 1, 2, 2.5, 3, 4, 4.5, 5, 6:** statement +
expression (incl. block-yield) mixins, full control-flow escape with correct stack
discipline, the proven generic-enum-`Unwrap` prerequisite, `Result` (no FatalError), and a
working concrete `Try!` — every shape gated so `Mixins.bf` stays verify-clean. Each task
lands behind its listed green gates.

## 10. Open questions / decisions deferred

- **Defining-scope vs call-site hygiene** — *decided v1:* call-site lexical hygiene
  (§3.4); true defining-scope capture → Task 7.
- **Generic mixins** — *decided:* collected generic-aware; expansion gated → Task 7.
- **Cross-file/corlib mixins** — *decided:* v1 same-file only (gated); `srcs` threading is
  in place (§3.2) so Task 7 relaxes the gate behind a corlib-mixin run-corpus test.
- **`var`/`out` params** — *decided:* v1 supports the limited `VarInfer` (self-typed value/
  simple-lvalue, single-load) that `Try!` needs; `out`/write-back `var` → later.
- **Lvalue-yielding mixins** (`GetRef!(b) += 200`, `Unwrap!(svRes)..Trim()`) — *decided:*
  gated in v1 (`yields_place` → existing path); a place-yield mode is Task 7.
- **Lambdas/local-fns in mixin bodies** — *decided:* gated in v1 (`has_lambda_or_localfn`),
  avoiding the uncollected-span `undef` miscompile and per-splice capture-overwrite; Task 7.
- **Comptime/const mixins** — *decided:* gated (graceful skip / existing const-init
  degrade); ungate in Task 8.
- **`Internal.FatalError`** — *decided:* does not exist; v1 `Unwrap` `.Err`→`default`; real
  runtime wiring (extern + JIT/AOT-resolvable symbol) is Task 8.
- **`Result.bf` prelude name collisions** — *decided:* Task 5 reconciles before Task 6;
  acceptance is full-corpora-green-with-prelude.
- **`switch (this)` with `var` binding** — *open until Task 4.5:* confirm `enum_pattern`
  binds `var` (not just `let`) payloads.
- **Labeled break/continue escape** — deferred; v1 targets innermost (kernel-consistent).
- **`(.)err` cross-error conversion** — deferred; v1 `Try!` is same-error both sides → Task 7.
- **Diagnostic sink in `lower_program`** — *decided:* v1 emits no lowering diagnostics
  (silent graceful skip); the `lower_program -> (…, Vec<Diagnostic>)` signature change is
  Task 8 (touches run_corpus.rs:133, corpus.rs:167, driver).
- **Method vs mixin name collision** — *decided:* separate namespaces; `name!(…)` resolves
  only against `mixins`. No precedence rule.
- **Recursion depth** — *decided:* `MIXIN_MAX_DEPTH = 64`, graceful skip on overflow;
  mutual recursion through a real method is unbounded (stated v1 limitation).
- **Multi-`return`-branch expression mixins** — handled by the result-slot model (each
  non-escaping branch stores; escaping branches leave via the caller). Covered by
  `mixin_return_escape.bf`.
