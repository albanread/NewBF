# Iterators — `foreach` over user types + a restricted `yield`

## 1. Overview

NewBF's `foreach` today is **structural duck-typing on hard-coded method names**: the
single `Stmt::ForEach` lowering arm (`newbf-sema/src/lower.rs:6924-7105`) special-cases a
numeric range, a heap-array local, and a List-like receiver probed for `Count() -> int` +
`Get(int) -> T` by literal name (`lower.rs:7042-7050`); anything else falls through to a
silently **skipped body** (the `if let Some(...) = sigs` block at `lower.rs:7055-7104` has no
`else`). There is **no `GetEnumerator()` call, no `IEnumerator`, no `MoveNext`/`Current`/
`Dispose`** anywhere in the path, and `yield` — though lexed as a keyword
(`newbf-lexer/src/token.rs:121`) — has **no parser arm** in `stmt()`
(`newbf-parser/src/parser.rs:1339-1357`), so `yield x` silently parses as a bogus
expression statement.

**v1 capability (one paragraph).** Add a **fifth `ForEach` branch** that drives a loop
through the classic enumerator protocol resolved **statically by name on the concrete
enumerator struct**: `recv.GetEnumerator()` returns an enumerator `e : Struct(eid)` (value)
or `Ref(eid)` (heap); the loop heads on `e.MoveNext() -> bool`, binds the loop variable from
`e.Current` (the `get_Current` property symbol), and calls `e.Dispose()` on **every** exit
edge (normal fall-off, `break`, **and `return`-through**) via the existing scope-cleanup
machinery. All members resolve through the same `pick_overload` probe the Count/Get path
already uses (`lower.rs:7043-7050`) — **no `IEnumerator<T>` interface type is needed**
(generic interfaces stay unsupported, §6). A corlib **top-level generic** `struct
ListEnumerator<T>` + `List<T>.GetEnumerator()` ships as the first real user. For
**generators**, v1 takes the **eager-materialization** path: a method body containing
`yield return` / `yield break` is rewritten — by **re-emitting the method body as owned
source text and re-parsing it with a fresh `FileId`** (the comptime emission precedent,
`newbf-comptime/src/emit.rs:429-432`, **not** an in-place AST mutation — §3.4) — into a body
that allocates a `List<T>`, turns each `yield return e` into `__yield.Add(e)`, each `yield
break` into `return __yield`, and returns the list. The method then becomes an ordinary
`List<T>`-returning method that `foreach` already iterates. The lazy coroutine state machine
is **explicitly deferred** (§5).

This document is modeled on `docs/design/itables.md` and `docs/design/mixins.md` (the two
wave-2/3 designs whose rigor and task structure this mirrors). Every load-bearing claim is
anchored to a re-verified `file:line`.

## 2. Representation / ABI / IR changes

### 2.1 No new IR instruction, no new `IrType`, no ABI change

The GetEnumerator loop reuses exactly the IR the Count/Get path already emits:
`create_block`, `alloca`, `store`/`load`, `cmp`, `cond_br`/`br`, and direct `call`
(`lower.rs:7066-7101`). `IrType` stays `Copy` (`newbf-ir/src/ty.rs`); the enumerator value
is an ordinary `Ref(eid)` (heap class) or `Struct(eid)` (value struct). No new instruction
is added to `newbf-ir`. **newbf-llvm needs zero changes.** Eager `yield` materialization is
a **source-text rewrite + re-parse in `newbf-sema`** that produces ordinary `List<T>`-using
AST, lowered by the existing path — likewise no new IR and no llvm change.

### 2.2 The sema ⊥ llvm contract (what sema emits by-name)

The HARD INVARIANT (sema must not depend on newbf-llvm; they agree via the IR contract +
named symbols) is preserved trivially: every new call is a **direct `self.fb.call(symbol,
args, ret)`** against a method symbol sema itself mangled, identical to how the Count/Get
path calls `count_sig.full_name` / `get_sig.full_name` (`lower.rs:7076,7088`). The symbols
are:

| Member | Resolution (each probe ends in `.cloned()` — see note) | Symbol source |
|---|---|---|
| `GetEnumerator()` | `methods[recv_id].get("GetEnumerator").and_then(\|c\| pick_overload(c, &[], true)).cloned()` | `MethodSig.full_name` (`lower.rs:5400-5419`) |
| `MoveNext() -> bool` | `methods[eid].get("MoveNext")…pick_overload(c, &[], true).cloned()` | `MethodSig.full_name` |
| `Current` (property) | `methods[eid].get("get_Current")…pick_overload(c, &[], true).cloned()` | property getter symbol `get_Current` (the established convention — `try_property_get` resolves `get_{name}`, `lower.rs:9397`) |
| `Dispose()` | `methods[eid].get("Dispose")…pick_overload(c, &[], true).cloned()` (OPTIONAL) | `MethodSig.full_name` |

**`.cloned()` is mandatory, not optional.** `pick_overload` (`lower.rs:5561-5573`) returns
`Option<&MethodSig>` borrowing `self.structs.methods`; you cannot then call `self.fb.call`
(needs `&mut self`) while that borrow is live. Every probe **must** `.cloned()` to end the
borrow before emission — exactly as the live Count/Get path does at `lower.rs:7046,7050`.
`MethodSig` is the owned struct at `lower.rs:5400-5419` carrying `full_name`, `ret`,
`params` (this-leading for instance methods), `is_instance`, `variadic`, `param_fn_sigs`.
The **element type** of the loop var is `current_sig.ret` (mirroring the Count/Get path
taking `elem_ty = get_sig.ret` at `lower.rs:7057`).

### 2.3 corlib types — a TOP-LEVEL generic enumerator (not a nested type)

The enumerator **must be a top-level generic** `struct ListEnumerator<T>`, **not** a nested
`struct Enumerator<T>` inside `List<T>`. **Why this is forced, not stylistic:**
`index_generic_decls` (`lower.rs:655-692`) iterates only `Item::Type` and `Item::Namespace`
bodies — it has **zero `Member` references** and never descends into a type's members. A
generic type declared as a `Member::Nested` is therefore **never inserted** into the
`(name, arity)` `generics` map, so `record_inst` (`lower.rs:1685`) hits its `else { return; }`
and the type is **never monomorphized** — it would lower to the `Ptr` fallback, and
`GetEnumerator`'s `match ge.ret { Struct(e)|Ref(e) => e, _ => bail }` (§3.1) would bail.
(Note: `register_type_struct` *does* recurse into `Member::Nested`, `lower.rs:2544-2546` —
but only for the **non-generic** `by_name` registration; the generic-monomorph index is a
separate, member-blind walk. Two different walks, only one of which descends.) Hoisting to a
top-level `struct ListEnumerator<T>` puts it on the indexed, monomorphizable path that
`List<int32>` itself already travels.

Additions to `newbf-corlib/bf/List.bf` (`Count`/`Get` at `List.bf:21-22`):

```beef
// A by-value enumerator over a List<T>: an index cursor + a borrowed buffer
// pointer. Top-level generic (NOT nested in List) so index_generic_decls
// collects it (lower.rs:655-692 is member-blind). Returned by value, so foreach
// copies it into an alloca; no heap allocation. The empty Dispose keeps the
// protocol uniform and lets the scope-dispose path be exercised once.
struct ListEnumerator<T> {
    T* mItems;
    int mCount;
    int mIndex;       // -1 before the first MoveNext

    public bool MoveNext() {
        this.mIndex = this.mIndex + 1;
        return this.mIndex < this.mCount;
    }
    public T Current { get { return this.mItems[this.mIndex]; } }
    public void Dispose() { }
}

// On List<T>:
public ListEnumerator<T> GetEnumerator() {
    ListEnumerator<T> e;
    e.mItems = this.mItems;
    e.mCount = this.mCount;
    e.mIndex = -1;
    return e;
}
```

**Note (List itself keeps the Count/Get fast path).** Because the new branch is ordered
**after** the Count/Get probe (§3, to preserve the five pinned run-corpus tests), the corlib
`List<T>` keeps iterating via `Count()`/`Get(int)` — `foreach_list.bf → 60` is unchanged.
`ListEnumerator<T>` is shipped so a *follow-on* can flip List to its enumerator without
touching the loop lowering, and so a corpus program (§4 example 0) can prove the
generic-value-struct ABI directly. `IEnumerator<T>` / `IEnumerable<T>` interface types are
**not** added in v1 (§6).

**This is a first-of-kind executable path (honest risk).** The runnable corlib
(`newbf-corlib/bf/`) today has **zero generic value structs** (verified: only generic
*classes* `List<T>`, `Handle<T>` exist there; the generic value structs in
`beef-tests/corlib-slice/` + `feature-suite/` are **parse+def-build only**, never
JIT-executed). So a monomorphized generic value struct with state-mutating instance methods,
returned by value and copied into an alloca under the Stomp guard, has never run on the
run-corpus. §4 example 0 (`enum_manual.bf`) proves this ABI in isolation **before** T1
layers the loop on top.

### 2.4 No metadata, no mangling change

The enumerator is a normal monomorphized generic struct; `ListEnumerator<int32>` mangles
under the standard mono prefix exactly like `List<int32>` and `List<int32>.Map<int32>` do
today (`List.bf:140-141` documents `@List$i32.Map$i32` for the Map *method*; a top-level
generic *type* mangles the same standard way). `List<int32>.GetEnumerator()` is an ordinary
instance method on the registered `List<int32>` owner, so it mangles like any other List
method. No new monomorph key, no new symbol namespace. The eager-`yield` rewrite produces a
method whose return type is a `List<T>` instantiation already collected by the existing
monomorph walk (§3.4).

## 3. Concrete changes (sema + parser + llvm + runtime), with seams

### 3.1 The fifth `ForEach` branch (sema, `lower.rs`)

Insert a branch into the `Stmt::ForEach` arm at `lower.rs:6924`, **after** the Count/Get
probe block (`lower.rs:7041-7104`), reachable when that probe yields `None` (the
`if let Some(...) = sigs` at `7055` has no `else`, and `coll`/`coll_ty` from `lower.rs:7041`
remain in scope, so the new code reuses the already-evaluated receiver). The shape mirrors
the Count/Get loop:

```text
// `coll`/`coll_ty` are already evaluated once at lower.rs:7041, BEFORE the
// Ref-only Count/Get probe. They remain valid here on the sigs == None path.
if let IrType::Ref(id) | IrType::Struct(id) = coll_ty {
    let ge = methods[id].get("GetEnumerator")
        .and_then(|c| pick_overload(c, &[], true)).cloned();      // .cloned() mandatory
    if let Some(ge) = ge {
        let eid = match ge.ret { Struct(e) | Ref(e) => e, _ => /* fall through to skip */ };
        let mn   = methods[eid].get("MoveNext")  …pick_overload(c, &[], true).cloned();
        let cur  = methods[eid].get("get_Current")…pick_overload(c, &[], true).cloned();
        let disp = methods[eid].get("Dispose")   …pick_overload(c, &[], true).cloned(); // optional
        if let (Some(mn), Some(cur)) = (mn, cur) {
            // form the GetEnumerator `this` receiver (§3.1.1), emit the loop (below)
            return;
        }
    }
}
// else: existing skipped-body fall-through (unchanged)
```

The loop body (one basic-block-per-edge skeleton, copied from the Count/Get arm
`lower.rs:7058-7103`):

1. `self.scopes.push(HashMap::new())` — a fresh scope frame for the loop var (matches
   `lower.rs:7058`).
2. **Form the `GetEnumerator` receiver `this` pointer (§3.1.1), then**
   `let e_slot = self.fb.alloca(enum_ty); store call(ge.full_name, [recv_this], enum_ty)` —
   evaluate the enumerator once into its own alloca (analogous to `coll_slot`,
   `lower.rs:7060-7061`). For a value-struct enumerator (`enum_ty = Struct(eid)`), `e_slot`'s
   address is the body pointer for all subsequent calls.
3. `let var_slot = self.fb.alloca(elem_ty); self.bind(name.text(src), var_slot, elem_ty,
   None)` — `elem_ty = cur.ret`. `bind` is `lower.rs:6554`; the 3-tuple `(slot, ty, elem)`
   matches every other branch.
4. **Register `Dispose` as a scope-cleanup hook** (§3.2) so `return`-through runs it.
5. Blocks `head`/`body`/`cont`/`exit` via `create_block` (matches `lower.rs:7066-7069`).
6. **head:** `let this_e = <e_slot address for a value struct | loaded ptr for a Ref>;
   let cond = call(mn.full_name, [this_e], bool); cond_br(cond, body, exit)`.
7. `self.terminated = true; self.switch(body)` (matches `lower.rs:7081-7083`).
8. **body:** `name = e.Current` → `call(cur.full_name, [this_e], elem_ty)` then `store
   var_slot`; then `self.loops.push((cont, exit, self.scope_allocs.len()))`,
   `self.stmt(body, src)`, `self.loops.pop()` (matches `lower.rs:7090-7092`). The
   `loops.push` triple is exactly the form at `lower.rs:7090` so `break`/`continue` and
   their `free_scopes_down_to(depth)` (`lower.rs:7112,7122`) work unchanged. **`this_e` for
   `Current` is the SAME `e_slot` body pointer as MoveNext's** — never a reload of a copy —
   so the mutation MoveNext made is what Current reads (see §3.1.1).
9. **cont → head:** unconditional `br head` (no index increment; MoveNext advances).
10. **exit:** `self.switch(exit)`; the registered scope-cleanup hook (step 4) emits
    `Dispose()` here on the normal/`break` fall-off; then `self.scopes.pop()`.

#### 3.1.1 Forming the `this` pointer — hand-built calls on a place, NOT a synthetic receiver

There are **two** receivers to form, both value-struct-sensitive:

**(a) `GetEnumerator`'s receiver (the collection).** The Count/Get path fires only for
`IrType::Ref(id)` (`lower.rs:7042`), where `coll` *is* the heap pointer and is passed
directly as `this`. The new branch also fires for the `IrType::Struct(id)` case the Count/Get
path rejects — there `coll` is the **struct value**, but `GetEnumerator` is an instance
method needing a **pointer** `this` (`lower.rs:11248` always pushes `body_ptr`). So:
- `Ref(id)` receiver → pass `coll` (the loaded pointer) directly as `GetEnumerator`'s `this`.
- `Struct(id)` receiver → `alloca enum_owner_ty`, `store coll`, pass the **alloca address**
  as `this`. This also covers an **rvalue** value-struct receiver (`foreach (x in MakeBag())`):
  `struct_base`'s non-lvalue arm only returns `Some` for a `Ref` rvalue (`lower.rs:9595-9605`,
  returns `None` for a value-struct rvalue), so the materialize-into-alloca step is required —
  `coll` is already the evaluated value, just store it.

**(b) The enumerator's own receiver (`MoveNext`/`Current`/`Dispose`).** For a **value-struct**
enumerator these methods mutate `mIndex`, so `this` MUST be the **pointer to the `e_slot`
alloca**, reused identically across all three calls and all iterations — never a reloaded
copy (a reloaded copy discards MoveNext's increment, and Current would read element 0
forever). For a `Ref(eid)` enumerator the loaded pointer is the body and mutation is in-place.

**Decision (corrected from the prior draft): hand-build the three calls directly; do NOT
route through `lower_method_call`.** `lower_method_call` (`lower.rs:11087`) recovers its
receiver from an **AST node** via `struct_base` → `lvalue(base)` → `self.lookup(s.text(src))`,
and identifiers in this AST are `Span`s whose `Span::text(src)` slices into real source
(`token.rs:33`, `&src[lo..hi]`). There is **no facility anywhere** to feed it a pre-lowered
`Value` as `this`, and a synthetic enumerator name that is not a substring of the user's file
cannot be expressed as a `Span` (no precedent in sema binds a local under a non-source name).
The proven precedent is **direct emit**: the auto-property getter builds its getter body with
`FunctionBuilder` and a `Ref(oid)` `this` param, emitting `field_addr`/`load`/`ret` directly
(`lower.rs:6036-6047`), and `try_property_get` builds the getter call by pushing `body_ptr`
as `this` and calling `getter.full_name` (`lower.rs:9395-9405`) — neither fabricates an
`Expr`. The branch therefore emits:

```text
// value-struct enumerator: e_addr = e_slot (the alloca address; Struct(eid) lvalue,
//   the body-pointer arm of struct_base at lower.rs:9583)
// Ref enumerator: e_addr = self.fb.load(e_slot, IrType::Ptr) once per use as needed
let cond = self.fb.call(mn.full_name.clone(),  vec![e_addr.clone()], IrType::Bool);
let cur  = self.fb.call(cur.full_name.clone(), vec![e_addr.clone()], elem_ty);
// dispose via the scope hook (§3.2)
```

**T1 SHOULD add a small shared helper** `call_instance_on_ptr(&mut self, sig: &MethodSig,
this_ptr: Value, args: Vec<(Value, IrType)>) -> Value` that prepends `this_ptr` (when
`sig.is_instance`) and coerces the explicit args against `sig.params[1..]` — the resolution/
coercion tail of `lower_method_call` (`lower.rs:11246-11260`) factored to take a `Value`
receiver. This keeps the three calls uniform with ordinary instance calls without pretending
the `&Expr`-keyed entry point can be fed a manufactured Ident. The acceptance test
`foreach_getenumerator.bf` (value-struct enumerator that mutates `mIndex`) pins that
MoveNext's state persists across iterations and that Current reads through the same slot.

### 3.2 Memory ownership of the enumerator (scope / Dispose) — exactly-once on EVERY exit edge

The prior draft claimed "Dispose on every exit edge" but emitted it only at the top of the
`exit` block. **That is wrong for `return`:** `Stmt::Return` lowering
(`lower.rs:6798-6800`) runs `run_all_defers` + `free_all_scopes` then `ret` — it does **not**
branch through the loop's `exit` block. A `foreach` body containing `return` would run the
`exit`-block Dispose **zero** times. `break`, by contrast, *does* branch to the registered
`exit` block (`lower.rs:7113`, `brk` = the `exit` pushed at `loops.push`,
`lower.rs:7090`), so an `exit`-top Dispose covers the break edge but not the return edge.

**v1 fix — register Dispose as a scope-cleanup hook, not a hand-placed `exit`-block call.**
Use the same `scope_allocs` frame + `free_scopes_down_to`/`free_all_scopes` machinery
(`lower.rs:6535-6553`) that gives `scope`/`new` locals their exactly-once cleanup
(`ScopeAlloc`, `lower.rs:7852-7876`). Register the enumerator's `Dispose()` call as a
cleanup hook in the loop's scope frame so:
- **normal fall-off** runs it when the loop's scope frame is popped at `exit`,
- **`break`** runs it via `free_scopes_down_to(depth)` (`lower.rs:7112`) before branching to
  `exit` (and the `exit`-block cleanup must then **not** double-emit — the hook lives in the
  frame the break already freed; do not also emit at `exit` top for the same frame),
- **`return`-through** runs it via `free_all_scopes` (`lower.rs:6799`),
- **`continue`** branches to `cont`→`head`→(false)→`exit` and does **not** double-Dispose
  (its `free_scopes_down_to(depth)`, `lower.rs:7122`, frees only frames *inside* the loop
  body, not the loop's own enumerator frame).

This is the §5-deferred "wire the enumerator into a `ScopeAlloc`" work, but **done now for
the Dispose call specifically** (a no-op-bodied call for the value enumerator), because
exactly-once-Dispose is a named top-3 risk and the Stomp guard would **not** catch a *missed*
Dispose (it's a skipped call, not a double-free). `foreach_dispose_once.bf` (§4) and
`foreach_dispose_return.bf` (§4) pin both the break and the return edges.

Three enumerator-storage cases:

- **Value-struct enumerator (v1 default, `ListEnumerator<T>`).** Lives in `e_slot`; no heap
  allocation, nothing to free. `Dispose()` is still called (no-op for the corlib enumerator)
  on every exit edge via the hook, for protocol uniformity and to exercise the path.
- **Heap (`Ref`) enumerator the loop constructed.** v1 still calls `Dispose()` on every exit
  edge via the hook, but does **not** auto-`delete` the heap body (documented limitation,
  §5; mirrors how itables.md §6 leaves `delete`-through-interface-values to current
  behavior). The corlib enumerator is value-typed precisely to keep v1 guard-clean. Full
  enumerator-ownership (`Dispose`+`delete` exactly-once) is deferred (§5).
- **`Dispose` is optional.** If the enumerator struct has no `Dispose`, the probe yields
  `None`, no hook is registered, and no dispose call is emitted. Only `MoveNext` + `Current`
  are required.

### 3.3 Parser: `yield return` / `yield break` (parser, `parser.rs`)

`yield` is already a `Keyword` (`token.rs:121`) but `stmt()` (`parser.rs:1339-1357`) has no
arm for it. Add a `TokenKind::Keyword(Keyword::Yield)` arm to `stmt()` and two AST variants:

```rust
// ast.rs, alongside ForEach (ast.rs:485-490):
/// `yield return expr;`
YieldReturn { span: Span, value: Expr },
/// `yield break;`
YieldBreak { span: Span },
```

The parser arm: `bump()` the `yield`; if the next token is `Keyword::Break`, emit
`YieldBreak`; if `Keyword::Return`, `bump()` then parse an expression → `YieldReturn`.
This is a behavior-preserving AST enrichment (today `yield x` mis-parses; **no corpus program
uses a `yield return`/`yield break` statement** — the ~14 `yield` substring hits across the
tree are all in **comments/prose**, not statements, so nothing regresses).

**Walker audit — the "forces an arm" safety net is REAL for only two walks; the rest are
wildcard-terminated and a missed edit miscompiles SILENTLY.** Verified per-walker:

| Walk | Location | Terminator | Adding a variant… |
|---|---|---|---|
| `Stmt::span()` | `ast.rs:547-567` | **exhaustive, no `_`** | **forces an arm (compiler error until added)** |
| `print.rs::stmt` | `print.rs:761-890` | **exhaustive, no `_`** | **forces an arm (compiler error until added)** |
| `collect_insts_stmt` | `lower.rs:2190` | `_ => {}` | **silently skipped — must hand-edit** |
| `for_each_stmt_expr` | `lower.rs:3727` | `_ => {}` | **silently skipped — must hand-edit** |
| `collect_lambdas_stmt` | `lower.rs:3616` region | `_ => {}` | silently skipped — hand-edit |
| `collect_local_fns_stmt` | `lower.rs:3912` region | `_ => {}` | silently skipped — hand-edit |
| `collect_mixins_stmt` | `lower.rs:4063` region | `_ => {}` | silently skipped — hand-edit |
| `caps_stmt` | `lower.rs:7391` region | `_ => {}` | silently skipped — hand-edit |
| `register_tuples_in_stmt` | `lower.rs:951` region | `_ => {}` | silently skipped — hand-edit |
| lowering `stmt` | `lower.rs:6924` region | (large match) | **diagnostic arm (§3.3 below)** |
| ownership.rs flow-scan | `ownership.rs:456` | `_ => true` | silently skipped — hand-edit (see §3.4 ordering) |
| ownership.rs drop-lambda | `ownership.rs:812` | `_ => {}` | silently skipped — hand-edit |

So **T2 must hand-edit every wildcard walker** that needs to descend into `YieldReturn`'s
contained `Expr` (at minimum `for_each_stmt_expr`, `collect_insts_stmt`,
`collect_lambdas_stmt`, `register_tuples_in_stmt`, `collect_mixins_stmt` — a `yield return`
whose expr contains `new List<…>()`, a generic call, or a lambda must be seen by the
monomorph/tuple/lambda collectors *if* the generator rewrite has not yet consumed it). The
correct arms are `Stmt::YieldReturn { value } => f(value)` (or the recursive equivalent) and
`Stmt::YieldBreak => {}`. **Because the compiler will NOT flag a missed wildcard walker, T2
ships a focused test** (a `yield return (x => x)` / `yield return new List<int32>()` fixture)
that proves the lambda/mono collectors saw the yielded expression, AND **T2 lands together
with T3** (the variants are inert without the rewrite anyway — §7), so no green boundary ever
has a parsed `yield` reaching lowering un-rewritten.

**The lowering `stmt` arm is a DIAGNOSTIC, never `unreachable!`.** After T3, a `yield` only
survives to lowering if it appears in a method whose return type is not `List<E>` (the v1
generator precondition, §3.4) — a *user error*, not an internal invariant. Emitting
`unreachable!` there would turn user input into a compiler panic and fail the "no panics"
verify gate. The arm emits a diagnostic ("`yield` outside a `List<E>`-returning generator")
from the start.

### 3.4 Generator rewrite: eager materialization (sema, source-text re-emit + re-parse)

A method is a **generator** iff its body contains a `YieldReturn`/`YieldBreak`
(syntactically; detected by a `for_each_stmt_expr`-style recursive walk, `lower.rs:3718` —
which T2 teaches to descend into the yield variants). The element type `E` is the method's
declared return-element type: **v1 requires a generator to declare `List<E>` as its return
type** (no inference) so `E` is known syntactically.

**The rewrite is NOT a pure AST→AST mutation, and the prior draft's claim that it is "the
same approach mixins use" was wrong on two counts (corrected):**

1. **Identifiers in this AST cannot be fabricated.** `Expr::Ident(Span)` (`ast.rs:218`),
   `Stmt::Local.name: Span` (`ast.rs:443`), and type paths all carry raw `Span`s that slice
   into source (`token.rs:33`). Synthesizing `__yield`, `Add`, `new List<E>()`, `return
   __yield` requires `Span`s whose `.text(src)` equals those strings — **none guaranteed to
   exist as substrings of the user's file.** There is no synthetic-name side table anywhere.
2. **The AST is borrowed immutably at lower time.** `lower_program(files: &[SourceFile<'_>])`
   builds `all: Vec<SourceFile>` holding `unit: f.unit` borrows (`lower.rs:5073-5080`) of
   immutable parsed AST. There is no `&mut` method body to rewrite in place.
3. **Mixins are not the precedent.** Mixin expansion splices at **lowering time inside
   `expr`** (`expand_mixin`), reusing the live `Lowerer` scope and resolving body free-names
   via `self.lookup` against **already-bound** names, using the mixin body's **original
   declaring-file spans**. It never runs as a pre-`collect_insts` AST rewrite and never
   invents a new identifier.

**The only working precedent is the comptime emission path.** `newbf-comptime/src/emit.rs:
429-432` builds new code as a **`format!`-ed owned `String`**, parses it with a **fresh
`FileId`** (`parse_file(&unit_src, fid)`), and keeps the owned `String` alive in a `generated`
vec so its spans stay valid. The generator rewrite adopts exactly this: for each generator
method, **re-emit its full signature + rewritten body as owned source text and re-parse it
with a synthesized `FileId`**, then replace the method's `CompUnit`-level decl with the
re-parsed one before `StructTable::build`/`collect_insts` consume it.

```text
List<E> Gen(...) {
    yield return a;
    if (cond) yield break;
    for (var i in 1...n) { if (i > 2) yield break; yield return i; }
}
==> (re-emitted as a String, re-parsed with a fresh FileId)
List<E> Gen(...) {
    List<E> __yield = new List<E>();
    __yield.Add(a);
    if (cond) return __yield;
    for (var i in 1...n) { if (i > 2) return __yield; __yield.Add(i); }
    return __yield;
}
```

**Rewrite rules (a full RECURSIVE statement walk, not straight-line only):**
- Prepend `List<E> __yield = new List<E>();` to the method's **top-level** block.
- **Recurse into every nested block, `if`/`else`, `for`/`while`/`do`/`foreach`, `switch` arm,
  and `defer` body**, rewriting each `yield` in place and **preserving** the surrounding
  control flow: `yield return e` → `__yield.Add(e);` (in situ — a `yield return` inside a
  loop stays inside that loop); `yield break;` → `return __yield;` (in situ).
- Append a trailing `return __yield;` to the method's **top-level** block (after any loop),
  for the empty/fall-off path.

The worked example above shows the loop in rule-3 preserved: the `for` survives, `yield
return i` becomes `__yield.Add(i)` *inside* it, and the trailing `return __yield` lands after
the loop at top level.

Because the re-parsed body is ordinary source, the existing `collect_insts` walk
(`lower.rs:2139`) sees `new List<E>()` / `__yield.Add(e)` and instantiates `List<E>`
normally; the `foreach` over the generator's result is then the ordinary Count/Get (or
GetEnumerator) path. **Ownership:** the returned `List<E>` is an owned heap object; the
*caller* is responsible for `delete`-ing it (or `scope`-ing the loop), identical to any
method returning `new List<E>()` today — no new ownership rule.

**Ordering (pinned).** `rewrite_generators` runs in `lower_program` **before** both
`StructTable::build`/`collect_insts` **and** ownership analysis, so every downstream walk —
monomorph collection, tuple registration, lambda collection, AND `ownership.rs`'s two
wildcard-terminated `Stmt` walks (`456`, `812`) — only ever sees the desugared
`__yield.Add(...)` / `return __yield`, never a raw `YieldReturn`. This is why the
wildcard-walker gaps in §3.3 are safe for the generator path specifically: by the time those
walks run, the yields are gone. (They are *not* safe between T2 and T3 if shipped separately
— hence §7 lands T2+T3 together.)

### 3.5 llvm + runtime

**newbf-llvm:** no change (no new instruction; `call`/`call_indirect`/`alloca`/blocks all
exist). **newbf-runtime:** no change — the value-struct enumerator allocates nothing; the
eager `List<E>` uses the existing `Internal.Malloc`/`Internal.Free` path (`List.bf:16,19`)
already covered by the Stomp guard.

## 4. Worked examples (the run-corpus programs that prove it)

All under `e:/NewBF/beef-tests/run-corpus/`, `Program.Main -> int32`, `// expect: N`,
JIT-run full-i32 value checks under the Stomp guard (the authoritative gate). The five
existing foreach programs (`foreach_range.bf` → 45, `foreach_closed_range.bf` → 15,
`foreach_list.bf` → 60, `foreach_break.bf` → 7, `array_foreach.bf` → 100) must stay green
(verified expected values).

0. **`enum_manual.bf` — `expect: 6`** (the generic-value-struct ABI proof, T0). `new
   List<int32>()`, `Add` 1/2/3, take `e = list.GetEnumerator()`, then a **manual**
   `while (e.MoveNext()) sum += e.Current;` (no `foreach`) → 6. Pins the monomorphized
   generic value struct (`ListEnumerator<int32>`) returned by value, copied into an alloca,
   and mutated in place under the guard — in **isolation**, so a generic-value-struct
   miscompile surfaces in T0, not conflated inside T1's loop lowering.
1. **`foreach_getenumerator.bf` — `expect: 60`** (inline user type — T1's real proof,
   corlib-independent). A user `struct Bag` with `GetEnumerator()` returning a value-struct
   `BagEnumerator` (cursor over an inline buffer) exposing `MoveNext()`/`Current`. `foreach
   (x in bag) sum += x` over `{10,20,30}` → 60. Pins: the new branch fires; the value-struct
   `this` pointer carries MoveNext's `mIndex` mutation across iterations, and Current reads
   through the same slot (§3.1.1).
2. **`foreach_enum_break.bf` — `expect: 30`** (committed semantics, unambiguous). Same
   `Bag`, body `if (x == 30) break; sum += x;` over `{10,20,30}` → adds 10, 20, then breaks
   at 30 before adding → `sum == 30`. Pins the `break`-to-`exit` edge and
   `free_scopes_down_to` wiring (`lower.rs:7112`).
3. **`foreach_dispose_once.bf` — `expect: 1`** (direct, observable). Enumerator with a
   `Dispose()` that increments a static counter; iterate a 3-element bag with a mid-loop
   `break`; `Main` does `return disposeCount;` → exactly **1** (one loop, one Dispose). Pins
   exactly-once Dispose on the break edge under the guard.
4. **`foreach_dispose_return.bf` — `expect: 1`** (the return-through edge, the §3.2 fix).
   Same Dispose-counting enumerator; the loop body `return`s out mid-iteration via a helper
   that runs the loop and returns `disposeCount` *after* the loop would have continued —
   structured so the only way the returned value is 1 is if `Dispose` ran on the
   `return`-through edge (`free_all_scopes`, `lower.rs:6799`). Pins exactly-once Dispose on
   the **return** edge — the gap the prior draft missed.
5. **`yield_eager_basic.bf` — `expect: 6`.** A generator `List<int32> Nums() { yield return
   1; yield return 2; yield return 3; }`; `foreach (x in Nums()) sum += x` → 6. Pins the
   eager re-emit + monomorphization of `List<int32>` from the synthesized body.
6. **`yield_break.bf` — `expect: 3`.** `List<int32> Upto(int32 n) { for (var i in 1...n) {
   if (i > 2) yield break; yield return i; } }`; `foreach (x in Upto(5)) sum += x` → 1+2 = 3.
   Pins the **recursive** rewrite (yield inside a `for`, loop preserved) and `yield break` →
   early `return __yield`.
7. **`yield_empty.bf` — `expect: 0`.** A generator that `yield break`s immediately;
   `foreach` body never runs → 0. Pins the empty-sequence fall-off (the trailing `return
   __yield` over an empty list).

Each `.bf` is self-contained (inline-defines its own types; corlib `List<T>` is in the
prelude for the yield programs). The `// expect:` values fit in i32 (well under the 8-bit
AOT-probe caveat — these run under the JIT harness anyway, per MEMORY).

## 5. v1 scope vs explicitly deferred

**In v1:**
- `foreach` over a user type via `GetEnumerator()` → value-struct (or `Ref`) enumerator with
  `MoveNext()`/`Current`(`get_Current`)/optional `Dispose()`, resolved **statically by name**
  on the concrete enumerator struct (no interface). Composes with `break`/`continue` and the
  loop stack; `Dispose` runs exactly once on **every** exit edge (normal/`break`/`return`).
- Corlib **top-level** `ListEnumerator<T>` + `List<T>.GetEnumerator()` as the proven generic
  example (List itself still iterates via the faster Count/Get path).
- `yield return` / `yield break` via **eager materialization into a `List<E>`** for generators
  that declare a `List<E>` return type.

**Deferred (honest):**
- **Lazy / coroutine state-machine `yield`.** A compiler-synthesized resumable enumerator
  (state field + `MoveNext` switch + cross-yield local spill/reload) is the genuinely hard
  transform; the SSA "instruction does not dominate all uses" trap (a value defined before a
  yield and used after must be reloaded from the state struct, not kept in an SSA register)
  is exactly the class of bug the dominance machinery is sensitive to. v1's eager path
  **changes semantics**: no laziness, no infinite sequences, the whole sequence is
  materialized into an owned `List<E>`. Documented divergence.
- **`IEnumerator<T>` / `IEnumerable<T>` interface-typed enumerators + dynamic dispatch.**
  Blocked on generic-interface registration/monomorphization, which is out of scope:
  `collect_iface_bases_type` only handles **non-generic** classes (`td.kind == Class &&
  generic_params.is_empty()`, `lower.rs:1578-1579`); generic interfaces stay `Ptr`. v1
  duck-types the concrete enumerator instead. (See itables.md for the interface rationale;
  the in-tree gate above is the load-bearing fact.)
- **Heap-enumerator auto-`delete`/ownership** (a `GetEnumerator()` returning a `new`-d `Ref`
  the loop owns). v1 calls `Dispose()` on every edge but does **not** auto-`delete`; the
  corlib enumerator is value-typed to avoid the issue. Follow-on wires the heap body into a
  `ScopeAlloc` (`lower.rs:7852-7876`) for exactly-once free under the guard.
- **Typed / pattern `foreach` bindings** (`for (int i in …)`, `for (var (a,b) in …)`). The
  parser drops the declared type / pattern today (`parser.rs:1865-1867,1840-1862`; `name` is
  just a `Span`, `ast.rs:485-490`). Recording it on `ForEach` is independent low-risk polish,
  out of this feature's critical path.
- **Generator return-type inference** (a `yield`-method with no explicit `List<E>`). v1
  requires the explicit `List<E>` declaration.

## 6. Load-bearing risks + mitigations

- **Ratchet breakage (the headline risk).** The five foreach run-corpus files, the verify
  corpus (160/160), and the parser corpus are behavior-pinned. The range/array/Count-Get
  resolution order at `lower.rs:6932/6986/7042` must be **unaltered**; the new branch is
  inserted **after** the Count/Get probe. *Mitigation:* the new branch only fires when the
  Count/Get `sigs` probe returns `None` (the `if let Some(...) = sigs` at `lower.rs:7055` has
  no `else`, and `coll`/`coll_ty` remain in scope) — a strict additive extension of the
  fall-through-to-skip. The `Ref`-only gate (`lower.rs:7042`) means `foreach_list.bf` (60)
  still takes Count/Get. Acceptance names the five files explicitly.
- **Value-struct `this` aliasing (MoveNext state lost).** *Mitigation:* pass the `e_slot`
  alloca **address** as `this` for a value-struct enumerator (the `Struct(id)` lvalue →
  body-pointer arm of `struct_base`, `lower.rs:9583`), reusing the **same** address for all
  three calls and all iterations; hand-build the calls (the auto-getter / `try_property_get`
  precedent, `lower.rs:6036-6047/9395-9405`) via the new `call_instance_on_ptr` helper — do
  NOT route through `lower_method_call` (no Value-receiver entry point exists, §3.1.1).
  `foreach_getenumerator.bf` pins it.
- **Value-receiver `this` for `GetEnumerator` (one level up).** A `Struct(id)` collection
  receiver (and any value-struct rvalue receiver) has no pointer `this`. *Mitigation:*
  materialize `coll` into a fresh alloca and pass its address; pass `coll` directly only for
  `Ref(id)` (§3.1.1a).
- **Exactly-once Dispose under the memory guard — including `return`.** *Mitigation:*
  register `Dispose` as a scope-cleanup hook in the loop's `scope_allocs` frame so
  `free_all_scopes` (`return`, `lower.rs:6799`), `free_scopes_down_to` (`break`,
  `lower.rs:7112`), and normal fall-off each run it exactly once, with no double-emit at the
  `exit` top for a frame `break` already freed (§3.2). `foreach_dispose_once.bf` (break) and
  `foreach_dispose_return.bf` (return) assert the counter is exactly 1 under the Stomp guard.
- **First-of-kind generic value struct on the executable path.** No generic value struct has
  ever JIT-run from the runnable corlib (§2.3). *Mitigation:* `enum_manual.bf` (§4 ex 0)
  proves the ABI in isolation in T0, before T1 layers the loop on top.
- **SSA dominance.** The loop reuses the head/body/cont/exit block structure and inline
  `alloca`/`load`/`call` sequence the Count/Get arm already produces (`lower.rs:7058-7103`),
  which verifies clean today. No new phi; no value crosses a block edge except through
  allocas. *Mitigation:* copy the proven skeleton; the run-corpus JIT verifier is the net.
- **Generator rewrite — synthetic names + immutable AST.** The rewrite cannot fabricate
  `Span`-backed identifiers and cannot mutate the borrowed AST in place. *Mitigation:*
  re-emit each generator's body as owned source text + re-parse with a fresh `FileId` (the
  comptime `emit.rs:429-432` precedent), replacing the method decl before `collect_insts`.
  A recursive statement walk preserves control flow (§3.4). Ordering pinned before
  `collect_insts` AND ownership so no walk sees a raw `YieldReturn`.
- **Walker audit — the compiler does NOT enforce it for sema/ownership.** Only `Stmt::span()`
  and `print.rs::stmt` are exhaustive; every sema and ownership `Stmt` walk is
  wildcard-terminated (§3.3 table). *Mitigation:* T2 hand-edits each, ships a focused
  lambda/mono-collection test over a yielded expr, and lands together with T3.
- **sema ⊥ llvm boundary (HARD INVARIANT).** Everything is in `newbf-sema` emitting IR +
  named symbols (the GetEnumerator path is pure sema, as Count/Get already is) plus a parser
  AST enrichment and a sema re-parse. *Mitigation:* no new IR instruction → newbf-llvm
  untouched.
- **Comptime sandbox (forbidden here).** `foreach`-over-user-types and generators are runtime
  constructs; the generator rewrite uses the comptime *parser/FileId* mechanism but is never
  routed through `newbf-comptime` evaluation. No float constants → the JIT-FP-constant-pool
  MEMORY caveat does not apply.

## 7. Task breakdown

Each task is agent-assignable with a one-line seed and a concrete acceptance gate. Gates that
must stay green at **every** boundary: verify corpus 160/160, parser corpus, run-corpus
(authoritative). A task lands only when its own test plus all prior gates are green.

**Two independent chains** (corrected from the prior draft's false `T0→T1` dependency): chain
A is `{T0 ∥ T1}` (T1 pins on an inline `struct Bag`, NOT on the corlib enumerator, so it does
not depend on T0); chain B is `{T2+T3 together}`. T4 last. T1's internal critical sub-task is
the `call_instance_on_ptr` receiver helper + value-struct `this`/Dispose wiring.

**T0 — Corlib `ListEnumerator<T>` + `List<T>.GetEnumerator()`, with a manual-iteration proof.**
*Seed:* add the **top-level** generic `struct ListEnumerator<T>` (§2.3 — NOT nested) and
`List<T>.GetEnumerator()` to `newbf-corlib/bf/List.bf`; do **not** change `Stmt::ForEach`.
Add `enum_manual.bf` (§4 ex 0) exercising `GetEnumerator()` + manual `MoveNext()`/`Current`.
*Accept:* `enum_manual.bf → 6` passes under the JIT/Stomp harness; full verify + run corpora
green with the new corlib members in the prelude. Proves the generic-value-struct ABI in
isolation. Additive — no lowering change.

**T1 — The fifth `ForEach` branch (GetEnumerator/MoveNext/Current/Dispose) — RISKIEST.**
*Seed:* in `Stmt::ForEach` (`lower.rs:6924`), after the Count/Get probe fails
(`lower.rs:7055` `None` path), probe the receiver `Ref(id)`/`Struct(id)` for
`GetEnumerator()` (`.cloned()`), take the enumerator id from `ge.ret`, probe
`MoveNext`/`get_Current`/optional `Dispose`. Add a `call_instance_on_ptr` helper; materialize
a value-struct/rvalue receiver into an alloca for `GetEnumerator`'s `this` (§3.1.1a); emit the
head/body/cont/exit loop passing the `e_slot` **address** as `this` for value enumerators
(§3.1.1b); register `Dispose` as a scope-cleanup hook so it runs on normal/`break`/`return`
edges (§3.2). Pins on an inline `struct Bag` — corlib-independent.
*Accept:* `foreach_getenumerator.bf → 60`, `foreach_enum_break.bf → 30`,
`foreach_dispose_once.bf → 1`, `foreach_dispose_return.bf → 1` pass under JIT/Stomp; the five
existing foreach programs unchanged (named explicitly); verify 160/160. **Riskiest task** —
the value-struct `this`-aliasing + exactly-once-Dispose-on-every-edge correctness under the
guard, plus the new place-based call helper, is where a subtle miscompile or guard fault
would surface.

**T2+T3 (landed together) — `yield` AST variants + walker edits + eager-materialization
rewrite.**
*Seed (T2 portion):* add `Stmt::YieldReturn`/`Stmt::YieldBreak` to `ast.rs` (§3.3), a
`Keyword::Yield` arm to `stmt()` (`parser.rs:1339`), the forced `span()`/`print.rs` arms, and
**hand-edited** arms in every wildcard-terminated sema walker (`for_each_stmt_expr` 3718,
`collect_insts_stmt` 2190, `collect_lambdas_stmt`, `register_tuples_in_stmt` 951,
`collect_mixins_stmt`, `caps_stmt`, ownership.rs 456/812) plus a **diagnostic** (not
`unreachable!`) lowering arm.
*Seed (T3 portion):* a `rewrite_generators` pass in `newbf-sema` run in `lower_program`
**before** `collect_insts` and ownership: detect yield-bearing `List<E>`-returning methods,
**re-emit the rewritten body as owned source text and re-parse with a fresh `FileId`** (the
`emit.rs:429-432` mechanism — NOT in-place AST mutation), applying the recursive rules in
§3.4 (prepend `List<E> __yield = new List<E>();`; `yield return e` → `__yield.Add(e);` in
situ; `yield break` → `return __yield;` in situ; trailing `return __yield;` at top level).
*Accept:* `yield_eager_basic.bf → 6`, `yield_break.bf → 3`, `yield_empty.bf → 0` pass;
a parser-corpus fixture with `yield return e;`/`yield break;` round-trips; a focused test
proving a lambda/generic call inside `yield return …` is collected; non-generator methods and
the verify corpus (160/160) unchanged. **Why merged:** the AST variants are inert without the
rewrite, and shipping T2 alone leaves a green boundary where a parsed `yield` reaches a
wildcard walker / the lowering diagnostic un-rewritten (§3.3).

**T4 — Journal + doc cross-link + verify pin.**
*Seed:* add a numbered journal entry (design + outcome) to `docs/journals/`; add a focused
verify-corpus fixture mirroring `foreach_getenumerator.bf` (pin the loop IR shape); cross-link
this design doc.
*Accept:* journal entry present; verify corpus count incremented and green; commit pairs with
the entry (conventional style + Co-Authored-By trailer).

**Dependency order:** chain A `{T0 ∥ T1}` (T1 independent of T0 — inline `Bag` fixture);
chain B `{T2+T3}` together; T4 last. The two chains are independent; T1 is the behavioral
core and the critical sub-path (its receiver helper); T2+T3 is the generator core.

**Final task count: 4** (T0, T1, the merged T2+T3, T4).
