# Type-Aware Generic-Method Mangling + Instance Generic Methods

## 1. Problem & goal

Generic methods in NewBF are mangled by **global method name only**: `mangle_generic(name, args)` (`lower.rs:6432`) produces `"{name}${type_codes(args)}"`, e.g. `First<int32>` → `First$i32`, regardless of which type owns `First`. The monomorph tables `gen_method_sigs: HashMap<String, MethodSig>` (`lower.rs:94`) and `gen_method_monos: Vec<(String, String, env)>` (`lower.rs:97`) are keyed/recorded by this owner-blind symbol. The decl index `index_generic_methods` (`lower.rs:280`) keys by name with `out.entry(name).or_insert(...)` — **first-writer-wins** (`lower.rs:295`).

Two consequences:

1. **Collisions.** Two same-named generic methods in different types with the same type-args collapse to one symbol *and* the **first-indexed** decl wins for *all* owners' calls (the second owner's body is never indexed or emitted). §107 (journal lines 2591–2607) documents this concretely: a user-defined `First<T>` collides with corlib `List.First` — both mangle to `First$…`, so all call sites bind to whichever decl `index_generic_methods` saw first.

2. **No instance generic methods.** `obj.Map<R>(f)` is unsupported. The generic-method lowering path (`lower.rs:3769–3796`) only ever emits a *static, receiver-less* direct call: it mangles `name + targs`, looks up `gen_method_sigs`, and calls with `sig.params.len() == args.len()` (no `this`). Generic methods are skipped from the per-type method table in both `fill_members_at` (`lower.rs:1529`) and `lower_type_at` (`lower.rs:2269`). The corlib workaround (`List.bf:135–151`) makes `Map`/`Filter`/`Fold` **static** generics on a `Functional` class taking an explicit `self`, e.g. `Map<T,R>(List<T> self, function R(T) f)`, **called bare cross-class** from `Program.Main` (`list_hof.bf:21–23`) — which works *only* because the bare call and the `Functional.Map` body collapse to the same global symbol today.

**Concrete failing example (today):**

```beef
class Box { public T Get<T>(T x) { return x; } }      // a Get<T>
class Sack { public T Get<T>(T x) { return x; } }     // another Get<T>
class Program {
    public static int32 Main() {
        Box b = new Box(); Sack s = new Sack();
        return b.Get<int32>(40) + s.Get<int32>(2);    // wants 42
    }
}
```

Both `Get<int32>` mangle to `Get$i32`; `index_generic_methods` keeps only the first; instance dispatch on a receiver is unimplemented. Each call lowers to a receiver-less direct call into the single first-indexed `Get$i32` body — garbage at best, a verifier/link failure at worst.

**Target capability (v1, honest scope).** (a) Mangle every generic-method monomorph by **owner + name + type-args** so same-named methods in different *concrete* owners never collide. (b) Resolve and lower **instance** generic-method calls `obj.M<R>(args)` with a real `this` receiver, for receivers whose **static type is a concrete (non-generic) owner**. (c) Keep working: the **bare same-class** (`M<T>(x)`), the **qualified static** (`Type.M<T>(x)`), the **bare cross-class static** (`Map<…>(xs, f)` — the `list_hof.bf` shape), and the **static-on-`Functional`** forms; keep all monomorph collection/emission green.

**Explicitly NOT in v1** (deferred, with guards so they fail loudly, never silently mis-dispatch):
- Instance generic methods on a **generic owner** (`List<int64>.Map<R>`) — Task B1. *This is the corlib HOF case*, so v1 ships **no run-corpus program that uses `obj.M<R>()` on a real generic type**; the corlib `Functional` static form is retained unchanged.
- **Inherited** generic instance methods (declared on a base, called on a derived receiver) — diagnosed, not silently mis-mangled.
- **`virtual`/`override`** generic methods — rejected at collection (one vtable slot can't hold a family of monomorphs).
- **`[Comptime]`** generic methods — rejected at collection (the gen-method emission loop does not register comptime symbols; see §6).
- **Recursive/transitive** generic-method instantiation where the inner call's type-args are abstract (`M<U>` inside `M<T>` with `U` a type-param) — diagnosed; concrete-arg self-calls (`M<int32>` inside `M<T>`) are supported via the normal gen-method path.

## 2. Current state (file:line)

- **Mangling.** `mangle_generic(name, args)` → `"{name}${type_codes(args)}"` (`lower.rs:6432`). `type_codes` encodes each `IrType` to a compact code string (`lower.rs:2196`), correct and reusable; it **never emits `.`**, so `.` is a safe owner separator.
- **Tables.** `StructTable.gen_method_sigs: HashMap<String, MethodSig>` (`lower.rs:94`), `gen_method_monos: Vec<(String, String, Vec<(String, IrType)>)>` = (mangled symbol, method name, env) (`lower.rs:97`). No owner stored anywhere.
- **Decl index.** `index_generic_methods` → `GenMethodDecls = HashMap<String, (&Member, &str)>` (`lower.rs:276, 280–302`), keyed by **name only**, `or_insert` = **first-writer-wins** (`lower.rs:295`).
- **Collection.** `record_method_inst(name, targs, …)` (`lower.rs:633–682`): mangles, dedups by `gen_method_sigs.contains_key`, builds a receiver-less `MethodSig` (`is_instance:false`, `variadic:None`), pushes the mono. Driven by `collect_insts_expr` for both `Ident`-base and `Member`-base `Expr::Generic` callees.
- **Call lowering.** `Expr::Call` with `Expr::Generic` callee (`lower.rs:3769–3796`): `generic_callee_name` (`lower.rs:6439`, returns `Option<&str>`) yields the name from `Ident` or `Member`; mangle; look up `gen_method_sigs`; arity guard `sig.params.len() == args.len()`; emit a **receiver-less** direct call.
- **Emission.** The mono loop (`lower.rs:2036–2062`) runs **once, after `lower_items` returns** (`lower.rs:1957`), re-finds the decl by name, lowers with `this_ty = None`, `sigs = &empty`, `extra = &[]`.
- **Pipeline phasing (load-bearing).** `lower_program` (`lower.rs:1926`): `StructTable::build(&all)` does **all** collection; then `m.structs = structs.defs.clone()` (`:1939`); then `lower_items(... &structs ...)` emits every body under an **immutable** `&structs` (`:1957`); then the gen-method loop (`:2036`) runs once. The `Lowerer` holds `structs: &'a StructTable` (`:2711`) — **shared, immutable**. Nothing during call lowering can mutate the mono tables.
- **Instance non-generic dispatch (the model to mirror).** `lower_method_call` (`lower.rs:5786–5901`): eval args once → `arg_tys`/`arg_vals`; three paths — base-chain, `Type.M` static, `obj.M` instance via `struct_base` (`:4955`) + `pick_overload(members:true)` (`:5854`); prepend `body_ptr` only `if sig.is_instance` (`:5861`); variadic packing (`:5864`); `pidx` starts at 1 for instance (`:5874`); vtable dispatch when `name ∈ vslots` (`:5884`).
- **`struct_base`.** `lower.rs:4955` — a `Lowerer` method; resolves a receiver to `(body_ptr, owner_id)` via the **live locals scope** and `expr`, emitting IR. **Exists only at lowering time.**
- **Generic methods skipped from the per-type table.** `fill_members_at` (`lower.rs:1529`) and `lower_type_at` (`lower.rs:2269`) both `continue` when `generic_params` is non-empty.
- **`this_slot`.** `lower.rs:2718` — `None` in static contexts; set only for instance methods/ctors/dtors (`:2548`). **There is no enclosing-type field on the `Lowerer`.**
- **Prefix table.** `t.prefixes[id]` = the owner's mangled symbol prefix ending in `.` (`"Box."`, `"List$i64."`, `"Outer.Inner."`), built at registration (`lower.rs:313, 947`).
- **`MethodSig`.** `lower.rs:2082` — `{ full_name, ret, params (this-leading for instance), is_instance, variadic }`.
- **Corpus gate mechanism.** The verify/parser corpora walk `feature-suite/src` dynamically (`corpus.rs:46,120,152`) and assert `clean == files.len()` — a **100% ratchet**, not a hardcoded `152` (`corpus.rs:105,190`). Adding a feature-suite file raises the denominator to 153 and still demands 153/153 clean.

## 3. Approach

**Chosen design: owner-prefixed symbols + a composite resolution key, with a *retained* `None`-owner fallback for bare cross-class statics, and a receiver-aware generic-method dispatch path that mirrors `lower_method_call`. Collection is made authoritative for v1's supported shapes (no lazy-at-lowering pass — see §3.4), so the call site can hard-assert resolution.**

Four pillars:

### 3.1 Owner-qualified mangling

```rust
// Some(id): "{prefixes[id]}{name}${codes}"  e.g. "Box.Get$i32", "List$i64.Map$R"
// None:     "{name}${codes}"                 (free / bare-cross-class static, == today)
fn mangle_generic_method(owner: Option<StructId>, name: &str, args: &[IrType],
                         t: &StructTable) -> String
```

`prefixes[id]` already encodes the owner's full path *and its monomorph args* (`"List$i64."`), giving generic-on-generic disambiguation **for free** when B1 lands: `List<int64>.Map<R>` → `List$i64.Map$R`, distinct from `List<float>.Map<R>` → `List$f32.Map$R`. The old `mangle_generic` (for generic *type* monomorphs, `Box<int>` → `Box$i64`) stays unchanged.

### 3.2 Composite resolution key + retained `None` bucket

```rust
type GenMKey = (Option<StructId>, String, String);   // (owner, name, type_codes(args))
gen_method_sigs:  HashMap<GenMKey, MethodSig>,
gen_method_monos: Vec<GenMethodMono>,
struct GenMethodMono { owner: Option<StructId>, sym: String, name: String,
                       env: Vec<(String, IrType)> }
```

Keying by the *triple* (not the mangled string) means resolution never re-parses a symbol and two owners can't alias even under a future ambiguous mangling. The mangled `sym` is stored for emission/call as the IR function name.

**Decl index becomes `GenMethodDecls = HashMap<(Option<StructId>, String), Vec<(&Member, &str)>>`** — keyed by `(owner, name)`, **value is a `Vec`** so multiple **overloads** of a same-named generic method in one owner coexist (review: a single owner can have `M<T>(T)` and `M<T>(T,T)`). `index_generic_methods` resolves each enclosing `TypeDecl` to its `StructId` via `by_name` (names are registered in pre-pass step 1, before this index runs) and **also inserts a `(None, name)` entry** (the bare-cross-class fallback bucket), appending to the `Vec`. Among a `Vec`, the matching overload is picked by **arity of explicit params vs. type-args + value-args** at collection/lowering (mirroring `pick_overload`'s arity discrimination).

### 3.3 `Lowerer.cur_type` — enclosing-type identity for static and instance contexts

Add `cur_type: Option<StructId>` to the `Lowerer` (`lower.rs:2755`), set in `lower_method` from a new parameter threaded from `lower_type_at`'s `owner_id` (`:2231/:2298`), for **both static and instance** methods. `this_slot` continues to govern *whether to prepend `this`*; `cur_type` governs *bare-call owner identity*. This makes the call site and the collector agree on the bare-owner rule independent of static/instance (the integration review's static-method blocker).

### 3.4 Collection is authoritative — no lazy-at-lowering pass

The draft's "lazy fallback collection at lowering time" is **architecturally impossible** and is removed: during call lowering the `Lowerer` holds `structs: &StructTable` (shared, immutable, `:2711`), the mono tables are already cloned into the module (`:1939`), and the single emission loop (`:2036`) has not-yet-run but iterates a `Vec` that must be complete before `build()` returns. There is no `&mut` and no fixpoint.

Therefore **collection resolves every owner v1 supports, so lowering always finds the key**, and **lowering hard-asserts presence** (a clear diagnostic on miss, never a dangling `call`). To resolve **instance** receiver owners in the collector — which has *no* `Lowerer` and *no* locals scope — we add a **minimal, explicit local/param/field type scope** to the collector (the integration/correctness reviews' shared blocker): thread `locals: Vec<(String, IrType)>` through `collect_insts_stmt/_expr`, populated from
- the current method's **params** (declared `AstType` → `lower_ty_env`),
- **`Stmt::Local`** declarations with an explicit declared type (skip `var`/inferred — see below),
- the current `TypeDecl`'s **fields** and **`this`** → `cur_owner`.

Receiver owner is then resolved for exactly the shapes `struct_base` resolves **identically** at lowering time: a **declared-typed local/param**, **`this`**, a **simple field of `this`**, and **`new T()`**. Any value receiver the collector cannot resolve to a concrete-owner `StructId` (inferred `var` locals, call-return receivers like `getBox().Get<…>()`, base-class-inherited methods, generic owners) is **not silently skipped** — it produces a `diag::unsupported` ("generic instance call on a receiver whose type cannot be resolved at compile time") so the failure is a clean diagnostic, never a divergence from lowering. This makes the collector and lowering **provably resolve the same owner** for every shape that reaches emission.

### 3.5 Receiver-aware dispatch, mirroring `lower_method_call`

- **Bare `M<T>(x)`.** Owner candidates, in order: `Some(cur_type)` (same-class), then `None` (free/bare-cross-class static, e.g. `list_hof.bf`'s `Map`). Pick the first whose `GenMKey` resolves. If `sig.is_instance` and `this_slot` is present, prepend the current `this`; otherwise receiver-less.
- **Qualified static `Type.M<T>(x)`.** Owner = `by_name[Type]` when `Type` is a registered type (not a local). Receiver-less direct call.
- **Instance `obj.M<T>(args)`.** Owner = the receiver's `StructId` from `struct_base(obj)`; mangle with `Some(owner_id)`; prepend `body_ptr` as `call_args[0]`; emit a **direct** call (never virtual — generic methods are non-virtual). If `(owner, name, codes)` is absent (e.g. inherited-from-base, generic owner), **diagnose** — do not emit a dangling call.

**Virtual generic methods:** rejected at collection (assert + diagnostic). Generic instance calls always emit a **direct** call; no `vslots` lookup.

### Alternatives considered & rejected

- **Embed `StructId` number in the symbol (`Get$S7$i32`).** Rejected: arena indices are unstable across edits and meaningless in a disassembly. `prefixes[id]` is stable, human-readable, already correct (handles nesting + monomorph args). Beef itself mangles every method with its owner *type* (`BfMangler::MangleMethodName` takes a `BfTypeInstance*`).
- **Flat concat for generic-on-generic (`List$Map$i64i32`).** Rejected: ambiguous. The dot-separated prefix (`List$i64.Map$i32`) is unambiguous because `type_codes` never emits `.`.
- **Owner *mandatory* everywhere (drop the `None` bucket).** **Rejected** (the draft chose this; both reviews flagged it as a blocker). `list_hof.bf` calls `Map<int32,int32>(xs, f)` **bare** from `Program.Main`, but `Map` lives on `Functional` — with owner mandatory the bare call resolves owner `Program` (or `None`) and misses the `(Functional, Map)` key → regression. We **retain `None`** as a legitimate fallback bucket for bare calls that don't match an enclosing-type generic method.
- **Lazy collect-at-lowering fallback.** **Rejected** as infeasible (§3.4): immutable `&StructTable` during emission, no fixpoint. Replaced by authoritative collection + a hard assert/diagnostic at lowering.
- **Fat-pointer / itable generic dispatch.** Rejected: generic methods are statically monomorphized; owner + type-args are known at the call site, so a direct call to a uniquely-mangled symbol is correct and fastest.
- **Reuse the per-type `methods` table for generic methods.** Rejected for v1: that table is keyed by concrete param types for overload resolution; generic params aren't concrete pre-mono. Keep `gen_method_*` separate; mirror `pick_overload`'s receiver/`this` ABI.

## 4. Representation & IR changes

No `newbf-ir` changes. A generic-method monomorph is just another `Function` whose `name` is the owner-mangled symbol and whose `params[0]` is `Ref(owner_id)` when instance. `IrType` stays `Copy`; `StructTable` stays lifetime-free (all new data owned: `StructId` (u32), `String`, `IrType`).

**Changed sema types (`lower.rs`):**

```rust
type GenMKey = (Option<StructId>, String, String);     // owner, method name, type_codes(args)

struct GenMethodMono {                                  // replaces the 3-tuple at :97
    owner: Option<StructId>,
    sym:   String,                                      // owner-mangled IR symbol
    name:  String,                                      // template method name (decl lookup)
    env:   Vec<(String, IrType)>,                       // method's own type-param bindings
}

// StructTable (:94,:97)
gen_method_sigs:  HashMap<GenMKey, MethodSig>,
gen_method_monos: Vec<GenMethodMono>,

// Decl index (:276): keyed by (owner, name), value a Vec for overloads,
// with a duplicate (None, name) entry as the bare-cross-class fallback bucket.
type GenMethodDecls<'a> = HashMap<(Option<StructId>, String), Vec<(&'a Member, &'a str)>>;
```

**`Lowerer` (`:2755`):** add `cur_type: Option<StructId>`.

**Mangling.** New `mangle_generic_method(owner, name, args, t)` (§3.1). `type_codes`, `mangle_generic` unchanged. `generic_callee_name` is **retired from the instance path**: the `Expr::Call` branch matches the three base shapes inline (mirroring `lower_method_call`), calling `struct_base` for the value-receiver case (the review's point — `generic_callee_name` returns `&str` and cannot cleanly yield an owner needing `&mut self`). It may remain a name-only helper for the bare/qualified shapes.

**`MethodSig`.** Unchanged shape; generic-method sigs now set `is_instance` truthfully (from decl modifiers) and put `Ref(owner_id)` at `params[0]` for instance methods. `variadic` is computed from the decl's trailing `params T[]` exactly as non-generic methods, so the arity assert (§7) is variadic-aware.

## 5. Sema / parser / codegen changes

**Parser/AST:** no changes. `obj.M<T>(args)` already parses as `Expr::Call { callee: Expr::Generic { base: Expr::Member { base: obj, name: M }, args: targs }, args }`. (Confirm with a parser-corpus addition; no grammar work.)

### 5.1 Mangling + key + decl index + `cur_type` (Task A1)

Add `mangle_generic_method`, `GenMKey`, `GenMethodMono` struct, `cur_type` field threaded from `lower_type_at`'s `owner_id`. Re-key `gen_method_sigs`/`gen_method_monos`/`GenMethodDecls` **mechanically with `owner = None` everywhere** (pure, symbol-identical: `(None, name, codes)`; `GenMethodDecls` value becomes `Vec` but still only `None`-keyed). `index_generic_methods` still indexes under `(None, name)` only. **No owner determination yet** — A1 is a true no-op refactor. (The `cur_type` field is *added and populated* in A1 so later tasks have it, but not yet *read*.)

### 5.2 Owner determination — static/bare/qualified collection (Task A2)

- `index_generic_methods` resolves each enclosing `TypeDecl` to `Some(StructId)` and inserts **both** `(Some(owner), name)` and `(None, name)` entries (Vec-append).
- `record_method_inst` gains `owner: Option<StructId>`: pick the matching overload from the `Vec` by explicit-param arity; mangle with owner; key the sig by `(owner, name, codes)`; set `is_instance`/`variadic` from the decl; push `GenMethodMono`.
- Thread the enclosing `TypeDecl`'s `StructId` (and a `cur_owner` for the collector) through `collect_insts_type`/`collect_insts_expr` (resolve once via `by_name`):
  - `Ident`-base → try `Some(cur_owner)` then `None` (records under whichever resolves; for bare cross-class like `Map`, the `None` bucket matches `Functional`'s `(None, Map)` entry).
  - `Member`-base naming a **type** → `Some(by_name[Type])`.
- This kills the §107 first-writer clobber and fixes `generic_method_collision.bf`. `list_hof.bf` still resolves `Map`/`Filter`/`Fold` via the retained `None` bucket.

### 5.3 Collector local/field type scope + instance-receiver collection (Task A3a)

Add the minimal `locals` scope to the collector (§3.4): thread `locals: Vec<(String, IrType)>` through `collect_insts_stmt/_expr`, populated from method params, explicitly-typed `Stmt::Local`s, `this`, and `this`-fields. For a `Member`-base whose base is a **value** expression, resolve the receiver owner from this scope for the supported shapes (declared-typed local/param, `this`, simple `this`-field, `new T()`) and record with `owner = Some(recv_id)`. Any other value receiver → **diagnostic** (no silent skip). This is split from A3b because it is the riskiest new infrastructure and deserves its own acceptance test.

### 5.4 Instance call lowering + emission (Task A3b)

Rework the `Expr::Generic`-callee branch (`lower.rs:3769`) to mirror `lower_method_call`'s three paths, matching base shapes inline:
- **Bare `Ident`:** owner = `Some(cur_type)` then `None`; arity guard becomes `formal.len() == args.len()` where `formal = if sig.is_instance { &sig.params[1..] } else { &sig.params[..] }`; prepend `this` iff `sig.is_instance && this_slot.is_some()`.
- **`Member` naming a type:** owner = `by_name[Type]`; receiver-less.
- **`Member` value receiver:** `struct_base(recv)` → `(body_ptr, owner_id)`; mangle `Some(owner_id)`; if the `GenMKey` is absent → **diagnose** (inherited/generic-owner case); else prepend `body_ptr`, coerce remaining args to `sig.params[1..]` (variadic-aware), emit a **direct** call.
- **Hard assert** `call_args.len() == sig.params.len()` before `fb.call` (variadic-adjusted: assert against the post-pack count, mirroring `lower_method_call`).

Emission loop (`lower.rs:2036`) iterates `GenMethodMono`:
- Re-find the decl via `(owner, name)` (Vec → pick by arity).
- `this_ty = owner.filter(|_| sig.is_instance).map(IrType::Ref)`.
- Pass the **owner's** method table as `sigs` (`structs.methods[owner]`) for `Some(owner)` so non-generic sibling calls and `this.field` inside the body resolve; `&empty` for `None`. (A generic body calling **another generic** method routes through `gen_method_sigs`, not `sigs` — supported only when that inner call was collected with **concrete** type-args; abstract-arg inner generic calls are diagnosed, see §1/§6.)
- `this_ty = Some(Ref(owner))` makes `lower_method` (`:2524`) emit the leading `this` and spill it, so `this.field` / bare member calls in the generic body work.

**SSA dominance:** the receiver is evaluated once via `struct_base` in the current block before the call — identical to non-generic instance dispatch; no new hazard. Reuse `coerce` for every argument so operand types match `sig.params`.

### 5.5 Backend / AOT

No `newbf-llvm` changes. The backend lowers each `Function` by `name`; owner-mangled symbols are just strings. The sema→llvm dependency rule is untouched (all changes in `newbf-sema`; comptime's callback is unaffected).

## 6. Interactions

- **Vtables.** Generic instance methods are non-virtual (direct calls); never occupy `vslots`/`vimpls`. **Reject `virtual`/`override`+generic at collection** (diagnostic). `apply_vtables` (`lower.rs:515–543`) unaffected.
- **Comptime.** `[Comptime]`+generic is **rejected at collection** (diagnostic). The gen-method emission loop (`:2036`) does **not** push to `m.comptime`, so a `[Comptime]` generic monomorph would emit but never register for JIT folding — silent breakage. Rather than wiring `m.comptime.push(sym)` into that loop speculatively (no existing client), we fail loudly. (Mind the JIT FP-constant-pool limit if ever revisited.)
- **Monomorphization / generic-on-generic.** Falls out naturally **once B1 lands**: a generic *type* monomorph `List<int64>` has `prefixes[id]="List$i64."`, so an instance generic `Map<R>` on it mangles `List$i64.Map$R`, distinct per owner-monomorph × method-monomorph. v1 lands **concrete-owner** instance generics only; **owner-monomorph prefix lookup is never attempted during collection** (collection order is source order; a `List<int64>` mono may not be registered yet) — all generic-on-generic owner resolution is deferred to B1, after the full type-mono table exists.
- **Inheritance.** `apply_inheritance` composes only non-generic method tables. An inherited generic instance method (decl on base, receiver derived) resolves owner = derived → `(Some(derived), name)` miss → **diagnostic** (§5.4), never a dangling symbol. Deferred as a follow-on.
- **Value-struct receivers.** For an `IrType::Struct` receiver, `struct_base` returns a **stack place** (pointer-to-struct), and the ABI `params[0]` must be that pointer, exactly as non-generic instance methods on value structs. v1's instance tests use **class** (`Ref`) receivers; a value-struct generic method, if encountered, takes `params[0]` = the struct pointer (not `Ref`) — handled by mirroring the non-generic value-struct path; otherwise diagnosed.
- **Target-typing.** Generic calls flow through the normal `expr` path; a target type wraps the result via the existing `try_target_typed_*` chain (`lower.rs:2917`). The call returns `sig.ret`, coerced by the caller. No change.
- **Null receiver.** `obj.M<T>(x)` with null `obj` is a runtime null-deref through `body_ptr` — identical exposure to non-generic instance calls; not null-guarded (only `?.` is). Consistent, noted.
- **The other three features.** *fn-values*: a method-ref to `obj.Map<R>` resolves to the owner-mangled symbol — owner-mangling is the prerequisite. *itables*: generic interface methods need per-(class,interface) symbols; owner-mangling provides uniqueness, itable layout is a separate follow-on. *targeted-args*: argument-position generic-method refs resolve a unique signature via the composite key. None block v1.

## 7. Risks & mitigations

- **Collection/lowering owner skew (now the central correctness property).** The lazy net is gone; correctness rests on collection and lowering resolving the **same** owner for every emitted symbol. *Mitigation:* both passes use the identical owner rule — `cur_type` for bare (not `this_slot`), `by_name[Type]` for qualified, and the **same restricted set of `struct_base`-resolvable receiver shapes** for instance (declared-typed local/param/`this`/`this`-field/`new T()`). Every other receiver is **diagnosed at collection**, so it can never reach lowering and miss. Lowering additionally **hard-asserts** key presence and emits a diagnostic on miss — never a dangling `call`.
- **LLVM verifier "instruction does not dominate all uses".** The instance receiver must be evaluated in, and dominate, the call's block. *Mitigation:* reuse `struct_base` (single eval, current block) and `lower_method_call`'s eager arg-eval order; never hoist across a branch. Covered by the verify corpus.
- **ABI mismatch (missing/extra `this`).** *Mitigation:* `is_instance` is the single source of truth, set once at collection from decl modifiers, consumed identically at call (prepend `body_ptr` iff `is_instance`, `pidx` from 1) and emission (`this_ty`). **Hard assert** `call_args.len() == sig.params.len()` (variadic-adjusted) before emit; negative tests for inherited/overloaded generics so the failure is a clean diagnostic.
- **Bare cross-class static regression (`list_hof.bf`).** *Mitigation:* the retained `None` bucket; A1/A2 acceptance explicitly include `list_hof.bf` → 18.
- **Overloaded generic methods in one owner.** *Mitigation:* `GenMethodDecls` value is a `Vec`; resolution picks by explicit-param arity (mirrors `pick_overload`).
- **Symbol churn breaking nothing visible.** Existing symbols change (`Identity$i32` → `Program.Identity$i32`, `Pick$i32` → `Util.Pick$i32`). *Verified empirically:* a workspace grep for literal generic-method symbols (`Identity$`, `Pick$`, `Map$`, `Filter$`, `Fold$`, `First$`, `$i32`, `$i64`) finds **only `lower.rs` source code** — no test, tool, or `.ll` fixture references them. Run-corpus checks return values, which are invariant.
- **sema must not depend on llvm.** All work in `newbf-sema`; no new llvm coupling. *Verified by the workspace build.*

## 8. Testing strategy

Gates (100% ratchets — adding a feature-suite file raises the denominator and the gate still demands all-clean):
- **LLVM verify corpus** (`newbf-sema/tests/corpus.rs`, `clean == files.len()`): dominance/type/ABI errors in lowered generic methods.
- **Parser corpus** (`newbf-parser/tests/corpus.rs`): confirms `obj.M<T>(args)` parses.
- **Run corpus** (`tests/newbf-tests/tests/run_corpus.rs`): behavioral gate (JIT, full i32).

New run-corpus programs (each `Program.Main` → int32, `// expect:`):

1. `generic_method_collision.bf` — **expect: 42.** Two classes each `static T Get<T>(T x)`; `A.Get<int32>(40) + B.Get<int32>(2)`. Fails today; passes with owner-mangling (A2).
2. `generic_method_instance.bf` — **expect: 42.** `class Box { public T Get<T>(T x){return x;} } … b.Get<int32>(40)+b.Get<int32>(2)`. Local-receiver instance path (A3b).
3. `generic_method_instance_this.bf` — **expect: 7.** Class with `int32 mV` and `public T Wrap<T>(T x){return x;}` plus a non-generic method calling `this.Wrap<int32>(this.mV)`. Same-class instance + `this` in a generic body.
4. `generic_method_two_owners_instance.bf` — **expect: 6.** Two classes with same-named instance `Id<T>`; call both on distinct receivers, sum. Collision + instance.
5. `generic_method_field_receiver.bf` — **expect: N.** A class holding `Box mBox;` and calling `this.mBox.Get<int32>(x)` — exercises the **field-receiver** branch of collector resolution + `lvalue(Member)`→`struct_base` (distinct from local/`this`/`new`).
6. `generic_method_static_unchanged.bf` — mirror of `generic_method_qualified.bf`, **expect: 49.** Guards the static qualified path.

**Negative / known-limitation acceptance** (must be a clean diagnostic, never dangling symbol or garbage):
- inherited generic instance method (decl on base, receiver derived);
- `virtual`/`override`+generic;
- `[Comptime]`+generic;
- instance generic call on an unresolvable receiver (inferred `var` local, call-return receiver).

**Regression guards** (must stay green at every task boundary): `generic_method.bf` → 12, `generic_method_qualified.bf` → 49, **`list_hof.bf` → 18**.

Add an instance-generic-method `.bf` to `beef-tests/feature-suite/src/` so the verify+parser corpora re-walk it (new file must verify clean; the ratchet handles the count bump).

## 9. Task breakdown (ordered)

**A1 — Mechanical re-key (no-op) + `cur_type` plumbing.**
Scope: `lower.rs` — add `mangle_generic_method`, `GenMKey`, `GenMethodMono` struct; swap `gen_method_sigs`/`gen_method_monos` to the keyed forms with **`owner = None` hardcoded everywhere**; change `GenMethodDecls` value to `Vec` (still `(None, name)`-keyed); add `Lowerer.cur_type` and thread `owner_id` from `lower_type_at` → `lower_method` → `Lowerer::new`, **populated but not yet read**. Deps: none. Accept: all three corpora green; `generic_method.bf`→12, `generic_method_qualified.bf`→49, **`list_hof.bf`→18** unchanged; symbols **identical** to today (verified by the symbol grep returning only `lower.rs`).

**A2 — Owner determination: static/bare/qualified + collision fix.**
Scope: `lower.rs` — `index_generic_methods` resolves enclosing `TypeDecl`→`StructId`, inserts both `(Some(owner),name)` and `(None,name)` Vec entries; `record_method_inst` gains `owner`, picks overload by arity, mangles/keys with owner, sets `is_instance`/`variadic`; thread `cur_owner` through `collect_insts_*`; bare → `Some(cur)` else `None`, qualified → `by_name[Type]`; call site reads `cur_type` for bare owner with `None` fallback. Deps: A1. Accept: corpora green; `generic_method_collision.bf`→42; **`list_hof.bf`→18 (retained `None` bucket)**; `generic_method.bf`→12, `generic_method_qualified.bf`→49 with now owner-qualified symbols.

**A3a — Collector local/field type scope + instance-receiver resolution.**
Scope: `lower.rs` — add `locals: Vec<(String,IrType)>` to `collect_insts_stmt/_expr` (from params, typed `Stmt::Local`s, `this`, `this`-fields); resolve value-receiver owners for declared-typed local/param/`this`/`this`-field/`new T()`; **diagnose** any other value receiver. Deps: A2. Accept: corpora green; a collection unit/snapshot test that the supported shapes record `Some(owner)` and an unsupported receiver emits the diagnostic (no silent skip).

**A3b — Instance generic-method dispatch (call + emission).**
Scope: `lower.rs` — rework the `Expr::Generic`-callee branch to match base shapes inline, call `struct_base` for value receivers, mangle `Some(owner_id)`, prepend `body_ptr`, is_instance-aware arity guard, **hard assert** `call_args.len()==sig.params.len()`, **diagnose** absent keys; emission loop sets `this_ty=Some(Ref(owner))` and passes `structs.methods[owner]` as `sigs` for instance monos. Add the feature-suite `.bf`. Deps: A3a. Accept: corpora green (incl. the new feature-suite file verifying clean); `generic_method_instance.bf`→42, `generic_method_instance_this.bf`→7, `generic_method_two_owners_instance.bf`→6, `generic_method_field_receiver.bf`→N; inherited/`virtual`/`[Comptime]`/unresolvable-receiver negatives all produce clean diagnostics.

**A4 — Guards & negatives hardening.**
Scope: `lower.rs` — implement the rejection diagnostics as real sema diagnostics (not debug asserts): `virtual`/`override`+generic, `[Comptime]`+generic, inherited generic instance method (owner-miss walk → diagnose), abstract-type-arg inner generic call. Deps: A3b. Accept: corpora green; each negative case is a clean diagnostic in the corpus harness; no dangling symbols, no garbage values.

**A5a — Corlib comment update (no behavior change).**
Scope: `newbf-corlib/bf/List.bf` — update the `Functional.Map/Filter/Fold` comment to note concrete-owner instance generics now work and that generic-owner instance methods await B1. Deps: A3b. Accept: corlib slice still verifies clean; no run-corpus value change. *(Honest: this is comment-only; the substantive migration is A5b/B1.)*

**B1 — Generic methods on generic owners (the corlib HOF enabler).**
Scope: `lower.rs` — emit instance generic-method monomorphs whose owner is itself a *type* monomorph (`List<int64>.Map<R>`): when lowering a type mono's members (`:2023`/`:2026`), detect generic methods and emit their monomorphs at the mono's id/prefix with the **combined env** (owner `T` ++ method `R`); resolve owner-mono prefixes only **after** the full type-mono table exists. Deps: A3b, A4. Accept: corpora green; `generic_method_on_generic.bf` (concrete expect value) passes.

**B2 — Corlib `List<T>` HOF migration (the marquee payoff).**
Scope: `newbf-corlib/bf/List.bf` — move `Map`/`Filter`/`Fold` onto `List<T>` as instance generic methods; update `list_hof.bf` (or add `list_hof_instance.bf`) to call `xs.Map<R>(f)`. Deps: B1. Accept: corpora green; the instance-syntax HOF program returns the expected value; corlib slice verifies clean.

Each A-task is independently landable behind all three gates. B1+B2 are the larger staged extension that delivers the `obj.Map<R>(f)` payoff on a real generic type.

## 10. Open questions / decisions deferred

- **Generic interface methods.** How an interface's generic method maps to a class's implementation symbol (needs itable design). Deferred; owner-mangling is the prerequisite, not the blocker.
- **`virtual` generic methods.** Rejected for v1; would need per-(name,codes) itable slots — separate design.
- **Free/namespace-level generic methods (`None` owner).** Supported by the retained `None` bucket; currently the bare-cross-class-static carrier. Revisit when namespace-scoped functions land.
- **Richer receiver-type inference.** v1's collector resolves a fixed set of shapes and diagnoses the rest. If full local-type propagation lands later, the diagnosed cases (inferred `var`, call-return receivers) can be promoted to supported — but only if collection stays authoritative (no lazy pass) or a real RefCell+drain fixpoint is designed first.
- **Base-class receiver dispatch for generic instance methods.** Deferred (diagnosed in v1). Would require composing generic-method tables across inheritance in `apply_inheritance`. Call out if a corlib case needs it.
- **Transitive/recursive generic instantiation with abstract type-args.** `M<U>` inside `M<T>` (U abstract) is the monomorphization-closure problem; v1 diagnoses it. A worklist-based transitive collector is a separate proposal.
