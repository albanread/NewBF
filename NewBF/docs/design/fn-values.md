# Uniform Function-Value Representation (closures, method-refs, delegates)

## 1. Problem & goal

NewBF can today create and call three kinds of "function value" — but only along narrow, mutually-incompatible paths:

- **Non-capturing lambda** (`x => x*2`): lowers to a free function `$lambdaN` (emitted via `lower_method` with the lambda's params bound as `extra`, **no `$self`**); its *value* is a bare code pointer (`global_addr`, `IrType::Ptr`). Works as an argument.
- **Bare method reference** (`Type.StaticMethod`): `try_method_ref` (lower.rs:4748) yields a bare code pointer. Works as an argument.
- **Capturing closure** (`a => a + b` where `b` is an outer local): emitted via `emit_closure` (lower.rs:2634) with signature `$lambdaN($self, params…)`; allocates a heap env `[code_ptr | cap0 | cap1 …]` (lower.rs:3692–3711); its *value* is the **env pointer**, and the *calling convention is different* — a call must load slot 0 (the code pointer) and pass the env back as a hidden `$self` (param 0). This special convention is only known when the function value lives in a *local* flagged in `Lowerer.closures` (lower.rs:2982).

The defect: **the function-value's representation does not travel with the value.** A function-typed *parameter* (`function R(P) f`) is registered in `fn_sigs` (lower.rs:2589–2601) with no closure flag — the registration comment even says closures "aren't recorded here." So when a capturing closure is passed to `Map<T,R>(self, function R(T) f)`, the callee's `f(x)` takes the plain non-closure branch (lower.rs:3854): it treats the env pointer as a code pointer and does `call_indirect(env, [x])`. The env's first 8 bytes *are* the code pointer, so on x86-64 this jumps to the right address — but with the wrong arguments: the callee passes `x` as param 0, while `$lambdaN` expects `$self` (the env) as param 0 and `x` as param 1. The capture reads garbage and/or the arg shifts off the end. This is the §49 segfault (journal 2026-05-31.md:1474–1481).

**Concrete failing example** (does not exist in the corpus today because it crashes):

```beef
// captures b; passed to a generic Map; SEGFAULTs today
int32 b = 10;
function int32(int32) addB = a => a + b;   // a CAPTURING closure
List<int32> ys = Map<int32, int32>(xs, addB);
```

> **Why this is verify-clean.** The LLVM backend synthesizes each `CallIndirect`'s function type from the **actual argument values**, not the callee's declared signature (opaque pointers; newbf-llvm `build_indirect_call`, lower.rs:573–591). An arity/type mismatch through a function value is therefore accepted by the LLVM verifier and only manifests as a runtime miscompile. **The verify corpus (`corpus.rs`) cannot catch this bug class — only the run corpus can.** This drives the testing strategy (§8) and the acceptance gates (§9).

**Goal:** one uniform representation so that *any* function value — bare function, non-capturing lambda, capturing closure, unbound static-method ref, bound instance-method ref — is **created, passed, stored, and called through a single indirect-call shape**, with one calling convention regardless of captures. Method refs are thunked into that shape. A true zero-cost bare-pointer path is preserved only for **C-ABI function-pointer positions** (struct fields, casts, extern callback tables) and the deferred provably-direct-call peephole — never for a value that may carry a capture.

## 2. Current state (file:line)

- **Lambda collection (two-pass):** `collect_lambdas_stmt` (lower.rs:1760–1809) records, per name-span, a `$lambdaN` symbol and queues `(name, ret, param_pairs, body, src)`; the emit pass (lower.rs:1959–1990) emits each. This is a **purely syntactic pre-pass**: it matches a lambda only when it is the initializer of a `function`-typed local and reads the lambda's param types from that *declared* type (lower.rs:1762/1779–1788). It never walks call arguments or general expressions.
- **Capture detection:** `detect_captures` / `caps_stmt` / `caps_expr` (lower.rs:3500–3642) walk the body for free identifiers resolving to outer locals via `lookup`. Captures are taken **by value at creation time**.
- **Lambda value creation:** `Expr::Lambda` (lower.rs:3677–3712). Non-capturing ⇒ `global_addr(name)` as `Ptr`. Capturing ⇒ `malloc((1+caps)*8)`, store code at slot 0 and each capture after (index `i+1`), value is the env `Ptr`. Captures recorded by symbol in `structs.lambda_captures` (a `RefCell`, lower.rs:122).
- **Closure body emission:** `emit_closure` (lower.rs:2634–2692). Signature is `$lambdaN($self, params…)`; `$self` is param 0, each capture binds to `$self[i+1]` via `elem_addr` (lower.rs:2660–2669); the lambda's params are `Value::Param(i+1)` (lower.rs:2671–2675).
- **Non-capturing lambda emission:** lower.rs:1972–1990 via `lower_method` with params bound as `extra` (indexed `base + params.len() + j`, lower.rs:2606–2611). **No `$self` is added** — only `emit_closure` adds one.
- **Function-typed param registration:** lower.rs:2589–2601 — populates `fn_sigs` only (no closure tracking). Function-typed *local* registration: lower.rs:2956–2984 — populates `fn_sigs` *and* flags `closures` if the init is a capturing lambda.
- **Call through a function value (by name):** lower.rs:3836–3866. Branches on `self.closures.contains(name)`: closure ⇒ load code from env slot 0, push env as first arg; bare ⇒ call the pointer directly. **This is the in-callee `f(x)` path only.** Passing a function value *into* a generic/overloaded method flows through `Expr::Generic`/the overload path (lower.rs:3772–3891) and `arg_value` (lower.rs:5595), coercing each arg to `sig.params[i]`.
- **Method ref:** `try_method_ref` (lower.rs:4748–4761) — static methods only, bare `Ptr`, no thunk, no bound `this`. Used by `function_pointer.bf` (`function int32(int32) f = Math2.Twice; f(6)`).
- **The single source of truth for type lowering:** `lower_ty_env` (lower.rs:6451). `AstType::Function` has **no arm** and falls to `_ => IrType::Ptr` (lower.rs:6502). It feeds: callee param IR type (`param_ir_ty` → lower.rs:6422/2532), the param spill slot alloca + bound type (2570/2607), `MethodSig.params`/`ret` (used to coerce args at the call site, 1583/3888), function-typed **field** types (`fill_fields_at`, 1400), and function-typed **return** types. The per-name registration sites (2589/2959) only populate `fn_sigs` — they do **not** set any IR slot/param/return/coercion type.
- **`coerce`** (lower.rs:6068): `(ptr, ptr) => v`; `(Struct, Ptr)` and `(Ptr, Struct)` fall to `_ => undef(to)` (6132).
- **IR:** `IrType` (ty.rs:14–36) has no function-value variant. `CallIndirect{callee, args}` (inst.rs:266) is the only indirect shape; LLVM lowering is mechanical. `arg_value` (lower.rs:5595) evaluates a call argument with no function-value awareness.
- **Corlib C-ABI table:** `beef-tests/corlib-slice/Runtime.bf:106–137` declares `struct BfRtCallbacks` with ~28 contiguous `function`-typed **fields** (`mAlloc`, `mFree`, …) — a `BfRtCallbacks`-style C function-pointer table. Each is 8 bytes today. `Internal.bf:402` casts and calls one raw: `((function void(void*))…mMarkFunc)(rawPtr)`. `Delegates.bf:246` has `function int(int,int) func0 = null;` and `:247–248` function-pointer copy/cast.

The fundamental issue is structural: **closure-ness is tracked by *local name* in the `Lowerer`, not carried by the *value*.** Once a value crosses a call boundary into a callee, the name is gone and the convention is lost.

## 3. Approach

**Chosen design: a reified two-word fat-pointer struct `Func$` = `{ code: Ptr, target: Ptr }`, with a single uniform calling convention "code(target, args…)" — applied only in closure-carrying positions (params/locals/returns), while C-ABI function-pointer positions keep the bare `Ptr`.**

A function value in a closure-carrying position is a value-struct (`IrType::Struct`) with exactly two pointer fields:
- `code` — the code pointer to call.
- `target` — the env/receiver passed as the **hidden first argument** to `code`. `null` when there is nothing to pass; the thunk/body simply ignores a null target it never reads.

The calling convention becomes **uniform and unconditional**: to call any function value `fv`, load `fv.code` and `fv.target`, then `call_indirect(code, [target, args…])`. There is no longer a "closure vs bare" branch at the call site — `target` is always passed as param 0.

To make that one convention work for *every* producer, every emitted callable target gets a leading `$self: Ptr` param (which it may ignore):
- A non-capturing lambda becomes `$lambdaN($self /*ignored*/, params…)`, `target = null`.
- A capturing closure stays `$lambdaN($self, params…)` with `$self` = env, `target = env`.
- A static method ref `Type.M` is wrapped by a **thunk** `$mref$Type.M($self /*ignored*/, params…) { return Type.M(params…); }`, `target = null`. (A thunk, not a raw code pointer, because `Type.M`'s real signature has no `$self` param — calling it through the uniform `code(target, …)` convention would shift every argument by one. The thunk absorbs and drops the `$self`.)
- A bound instance method ref `obj.M` is wrapped by a thunk `$mrefb$T.M($self, params…) { return ((T)$self).M(params…); }`, `target = obj`. The thunk forwards `$self` as the receiver.

**Position gating is non-negotiable (Integration blocker).** `Func$` is the lowering of a `function R(P)` type **only** when that type is in a closure-carrying position: a method/lambda **parameter**, a **local**, or a **return type**. A `function`-typed **struct field**, an explicit **function-pointer cast target**, and **extern/callback-table** types stay bare `Ptr`. Reason: `BfRtCallbacks` (`Runtime.bf:106–137`) is a C-ABI table of ~28 function-pointer fields, each 8 bytes; widening every field to a 16-byte `Func$` would double the struct and shift every offset, breaking the C-ABI layout the corlib relies on — and the verify corpus would change layout (a regression we must not introduce). Therefore **`lower_ty_env`'s `AstType::Function` arm stays `Ptr`**, and Func$ is produced by a *position-aware* helper used only at param/local/return sites (§5.2).

**Why fat pointer over the alternatives** (full trade-offs in 3.1): the value self-describes (code + target travel together), so the convention no longer depends on call-site name knowledge — which is exactly the structural bug. It is post-monomorphization-clean: `Func$` has one layout independent of the function signature (both fields are opaque `Ptr`), so there is **no monomorph explosion** of representation types. It maps directly onto Beef's `Delegate { mFuncPtr, mTarget }` (upstream BfExprEvaluator.cpp:15837–15858), giving a clean future bridge to `System.Delegate`/`Event`. And it keeps a true zero-cost path for genuine C function pointers (fields/casts/extern), which stay bare `Ptr`.

**The correctness slice (Slice A) is atomic: T1+T2+T3+T4.** The reviews proved the original T2/T3/T4 split is un-shippable in isolation (each leaves a corpus program broken in a verify-clean way). The single landing routes *every corpus-reachable producer* — capturing lambdas, non-capturing lambdas, and **static method refs** (via thunk) — through the uniform `$self`-leading convention, makes param/local/return slots `Func$`, and rewrites the call site unconditionally. Only with all four together do `closure_basic`, `list_hof`, and `function_pointer` stay green.

**Later slices:** bound method-ref thunks (Slice B / T5); lambda-directly-in-call-arg target typing (Slice C / T6, correctly scoped as a pre-pass change); by-value lifetime documentation (T7); `delegate`/`Event` bridge (T8). By-reference capture is explicitly **deferred** (§10).

### 3.1 Alternatives considered & rejected

- **Side-table tagging keyed by value (extend the `closures` HashSet to a value→kind map).** Rejected: an SSA `Value` produced in one function is meaningless in the callee; the tag cannot cross the call boundary, which is the entire bug. Works only within a single function's locals — exactly today's limitation.

- **"Every callee always expects `$self`; pass env-or-null, but keep the value a bare `Ptr`."** This is the variadic-arity convention made uniform. Rejected because the *value* is still a single `Ptr` that cannot distinguish "this is a code pointer, call it directly with null self" from "this is an env, load slot 0." You would have to encode the discriminator in the pointer (low-bit tagging) or always heap-allocate even non-capturing lambdas (defeating zero-cost and adding `malloc` to a hot path). The fat pointer makes the discriminator explicit and free (the `target` field) with no allocation for the bare case.

- **New IR type `IrType::Func{...}` (a backend-special function-value type).** Rejected: it would make `IrType` non-`Copy` or require a `StructId`-like handle anyway, and it forces `newbf-llvm` to learn a new lowering. Since `Func$` is just a two-pointer struct, reusing `IrType::Struct(StructId)` means **zero LLVM backend changes** (it lowers like any 2-field value struct), honoring the sema/llvm dependency rule.

- **Always heap-allocate a `Delegate` object (Beef's literal model).** Beef allocates a heap delegate even for non-capturing binds (BfExprEvaluator.cpp:15821). Rejected for NewBF's no-GC model: that adds a `malloc` (and a `delete`/`scope` lifetime obligation) to *every* function value. The value-struct fat pointer is stack/register-resident and allocation-free except for the capture env itself.

- **Lower *every* `function` type (incl. fields/casts) to `Func$`.** Rejected (Integration blocker): breaks the `BfRtCallbacks` C-ABI table layout and the `Internal.bf:402` raw cast-and-call. Hence the position gating above.

- **vtable-embedded dispatch (treat a function value as a one-method interface).** Rejected: entangles function values with the itable mechanism (explicitly an invariant: "function values are for HOF parameters, not interface polymorphism"). It also costs an extra indirection versus the direct `code`/`target` load.

## 4. Representation & IR changes

**No changes to `IrType`, `InstKind`, or `newbf-ir` at all.** `Func$` is an ordinary value struct; `CallIndirect` already takes `(callee: Value, args: Vec<Value>)`. This respects `IrType: Copy` (we add no variant) and keeps the backend mechanical.

**The `Func$` struct.** Registered as the **very first** struct during the StructTable build, with a stable, well-known name `"$Func"` and `StructId` cached on the `StructTable`:

```
StructDef { name: "$Func", fields: [ FieldDef{name:"code", ty:Ptr}, FieldDef{name:"target", ty:Ptr} ] }
```

Add one field to `StructTable` (owned data only — no lifetime; consistent with the no-lifetime invariant):

```rust
// in StructTable
func_struct: StructId,        // the well-known $Func id, set FIRST in build()
```

> **Default-id hazard (Correctness major).** `StructTable` derives `Default`, so an unset `func_struct` would be `StructId(0)` — a *real, valid* id aliasing the first registered struct, with no panic. Mitigation: register `$Func` **first** in `build()` so `StructId(0)` genuinely *is* `$Func`, and add a build-time assertion `struct_def(func_struct).name == "$Func"` and `fields == [Ptr, Ptr]`. Any `lower_ty_env`/`register_tuple_type` call that runs during early build (e.g. lower.rs:436) is then safe even if it touches a function type, because `$Func` already exists. (The position-gated lowering also means those early field/tuple paths never produce `Func$` anyway.)

A function value of any Beef signature `function R(P…)` in a closure-carrying position lowers to `IrType::Struct(func_struct)`. The signature `(R, [P…])` is *not* part of the representation — it is tracked exactly as today in `fn_sigs` (return + param types), so there is **one** `Func$` layout and no monomorph explosion.

**Calling convention (the one shape).** For a function value `fv : Struct(func_struct)` and a known sema signature `(ret, ptys)`:

```
code   = load (fieldaddr fv, func_struct, 0)   : Ptr
target = load (fieldaddr fv, func_struct, 1)   : Ptr
result = call_indirect code, [target, coerced-args…]   : ret
```

Every emitted callable target's LLVM signature is `code(ptr $self, ptys…) -> ret`. `$self` is param 0 unconditionally. **Because LLVM won't reject an arity mismatch (§1), the call site asserts** `call_args.len() == ptys.len() + 1` before emitting.

**Construction.** A `Func$` value is built in an alloca'd slot (addressable value struct, per the addressability invariant), storing `code` and `target`:

- Non-capturing lambda / static method-ref thunk: `code = global_addr(sym)`, `target = null`.
- Capturing closure: `code = global_addr($lambdaN)`, `target = env` (the `malloc`'d `[cap0 | cap1 …]` — slot 0 no longer holds the code pointer; see §5.1).
- Bound method ref: `code = global_addr($mrefb$T.M)`, `target = obj`.

**Mangling.** Method-ref thunks get deterministic symbols: `$mref$<FullMethodName>` for static, `$mrefb$<FullMethodName>` for bound (the body differs: bound casts and forwards `$self`). The `$Func` struct name is fixed and never mangled. No change to generic mangling (`mangle_generic`).

**ABI note (AOT + JIT).** Both fields are pointer-width; the struct is two words, passed by-value the same as any 2-pointer struct, allocation-free. Crossing a call boundary, the callee param of type `function R(P)` (in a closure-carrying position) is declared `IrType::Struct(func_struct)` — identical type on both sides, so no ABI mismatch. **Attributed calling conventions** (`[CallingConvention(.Stdcall)]`, Platform.bf:128/188) apply to C-ABI function-pointer *fields/externs* — which stay bare `Ptr` — and are out of scope for `Func$` (Risk 7.8).

## 5. Sema / parser / codegen changes

**Parser/AST: no changes.** `AstType::Function { return_ty, params }` and `Expr::Lambda` already exist; method refs are `Expr::Member`. `delegate` is a distinct AST node (`Item::Delegate`, ast.rs:664) and is *not* swept into the `Func$` lowering (§6). The work is entirely in `newbf-sema/lower.rs` (+ the one `StructTable` field).

### 5.0 The position-gated lowering helper (the real change site)

`lower_ty_env`'s `AstType::Function` arm **stays `Ptr`** (so fields/casts/externs are unaffected). Introduce a position-aware wrapper used at param/local/return sites:

```rust
/// Like `lower_ty_env`, but a `function R(P)` in a *closure-carrying* position
/// (param, local, return) lowers to the `$Func` value-struct rather than a bare
/// code pointer. Fields, casts, and extern callback tables must NOT use this.
fn lower_value_ty(ty: &AstType, src: &str, structs: &StructTable, env: TyEnv) -> IrType {
    if let AstType::Function { .. } = ty {
        return IrType::Struct(structs.func_struct);
    }
    lower_ty_env(ty, src, structs, env)
}
```

This is threaded through:
- callee param IR type (`param_ir_ty`, lower.rs:6422/2532) and the param spill slot alloca (2570),
- `MethodSig.params`/`ret` construction (so the call-site coerce at 3888 targets `Func$`),
- function-typed **local** slot type (2959 path) and function-typed **return** type (`ret_ty` in `lower_method`).

Field types (`fill_fields_at`, 1400), cast targets, and extern decls keep calling `lower_ty_env` (bare `Ptr`). With `lower_value_ty` as the single source of truth for these positions, the §5.2 "override the slot type at the registration site" mechanism is **deleted** — those sites reduce to populating `fn_sigs` and removing the `closures` flag.

### 5.1 Producers — make every function value $self-leading; capturing ones build a `Func$`

- **One callee ABI for all lambdas.** Route **non-capturing** lambdas through `emit_closure` with an **empty caps list** (instead of `lower_method`+`extra`). The captures loop becomes a no-op; the signature is uniformly `$lambdaN($self, params…)`. This eliminates the off-by-one risk in `extra`-param indexing (Correctness major) and guarantees one callee ABI. `$self` is ignored by a non-capturing body.

- **`emit_closure` (lower.rs:2634–2692):** bind each capture to `$self[i]` (was `$self[i+1]`, since slot 0 no longer holds the code pointer). The lambda's params remain `Value::Param(i+1)` (after `$self`). `$self` = `target` = env.

- **`Expr::Lambda` (lower.rs:3677–3712):**
  - **Non-capturing:** keep returning a bare `global_addr(name)` as `Ptr` (the value coerces to `Func$` only when it crosses a `Func$`-typed boundary — §5.4). The *emitted body* is now `$self`-leading (above), so the uniform call passes a `null` `$self`.
  - **Capturing:** build a `Func$` slot. Env becomes `malloc(caps*8)` holding **only captures** (drop the slot-0 code pointer); store captures at index `i`. Build `Func$ {code = global_addr($lambdaN), target = env}` in a fresh alloca; return `(load slot, Struct(func_struct))`. Capture recording in `lambda_captures` unchanged; index shifts to `i`.

- **Static method ref `Type.M` (`try_method_ref`, lower.rs:4748):** emit (once, de-duplicated by `full_name` in a thunk set) a thunk `$mref$<full>($self, P…){ return <full>(P…); }`, and return a **bare `Ptr`** `global_addr($mref$<full>)` (so it coerces to `Func$ {code, target=null}` at the boundary like a non-capturing lambda). **This lands in Slice A (T4), not later** — see §3 and §9. (Lands with T3 because `function_pointer.bf` puts a static method ref into a `Func$` local; without the thunk the uniform `code(null, args…)` shifts every arg.)

- **Bound method ref `obj.M`** (new path in `Expr::Member` value position, Slice B/T5): emit thunk `$mrefb$<full>($self, P…){ return ((T)$self).M(P…); }`, build `Func$ {code = global_addr($mrefb$<full>), target = obj_body_ptr}`. The thunk's `$self` forwarding must match the receiver mode: a **class** receiver passes the body pointer; a **value-struct / `mut` / `ref`** receiver (Functions.bf:83 `function StructB FuncMut(mut StructB this, …)`) must pass the address with the right mode (Risk 7.9). First slice supports class receivers; value-struct receivers are flagged unsupported until the mode forwarding is implemented.

Thunks are collected in a `method_ref_thunks: HashSet<String>` (keyed by symbol to de-dup, Risk 7.6) populated during lowering and drained in the emit phase next to `lambda_emits` — the established two-pass convention.

### 5.2 Consumers — function-typed params, locals, and returns are `Func$`

All three positions get their IR type from `lower_value_ty` (§5.0), so caller value and callee param are both `Func$` and the call-site coerce auto-wraps a bare-`Ptr` arg:
- **Param registration (lower.rs:2589–2601):** keep recording `(ret, ptys)` in `fn_sigs`; the slot type now comes from `lower_value_ty` (via `param_ir_ty`). **Delete the per-name `closures` flag from the call decision.**
- **Local registration (lower.rs:2956–2984):** slot type via `lower_value_ty`; drop the `closures.insert` and its detection block.
- **Return type:** `lower_method`'s `ret_ty` uses `lower_value_ty` so `Return` coerces the produced `Func$` to a `Func$` ret (no-op), not to `Ptr` (which would `undef` it). This is what makes T7 (`closure_returns_fn`) possible.
- Remove the `closures` HashSet field, its init, the detection block, and the `contains()` branch **in one commit** (Planning minor) so there are no dead fields / unused-insert warnings; `cargo build` must be clean.

### 5.3 The call site — one shape (in-callee `f(args)`)

`f(args)` through a function-typed name (lower.rs:3836–3866) becomes unconditional:

```rust
if let Some((ret, ptys)) = self.fn_sigs.get(name).cloned()
    && let Some((slot, _)) = self.lookup(name)
{
    let fid = self.structs.func_struct;
    let code   = self.fb.load(self.fb.field_addr(slot.clone(), fid, 0), IrType::Ptr);
    let target = self.fb.load(self.fb.field_addr(slot.clone(), fid, 1), IrType::Ptr);
    let mut call_args = vec![target];
    for (i,(v,t)) in arg_vals { call_args.push(self.coerce(v,t, ptys.get(i).copied().unwrap_or(t))); }
    debug_assert_eq!(call_args.len(), ptys.len() + 1); // LLVM won't catch arity drift (§1)
    return (self.fb.call_indirect(code, call_args, ret), ret);
}
```

No branch on closure-ness. **The caller side (passing a function value into a generic/overloaded method) is covered separately:** the generic-callee path (3772–3820) and overload path (3882–3891) coerce each arg to its resolved param type, which is now `Func$` (via `lower_value_ty` in `MethodSig.params`); the capturing-lambda producer returns `Func$`, so `coerce(Func$→Func$)` is a no-op and `coerce(Ptr→Func$)` fires only for non-capturing/method-ref args (§5.4). Both legs must be verified by `closure_arg.bf` driving the **generic** `Map` (the path that actually crashed), not a hand-rolled HOF.

### 5.4 Coercion — bare `Ptr` ↔ `Func$`, and null

`coerce` (lower.rs:6068) gains:
- **`from == Ptr && to == Struct(func_struct)`** ⇒ build a `Func$` in a fresh alloca with `code = v`, `target = null`, return the loaded struct. This auto-wraps a non-capturing lambda or static-method-ref thunk pointer when it crosses into a `Func$`-typed slot/param/return. **`Const::Null` (Ptr)** flows through this same arm to `Func$ {code=null, target=null}`, giving `function R(P) f = null;` a defined value.
- **`Func$ → Ptr` is NOT allowed** (would drop `target`) — except the design must still handle the **C-ABI cast** `(function void(void*))field` (Internal.bf:402): that cast target is a *field/cast position*, which lowers to bare `Ptr` (never `Func$`), so it stays a `Ptr→Ptr` reinterpret and is unaffected. There is no `Func$→Ptr` path in well-formed code.

**Null semantics (Correctness/Integration minor):** `function R(P) f = null;` ⇒ `Func${null,null}`. `f == null` lowers to `f.code == null` (a single pointer compare on the code field), not a struct compare. **Calling a null function value is UB** (same as Beef) — no guard is emitted. A `null`-init test (`fn_null.bf`) pins this.

### 5.5 Zero-cost / fast paths (what actually stays bare)

The bare-`Ptr` zero-cost path is preserved for the positions that are *not* `Func$`:
- **C-ABI function-pointer fields** (`BfRtCallbacks`), **casts**, and **extern callback tables** stay bare `Ptr` (position gating, §3/§5.0). Their layout and raw cast-and-call (Internal.bf:402) are unchanged — this is what keeps the verify corpus at 152/152 with **no layout change**.
- **Deferred direct-call peephole:** eliding the `Func$` wrap when a non-capturing lambda is passed directly to a statically-known `$lambdaN` callee — out of scope (§10).

> **Correction to the original draft:** `list_hof.bf` does **not** keep its current IR. Its function-typed locals become `Func$` (alloca + 2 stores + load). It still **passes behaviorally** (returns 18), now with a `null` `$self` in the indirect call. The acceptance criterion is **behavioral (run-corpus value)**, not IR-shape identity (Planning major).

### 5.6 SSA-dominance correctness

The `Func$` construction (alloca + two stores + load) happens at the **producer site**, in the block where `code`/`target`/captures are live — exactly where the env `malloc` already happens (dominance-safe today, journal §48). **Correction (Correctness minor):** allocas emit into the **current** block (`func.rs:129`/`110`), not a dedicated entry block; dominance holds because the `Func$` alloca + stores + load are **co-located in the producer block** and dominate the use. At the call site, `field_addr`/`load` on the param/local slot dominate trivially. No phi nodes are introduced; nothing is produced in a conditional sub-block and used after a merge — the same shape as the existing closure path that passes 152/152. **Known limitation (documented, not fixed):** a `Func$`/env built inside a **loop** allocas (and the env `malloc`s) every iteration — unbounded stack/heap growth, same class as the existing per-iteration env leak (§10).

## 6. Interactions

- **C-ABI function pointers / corlib callback tables:** preserved by position gating (§3). Fields stay bare `Ptr`; `BfRtCallbacks` keeps its 8-byte-per-field layout; the verify corpus shows **no layout diff**. A T1 acceptance assertion pins this.
- **`delegate` types:** `delegate R(P)` is a *distinct* AST node (`Item::Delegate`) from `function R(P)`. The `Func$` lowering keys **only** on `AstType::Function`, so `delegate`-typed fields/params/values in the corpus (Delegates.bf, FuncRefs.bf, `Event<delegate …>` in Console.bf) are untouched in the first slices. Beef's `delegate` is a heap GC object (`{mFuncPtr, mTarget}`); making it layout-compatible with `Func$` is deliberate **T8 groundwork**, not an automatic consequence.
- **Vtables / virtual dispatch:** orthogonal and untouched. A bound method ref of a *virtual* method thunks to a *static* forwarding call in Slice B (binds the concrete `full_name`); runtime virtual dispatch through a bound ref is deferred (§10). Function values never become vtable calls (invariant preserved). `mref_bound_arg.bf` uses a **non-virtual** method to avoid silently testing the unsupported path (Planning minor).
- **Monomorphization:** `Func$` has *one* layout for all signatures, so monomorphizing `Map<int,int>` vs `Map<float,float>` creates no new representation types — only the `fn_sigs` `(ret, ptys)` differ. No mono explosion (Risk 7.2).
- **Target-typing (Slice C / T6) — correctly scoped:** `collect_lambdas_stmt` is a *syntactic* pre-pass that fixes a lambda's param types at collection time and only matches lambdas that initialize a `function`-typed local; it does **not** walk call args, and `pick_overload` runs later. So an inline `Map(xs, x => x*3)` lambda gets no `$lambdaN` symbol today and `Expr::Lambda` returns `undef`. T6 therefore requires a **pre-pass change** (extend `collect_lambdas_stmt` to detect lambda-in-call-arg position and resolve the callee param types there) **or** a deferred-emit mechanism that records param types at the call site. It is **not** a thin "read pick_overload's sig" add-on; T6 is split into T6a (collection) and T6b (target-typing) accordingly (§9).
- **Comptime:** a `Func$` is a normal two-pointer struct the JIT already handles; **constructing/storing** one at comptime is fine. But its `code` field is a `global_addr` to a lambda/thunk symbol that is only meaningful at AOT/JIT-run time — **comptime must not dereference (call through) it**. JIT-evaluating a *call through* a function value at comptime is out of scope for the first slices. The known JIT FP-constant-pool limit (MEMORY) is irrelevant (no float constants).
- **AOT vs JIT:** identical IR; the two-word struct lowers the same. Use the run-corpus JIT harness for value checks > 255; keep new test expectations ≤ 255 for AOT-safety.

## 7. Risks & mitigations

1. **LLVM verifier "dominate all uses":** mitigated by building `Func$` at the producer site, same block as captures; no cross-merge production (§5.6). Gate: corpus.rs 152/152.
2. **Monomorph explosion:** avoided by design — one `Func$` layout; only `fn_sigs` varies.
3. **Silent ABI miscompile (the dominant failure mode):** the LLVM backend builds indirect-call types from *arg* types, not callee signatures, so an arity/type mismatch is **verify-clean** (§1). Mitigations: (a) the **run corpus is the authoritative gate** for the convention slices — `function_pointer.bf`, `lambda_basic.bf`, `lambda_params.bf`, `closure_basic.bf`, `list_hof.bf` are explicit accept criteria; (b) the call site asserts `call_args.len() == ptys.len()+1`; (c) a sema unit test iterates `Module.funcs` and asserts every `$lambda*`/`$mref*` has `param[0].ty == Ptr` and `$Func` has exactly two `Ptr` fields.
4. **C-ABI layout regression (Integration blocker):** position gating keeps fields/casts/externs bare `Ptr`; `BfRtCallbacks` layout is asserted unchanged in T1. `lower_ty_env(Function)` stays `Ptr`; only `lower_value_ty` yields `Func$`.
5. **sema must not depend on newbf-llvm:** no backend changes (Func$ is a plain struct; CallIndirect exists). T3 accept verifies no new `use newbf_llvm` appears in sema.
6. **Method-ref thunk collisions / double emission:** de-dup by symbol in a `HashSet<String>`, drained once.
7. **Null-target deref:** only capturing closures and bound refs set non-null `target`, and only those bodies read `$self`; non-capturing lambdas / static thunks ignore `$self`. Asserted by construction.
8. **Attributed calling conventions** (`[CallingConvention(.Stdcall)]`): apply only to C-ABI function-pointer fields/externs (bare `Ptr`), not `Func$`. Out of scope; flagged if a `Func$`-position type ever carries the attribute.
9. **Value-struct / `mut` / `ref` receivers in bound method refs** (Functions.bf:83): the `$mrefb` thunk must forward `$self` in the receiver's mode. First slice supports class receivers; value-struct receivers are flagged unsupported until mode forwarding lands.
10. **Capturing a function value (a `Func$`, 16 bytes) breaks the 8-byte env-slot stride** (Correctness minor): the env stores captures on a fixed 8-byte stride sized by the value type, which truncates a 16-byte `Func$` capture. **Decision:** document ≤8-byte-scalar by-value capture as the supported set; capturing a `Func$` (closure-capturing-closure) is **unsupported** until per-slot sizing lands (§10).
11. **By-value capture diverges from Beef's by-ref semantics** (not just lifetime): a closure capturing a mutable outer local will **not** observe later writes to that local. This is an *observable* semantic difference vs Beef, documented in §10 (T7 journal note).

## 8. Testing strategy

**The run corpus — not the verify corpus — is the authoritative gate for the convention slices**, because the verifier cannot reject function-value arity/ABI mismatches (§1). All gates must stay green at every task boundary:
- **Run corpus** (run_corpus.rs): existing programs incl. `closure_basic → 57`, `list_hof → 18`, `function_pointer → 12`, `lambda_basic`, `lambda_params` must keep passing. **These are the real gate for T2–T6.**
- **Verify corpus** (corpus.rs): 152/152 LLVM-clean — catches dominance/type errors and, via the T1 layout assertion, the `BfRtCallbacks` regression. Necessary but **not sufficient** for convention changes.
- **Parser corpus:** 152/152 (no parser change).

New run-corpus programs (each `Program.Main → int32`, `// expect: N` with N ≤ 255, **pinned before T3 lands** — an unpinned expectation is not an acceptance criterion):

1. `closure_arg.bf` → captures `b`, builds `addB = a => a+b`, passes it to a **generic** `Map<int,int>`, then folds — concrete expected value pinned in the program header. (The §49 crash, now correct.)
2. `closure_capture_two.bf` → captures two outer locals, passed to `Filter`; validates multi-capture indexing (slot `i`).
3. `mref_static_arg.bf` → static method ref `Math.Square` passed to `Map` via `$mref$` thunk (lands in Slice A / T4).
4. `mref_bound_arg.bf` → bound **non-virtual** instance-method ref `acc.Add` passed to `Fold` (T5); validates `target=receiver`.
5. `lambda_direct_arg.bf` → inline lambda `Map(xs, x => x*3)` (T6); no intermediate `function`-typed local.
6. `closure_returns_fn.bf` → returns a function value whose env was `malloc`'d (heap, outlives the frame); confirms by-value capture survives return and exercises the `Func$` **return type**.
7. `fn_null.bf` → `function R(P) f = null;` then `f == null` is true; pins null semantics (calling it is UB, not tested).

Plus Rust unit tests in lower.rs: `$Func` is `StructId(0)` with 2 `Ptr` fields; every emitted `$lambda*`/`$mref*` has `Ptr` param 0; every `Func$` indirect call supplies exactly `1 + ptys.len()` args; the `BfRtCallbacks` struct retains its bare-`Ptr` field layout/size.

## 9. Task breakdown (ordered, agent-assignable)

> **Slice A (the correctness landing) is atomic: T1+T2+T3+T4 land together** (or in strictly that order with T3/T4 in one commit). The reviews proved each cannot keep gates green alone (T2 breaks `list_hof`; T3-without-T4 breaks `function_pointer`; the call-site rewrite and env-layout change must be simultaneous). T1/T2 are still useful sub-steps but their *standalone* acceptance is only "compiles + verify 152/152", **not** behavioral.

**T1 — Register `$Func` first; add `lower_value_ty`; establish position gating.**
Scope: `StructTable.func_struct` field; register `$Func` as `StructId(0)` in `build()` with the default-id assertion; add `lower_value_ty` (§5.0); **keep `lower_ty_env(Function) = Ptr`**. Deps: none. Accept: unit test `func_struct == StructId(0)` with fields `[code:Ptr, target:Ptr]`; **`BfRtCallbacks` keeps its 8-byte-per-field layout**; verify 152/152, parser 152/152, run-corpus unchanged (no behavior change yet).

**T2 — Single lambda calling convention ($self-leading), env layout unchanged.**
Scope: route *all* lambdas (capturing and non-capturing) through `emit_closure` so every `$lambdaN` is `$self`-leading; non-capturing bodies ignore `$self`. **Do NOT yet drop the slot-0 code pointer or re-index captures** — that stays coupled to the call-site rewrite in T3. Deps: T1. Accept (standalone, behavior-preserving): verify 152/152; the slot-0 env layout and old call site still agree, so `closure_basic → 57` and `list_hof → 18` still pass. *(This is the genuinely layout-neutral half of the original T2.)*

**T3 — `Func$` producers + consumers + uniform call site + env re-layout (atomic with T4).**
Scope: `lower_value_ty` at param/local/return sites (delete the per-site slot-type overrides and the entire `closures` field/init/detection/branch in one commit); `Expr::Lambda` capturing case builds a `Func$` and returns `Struct(func_struct)`, env holds only captures at index `i`; `emit_closure` reads captures at `$self[i]`; the call site (§5.3) becomes the unconditional code/target load with the arity assert; `coerce` gains `Ptr↔Func$` and null handling (§5.4); the generic/overload caller legs coerce args to the now-`Func$` param type (§5.3). Deps: T1, T2. **Co-lands with T4.** Accept (run-corpus is the gate): `closure_arg.bf`, `closure_capture_two.bf`, `fn_null.bf` pass; `closure_basic`, `list_hof`, `lambda_basic`, `lambda_params` still pass; verify 152/152; `cargo build` clean (no dead `closures`); no `use newbf_llvm` in sema. **This is the slice that fixes the §49 segfault.**

**T4 — Static method-ref thunks (part of Slice A; co-lands with T3).**
Scope: `try_method_ref` (4748) emits a de-duplicated `$mref$<full>($self,P…){return <full>(P…);}` thunk and returns `global_addr($mref$<full>)` as bare `Ptr` (coerces to `Func$ {code,target=null}`); thunk-collection set + emit-pass drain. Deps: T1, T2; **must land with T3** (a static method ref in a `Func$` local is reachable via `function_pointer.bf`). Accept: `function_pointer.bf → 12` still passes; `mref_static_arg.bf` passes; gates green.

**T5 — Bound instance method-ref thunks.**
Scope: new `Expr::Member` value-position path → `$mrefb$<full>($self,…){ ((T)$self).M(…) }`, `target=receiver` (class receivers; value-struct/`mut`/`ref` receivers flagged unsupported, Risk 7.9). Deps: T4. Accept: `mref_bound_arg.bf` (non-virtual method) passes; journal note on the virtual-dispatch and value-receiver deferrals; gates green.

**T6a — Collect inline lambdas in call-arg position.**
Scope: extend `collect_lambdas_stmt` to walk into `Expr::Call`/`Expr::Generic` args and assign `$lambdaN` symbols to inline lambdas (the pre-pass change Slice C actually needs). Deps: T3. Accept: an inline-lambda program lowers without `undef`; gates green.

**T6b — Target-type inline-lambda params from the resolved callee sig.**
Scope: supply the inline lambda's param types from `pick_overload`'s resolved `ptys` (eagerly-evaluated-arg interlock), not a declared local type. Deps: T6a. Accept: `lambda_direct_arg.bf` passes; gates green.

**T7 — `closure_returns_fn` + by-value lifetime/semantics documentation.**
Scope: test + journal note pinning (a) by-value capture survives return *only because the env is leaked* — any future env-free work must exclude escaping function values; (b) the **observable** by-ref divergence from Beef (captured mutable locals don't see later writes); (c) the ≤8-byte-capture limitation. Requires the `Func$` **return type** (T3). Deps: T3. Accept: `closure_returns_fn.bf` passes; journal §-entry added.

**T8 (optional, after T5) — Delegate/Event bridge groundwork.**
Scope: when `System.Delegate` is added, make it layout-compatible with `Func$` (same two pointer fields) so a function value is assignable to a `Delegate` local — without sweeping `delegate`-typed fields/params into `Func$` automatically (they remain a distinct AST node, §6). Deps: T5 + Delegate stdlib existing. Accept: a `delegate`-typed local holds a function value and is callable; gates green. (Design-only until Delegate lands.)

## 10. Open questions / decisions deferred

- **By-reference capture (Beef's model):** deferred. First slices are by-value; the **observable** difference is that a closure capturing a mutable outer local does not see later writes (not merely a lifetime concern). By-ref requires the env to store pointers to outer locals plus lifetime tracking tied to NewBF's `scope`/ownership model (CORETYPES.md). **Decision: ship by-value; document the semantic divergence in the journal.**
- **Capturing a function value / wide captures:** the env uses a fixed 8-byte slot stride, so capturing a 16-byte `Func$` (closure-capturing-closure) or any >8-byte value struct is **unsupported** until per-slot sizing (sum of `sizeof` with offsets) lands.
- **Recursion through a function value:** a self-referential closure captures the pre-assignment (undef/null) value under by-value capture — a known edge case, deferred with by-ref capture.
- **Capturing `this`:** captured by-value like any outer local in the first slice; a bound method ref (`obj.M`) is the explicit cheaper way to carry a receiver. Auto-converting `this`-capturing closures to bound refs is deferred.
- **Virtual dispatch through a bound method ref:** Slice B binds the concrete `full_name`; runtime virtual dispatch (thunk loads the receiver's vtable) is deferred.
- **Value-struct / `mut` / `ref` receivers in bound refs:** the `$mrefb` thunk forwarding for non-class receivers is deferred (Risk 7.9).
- **Env `delete`/lifetime for closures:** capturing closures `malloc` an env that is never freed (a leak, same as today's §48 path; in-loop builds leak per iteration). `closure_returns_fn` is correct *only* because of this leak — any future env-free work must exclude escaping function values. Integrating env lifetime with `scope`/`delete` is a follow-on.
- **Attributed calling conventions on `Func$` positions:** out of scope; only C-ABI bare-`Ptr` fields/externs honor `[CallingConvention(...)]` today.
- **Function-value equality / `Delegate.Equals`:** `f == null` is defined as `f.code == null`; full structural equality (`Func$.Equals` comparing both fields) is deferred to the Delegate bridge (T8).
- **Zero-cost direct-call peephole:** eliding the `Func$` wrap when the callee is statically the same `$lambdaN` — deferred; the one-null-arg cost is accepted.
