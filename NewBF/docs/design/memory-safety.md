# Manual-Memory Debug-Time Safety — Design

> Status: design-final (incorporates the correctness / integration / planning
> adversarial reviews). All file:line references verified against the tree at
> the time of writing.

## 1. Problem & goal

NewBF's defining promise (MANIFESTO core decisions 6–7, PLAN §1.3/§2.5a, Phase 4)
is **manual memory with debug-time safety**: you write `new`/`delete`/`scope`,
and the toolchain catches your mistakes — at compile time when provable,
deterministically (within the limits stated below) at runtime otherwise. Today
neither half exists. `new` lowers to `malloc`, `delete` to a dtor-chain + `free`
(lower.rs:7349–7459, 7547–7587), and that is the whole story: a use-after-free
reads freed heap silently, a double-free corrupts the CRT allocator, a leak
vanishes, and a `scope` object created inside an `if` branch leaks by design
(lower.rs:5854–5866).

The target is two complementary halves that compose:

- **(a) Runtime debug guard** (`newbf-runtime`, hooked into `new/delete →
  newbf_alloc/newbf_free` for **both** JIT and AOT): a **stomp allocator**
  (guard-page-per-allocation, decommit-on-free, **with freed pages quarantined
  — never recycled** — so a UAF/overrun faults at the offending instruction), a
  **leak ledger** (live-allocation side table + alloc-site, reported on demand
  and at shutdown), and **double-free detection** via persistent tombstones (the
  *authoritative*, deterministic UAF/double-free signal — the page fault is the
  fast hardware net, the ledger is the truth). On in debug, stripped to plain
  `malloc`/`free` in release.
- **(b) Compile-time delete-flow analysis** (`newbf-sema`): flag a *provable*
  leak (an owning `new` with no `delete`/`scope` on some path) and a *provable*
  double-`delete` (two deletes of the same binding with no reassignment between,
  **including `delete` of a `scope`-bound binding**) — conservatively (never a
  false positive), distinct from the runtime net.

Concrete target — after this feature, these three programs behave as specified:

```beef
// (a) runtime UAF — faults deterministically (pages quarantined), and the
//     ledger names the free-site in the crash dump.
static int32 Main() {
    let p = new Node();
    delete p;
    return p.value;      // ACCESS_VIOLATION at this load (page decommitted, never reused)
}

// (a) runtime double-free — second delete finds a Freed tombstone → abort.
static int32 Main() {
    let p = new Node();
    delete p;
    delete p;            // guard: tombstone hit → "double free of <site>" → abort
    return 0;
}

// (b) compile-time provable leak — `nbf check` reports, no run needed.
static int32 Main() {
    let p = new Node();  // warning: leak — 'p' is never deleted on any path
    return 0;
}
```

The composition rule (PLAN §2.5a): **compile-time catches the provable subset;
the runtime guard is the net for everything else.** They share nothing
structurally — the analysis is conservative and silent when unsure; the guard
always runs in debug.

### Guarantee precision (review correctness #2)

The reference `StompAlloc.cpp` recycles freed pages (`VirtualFree(MEM_DECOMMIT)`
+ clears the page used-bits), so a UAF only faults *until the address is
reused*. We **diverge deliberately**: freed pages are **quarantined** (decommit
and permanently retire the address range), making the page fault deterministic
for the lifetime of a debug run, at the cost of address-space growth (acceptable
in debug; the `Vm` reservation grows on demand and a process-recycle API resets
it between corpus programs — §4, §8). Independently, the **side-table ledger is
deterministic regardless of page state** and is the authoritative signal for
double-free and leak reporting.

## 2. Current state (file:line, verified)

**Allocation path — six call sites, three shapes.** A whole-tree grep
(`"malloc"|"free"`) gives exactly:
- `5603` — **closure-environment** `malloc(words·8)` for capturing lambdas (a
  `$Func` env). **Never freed anywhere** (a pre-existing leak this design
  inherits; shape = *raw*, no header).
- `7256` — `alloc_array`: `malloc(8 + count·esz)`, store length at base, **return
  `base+8` (the elements pointer)** (lower.rs:7250–7260). Shape = *length-prefixed
  array*. `lower_array_new`/`lower_array_new_init`/`pack_variadic_args` all route
  through this.
- `7395` — `lower_new` object: `malloc(size) -> Ref(id)` → store `$header`
  (vtable ptr or null) → `emit_field_inits` → base-ctor chain → ctor. Shape =
  *object* (8-byte `$header` at offset 0, user data after).
- `7468`, `7492` — `construct_string` / `lower_interp` String allocations. Shape =
  *object*-like.
- Frees: `7557` (`free(base)` for arrays, reconstructing `base = elements − 8`),
  `7564` (non-`Ref` bare free), `7586` (`emit_destroy`: dtor chain then `free(v)`).

**Scope tracking.** `scope_allocs: Vec<(BlockId, Vec<(Value, StructId)>)>`
(lower.rs:4644). `PrefixKw::Scope` (5848–5867) calls `lower_new`, then registers
the result **only if `cur == entry`** (5860–5864) — statement-level scopes only.
`free_scope_top` (4691–4698) / `free_all_scopes` (4702–4712) emit dtor+free in
reverse; `free_all_scopes` runs on `return` (4909) only. **`break`/`continue`
run no cleanup** and just `br` to the loop target. The documented gap (5854–5866):
a `scope` in a conditional sub-expression isn't tracked (would violate SSA
dominance) and **leaks**.

**Runtime crash machinery (built, hooks waiting).** `crash_dump.rs`:
signal-safe SEH filter (returns `EXCEPTION_CONTINUE_SEARCH`, line ~344 — a fault
propagates to WER and **terminates the process**; it is not catchable in-process
on a sibling thread), VEH stack-overflow handler, and **memory-guard shadow
state already wired**: `GUARD_INSTALLED`, `LIVE_ALLOCS`, `LIVE_BYTES`,
`TOTAL_ALLOCS`, `TOTAL_FREES`, `LAST_FREE_ADDR` atomics (42–48), with publish API
`note_memory_guard_installed`/`update_guard_metrics`/`note_free` (52–73) — **all
lock-free atomic stores/loads by design** — and a dump section that prints them.
`lib.rs` re-exports these and documents the stomp allocator as "Sprints 09–11,
not yet implemented". **Cargo.toml has zero dependencies and no `crate-type`.**
**Not linked into AOT** (aot.rs:79–82: "runtime staticlib … joins this arg list
when it lands"; the link list is CRT-only).

**IR.** `InstKind` (inst.rs:207–288): `Call { callee: Callee{name}, args }` is the
only alloc primitive. `Module.comptime: Vec<String>` (module.rs:59) is the
precedent for module-level metadata carried sema→backend.

**JIT vs AOT (the linchpin — verified).** JIT: `OrcJit::from_ir` (jit.rs:113–180)
adds **one** `DynamicLibrarySearchGeneratorForProcess` (jit.rs:152). Its own
comment (jit.rs:143–147) states it resolves "Win32 exports from loaded DLLs":
i.e. on Windows it resolves through `GetProcAddress` over **loaded modules' PE
export tables**. `malloc`/`free` resolve **only** because they live in
`ucrtbase.dll`/`msvcrt.dll`. There is **zero precedent** for resolving a
host-EXE-defined Rust symbol this way — `eval_const_i64` (newbf-comptime/eval.rs)
JITs only self-contained integer functions, and the driver calls
`install_crash_handler` as a *direct Rust call*, not via the JIT. AOT:
`emit_object` + `link_executable` (aot.rs:68–134) link CRT/kernel libs; no runtime
lib, `/ENTRY:mainCRTStartup` runs the C `main` codegen emits.

**Comptime is a third JIT host.** `eval_const_i64` (eval.rs:21) builds a **fresh
`OrcJit::from_ir` per call** and drops it on return. `newbf-comptime`'s deps are
`newbf-ir` + `newbf-llvm` **only** (NOT `newbf-runtime`). `newbf-tests` dev-deps
do **not** include `newbf-runtime` either.

**Diagnostics.** `Program{interner, graph, diagnostics: Vec<Diagnostic{span,
message}>}` (lib.rs:50–54), produced by `analyze` → `builder.resolve_and_check`
(lib.rs:58–76). **Lowering has no diagnostic sink** — `lower_program(files,
program)` returns only the `Module`. The DefGraph **does not carry method bodies**
(`BodyKind` is `{Block, Expr, None}`, model.rs:270) — bodies live only in the raw
`CompUnit` AST in `files`. The corlib **prelude is prepended inside lower.rs**
(3433–3447), **not** in `analyze()`. The run-corpus harness (run_corpus.rs:33–48)
is **JIT-only**, transmutes+calls `Program.Main`, checks the i32 — it ignores
`Program.diagnostics` and installs **no crash handler / no runtime**.

**Corpus contains real leaks.** `run-corpus/prelude_probe.bf` is `Probe p = new
Probe(); return p.Answer();` — `p` is read, never deleted, survives to the return
edge: a textbook provable leak. `list_hof.bf`/`list_hof_instance.bf` do `new
List<int32>()` then only read it. At least 3–4 corpus programs are genuine
never-deleted leaks. **The leak-detection ratchet must reconcile this** (§7, §9
Task 5.5).

**Crate edges.** `newbf-sema → {lexer,parser,ir,corlib}`; `newbf-llvm` is a
**dev-dependency only** of sema (the hard invariant). `newbf-runtime` depends on
nothing — so `newbf-llvm → newbf-runtime` is a **clean new edge (no cycle)**.

## 3. Approach

Two parallel tracks that meet only at the alloc-path symbol names and the JIT
symbol-registration seam. **Ship the runtime guard first (self-contained,
immediately valuable), then the compile-time analysis.** The single biggest
correction vs. the draft: **register `newbf_alloc`/`newbf_free` as ORC
*absolute symbols* inside `OrcJit::from_ir`, not via process export** — the
draft's "process export is simpler" was the root planning error and is unsound
on Windows (review correctness #1, integration #1, planning #1/#2).

### Track A — runtime debug guard (`newbf-runtime`)

**A0. Symbol registration in the JIT (the de-risking prerequisite, new Task 0).**
`OrcJit::from_ir` gains, *before* adding the module, a call to
`LLVMOrcAbsoluteSymbols` that defines in the main JITDylib the addresses of the
Rust functions `newbf_alloc`, `newbf_free`, and `newbf_install_crash_handler`
(taken as `fn` items: `newbf_alloc as usize`, etc.). This:
- works **regardless of which host** links/exports the symbols — the same
  `OrcJit::from_ir` serves the run-corpus harness, the comptime per-call JIT
  (eval.rs), and the driver, so **all three JIT hosts are covered by one edit**;
- requires the new crate edge `newbf-llvm → newbf-runtime` (clean, no cycle —
  §2) so `OrcJit` can name the Rust functions;
- is **proven in isolation by a smoke test BEFORE any rename**: JIT a tiny module
  whose one function calls `newbf_alloc(16,-1,0)` then `newbf_free(p)`, look it
  up, run it, assert no fault. This closes the load-bearing assumption the draft
  left to integration.

(Absolute symbols also stay valid when the generator is present — the generator
is a fallback for CRT/Win32 names; explicit definitions win.)

**A1. Symbol indirection, not call-site rewriting.** Sema already controls the
callee name string. Change every alloc-path emission to call **`newbf_alloc`** /
**`newbf_free`** (two stable C-ABI symbols owned by `newbf-runtime`) instead of
`malloc`/`free`. The debug runtime defines them via the stomp path; the release
runtime defines them as thunks (A5). The IR is byte-identical debug-vs-release —
the "strip" is a runtime-link/runtime-flag choice, not a codegen choice — so the
verify gate and the alloc-path knowledge all stay in one place and
`newbf-llvm` lowering is untouched (sidestepping the SSA-dominance trap).

**A2. Alloc kinds — the helper is shape-aware (review integration #3).** The
draft's "single one-edit helper" is false: there are three shapes. The helper is:

```rust
enum AllocKind { Object(StructId), Array { header_bytes: u32 }, Raw } // Raw = closure env
fn heap_alloc(&mut self, size: Value, kind: AllocKind) -> Value;
```

It emits `newbf_alloc(size: i64, type_id: i32, site_id: i32) -> ptr` with:
- `type_id = StructId.0` for `Object`, **`-1`** for `Array` and `Raw`;
- `site_id = 0` in the first slice (named sites are Task 7).

**All six sites route through it**, including 5603 (closure env, `Raw`) and 7256
(array, `Array{header_bytes:8}`) — so closures and arrays are *also* guarded.
This means the acceptance gate for the rename is the **run-corpus** (which
exercises arrays + closures end-to-end), **not** the verify-corpus.

**A2-array. The array/String ABI vs page-end alignment (review integration #2,
correctness minor).** The array path returns `base+8` and `delete` reconstructs
`base = elements−8`. To make this composable with the guard **the free side does
no pointer arithmetic**: `newbf_free` takes whatever pointer the program holds
and the **ledger maps it to the real allocation base**. Therefore:
- For `Array`/`Raw`, `newbf_alloc` returns a **front-aligned base** (malloc-like);
  the program adds its own `+8` header offset exactly as today. The ledger key is
  the base the program will later pass to free (for arrays, the design **drops the
  `−8` reconstruction at lower.rs:7557** and frees the elements pointer; the
  ledger records the elements pointer as the live key, mapping it to the true
  block). Forward overruns of array elements are page-protected only if the
  *Object* page-end alignment is applied; for `Array`/`Raw` v1 keeps front
  alignment and relies on the ledger for UAF/double-free (the deterministic
  signal). **Array underrun below the elements pointer is not page-protected**
  (matches StompAlloc; stated, not implied).
- For `Object` (Ref), `newbf_alloc` page-end-aligns the user region so a forward
  overrun past the object body hits the guard page. The `$header` at offset 0 and
  field-default writes index forward from the user pointer (no negative index), so
  page-end alignment is safe for objects. Objects whose size is not a page
  multiple leave slack between the body and the guard page (a small forward
  overrun reads slack, no fault) — the ledger remains the deterministic backstop.

This split is encoded in `AllocKind` (Object ⇒ page-end-align; Array/Raw ⇒ front-
align), so the contract is explicit per shape, not implied.

**A3. The stomp allocator** ports `StompAlloc.cpp` behind a thin `Vm` shim
(PLAN §2.5b). Reserve a large region with `reserve`; per alloc, commit
`ceil((size+header)/PAGE)` pages, write a header `{num_pages, magic, site_id,
gen}` at the page start. **Edge cases ported exactly**: `size == 0` and
`size % PAGE == 0` get the `alignedOffset += PAGE` bump StompAlloc uses, or the
header is clobbered / a size-0 alloc faults (review correctness, missing #5 —
empty collection inits can produce size-0). On free: **do the ledger lookup
FIRST** (never deref the possibly-decommitted page header on the free path —
review correctness #7), validate, mark the page range **quarantined**, and
`decommit`. A `BitSet` per range tracks committed pages; ranges grow on demand.

**A4. Ledger + tombstones + double-free + crash-dump join.** The ledger is a side
table — **not** in the object header (the 8-byte release `$header` stays stable;
ABI invariant preserved):

```rust
enum State { Live, Freed }                     // tombstone state
struct AllocMeta { base: usize, size: usize, site_id: u32, gen: u32,
                   state: State, free_site: u32, phase: Phase }
// keyed by the USER pointer the program holds (base for objects, elements for arrays)
live: HashMap<usize, AllocMeta>
```

- **Free** looks the key up: **missing → wild free** → abort+dump; **`Live` →**
  mark `Freed` (tombstone, keep the entry), record `free_site`, decommit/quarantine
  the pages; **`Freed` → double-free** → abort+dump naming `free_site` (review
  correctness #7). Tombstones are **never removed and pages never recycled**, so a
  stale pointer always re-finds its tombstone even though its address is retired —
  no guard-injected UAF from page reuse.
- **Crash-dump join is lock-free.** The ledger publishes via the *existing*
  atomic shadow-state API (`update_guard_metrics`/`note_free`) after each op. The
  **dump never takes the ledger lock and never reads the HashMap** — it reads only
  the atomics (review missing: the abort-on-double-free path must not hold the
  ledger lock while dumping; it publishes atomics, releases the lock, then aborts).
- **Leak report** walks the live set (entries still `Live`) on demand
  (`newbf_guard_report_leaks()`) and at `atexit`, **excluding `Phase::Comptime`
  entries** (A6) and excluding compiler-synthesized closure-env/array/String
  allocations from the *named-leak* report only if they carry no user site (they
  still count in metrics).

**A5. Debug/release strip without `cfg!(debug_assertions)` (review integration #6).**
Tying the strip to how `newbf-runtime` *itself* was compiled is wrong (a
release-profile driver building a debug Beef program would get thunks). Instead:
`newbf_alloc`/`newbf_free` are **always present**; behavior is selected by a
**runtime mode flag** `newbf_set_guard_mode(Stomp | Thunk)` set once at startup
(default `Stomp` in debug hosts, `Thunk` in release), read on the alloc fast path
via a relaxed atomic. `Thunk` mode is a straight `malloc`/`free` (it accepts and
ignores `type_id`/`site_id` — a 3→1 arg adaptation that is one register move, not
a bare jump; the small, documented release cost, review missing #3). For AOT, the
driver passes the target program's build profile to choose the mode the emitted
entry stub sets (A7). This decouples the strip from Rust's own profile.

**A6. Comptime phase tagging (review correctness/integration #4, the today-half
of the deferred §10 item).** `eval_const_i64` sets a process/thread-local
`comptime active` bit around the JIT call (a `newbf_guard_enter_comptime()` /
`_exit_comptime()` pair, exported from the runtime and called from
newbf-comptime — this needs **newbf-comptime → newbf-runtime** as a dep, or the
bit is set through `newbf-llvm` which both crates already use; the cleaner edge is
comptime→runtime). Allocations made while the bit is set are tagged
`Phase::Comptime` and **excluded from the leak report and live-count assertions**.
Per-JITDylib precise teardown of comptime allocations stays deferred (§10).

**A7. Site IDs + AOT entry (Task 7 + the entry-stub question, review missing).**
`site_id` is a `u32` literal sema passes as the third `newbf_alloc` arg, indexing
a **site table** emitted into the module as a global (`Module.alloc_sites:
Vec<AllocSite{function,file,line}>`, mirroring `Module.comptime`). First slice:
`site_id = 0`, no table — the guard still faults/aborts deterministically; the
dump shows the *free address* (already wired) but not a named site. The AOT
**entry stub**: there is no separate stub today (codegen emits the C `main`,
`/ENTRY:mainCRTStartup`). The runtime staticlib provides a real `mainCRTStartup`-
compatible bootstrap (or the link uses `/ENTRY:newbf_entry`) that calls
`newbf_install_crash_handler` + `newbf_set_guard_mode` then jumps to the codegen
`main`. This is a **runtime-provided entry**, decided here so the guard is active
in AOT (previously unspecified). For JIT, the harness/driver call install+mode at
startup (the run-corpus harness wiring is an explicit Task-3 subtask).

### Track B — compile-time delete-flow (`newbf-sema`)

**B0. Where the pass runs and what it sees (review planning #4, missing).** A new
pass `check_delete_flow` runs inside `analyze()` after `resolve_and_check`,
appending to `Program.diagnostics`. Because the DefGraph carries no bodies, it
**re-walks the raw `CompUnit` ASTs in `files`** (the same source lowering walks).
It needs just enough type info to know whether a `new`/`delete` operand is an
**owning class** (vs. String/array/value) and the static type of a deleted
binding; it builds a **minimal local symbol/type map per method body** (binding →
declared/inferred type, resolved against the def-graph types) — this is real,
scoped work (Tasks 5/6 own it; it is **not** a free traversal). It sees the same
**user sources** the corpus/corlib-slice provide; it does **not** see the
lower-time-injected corlib prelude (the prelude is library code, not user `new`s),
so the ratchet's universe is precisely the user `.bf` files (§8 pins this).

**B1. Tracked bindings — user-written `new` of a class only (review integration
minor).** The lattice tracks a local binding **only** when it is initialized from a
**user-written `new ClassType()`**. It does **not** track compiler-synthesized
allocations: String interpolation/literals, target-typed array/collection
literals, closure envs. Those are the runtime guard's job, never delete-flow's
(so the corpus's sugar-allocations can't trip the leak rule).

**B2. The lattice.** State per tracked binding ∈ `{Owned, Moved, Deleted, Dropped}`:
- `let p = new T()` → `Owned`.
- `delete p` → if `Owned`: `Deleted`; if already `Deleted` (no intervening
  reassignment): **provable double-free** diagnostic.
- **`delete p` where `p` is `scope`-bound** → **provable double-free** (the scope
  cleanup will also free it — review correctness #5). This is in the first
  analysis wave; the *runtime* minimal fix is B3-dereg below.
- `return p` / explicit `q = p` to another tracked binding / reassignment of `p`
  itself (the old value leaks if `Owned` and not deleted → flag if provable) →
  `Moved`. **Reserve `Moved` strictly for `return` and tracked-binding
  reassignment** (review correctness #4 — Beef has no implicit move).
- **A plain argument pass `f(p)` does NOT move** (Beef passes by reference; the
  caller still owns) — `p` stays `Owned`. This keeps leak detection alive for
  `list_hof`/`prelude_probe`-shaped code while staying sound.
- **Drop from tracking** (state `Dropped`, never diagnosed) the moment the binding
  is: address-taken, captured by a `$Func`, stored into a field/aggregate, or
  flows anywhere the analysis can't follow. These are the genuinely
  un-followable cases — distinct from `Moved`.
- At every function-exit edge, any binding still `Owned` (never deleted, never
  moved, never dropped, not `scope`) → **provable leak**.

**Conservatism rule (no false positives):** diagnose only when *every* path
agrees; `Dropped` wins any merge. Double-free ships first (near-syntactic);
leak ships after corpus reconciliation (§9 Task 5.5).

**B3. Scope cleanup correctness (lives in sema; review correctness #6,
integration #5, planning #5).** Two fixes:

1. **De-register on explicit delete (minimal runtime correctness, first wave).**
   `lower_delete` removes a binding from `scope_allocs` when it is explicitly
   deleted, so scope cleanup skips it — preventing the now-fatal double free a
   `scope p; … delete p;` would cause. (The *diagnostic* for this is B2; the
   de-registration is the belt-and-suspenders runtime guarantee.)

2. **All-exit cleanup via per-site null-guarded slots.** For a `scope`
   allocation that does not dominate block exit (e.g. inside an `if` branch):
   - allocate **one slot per scope-alloc *site*** (not per binding — two branches
     newing into "the same" binding need two slots, else the first leaks; review
     correctness #6), via a `FunctionBuilder` **entry-block alloca** API (reuse
     the existing `this_slot`/`idx_slot` alloca pattern that already lands allocas
     dominating all blocks);
   - **emit an explicit `store null → slot` in the entry block** (alloca does not
     zero memory — a missing null-store frees an uninitialized pointer on the
     not-taken path, review correctness #6);
   - on the allocating branch, `store ptr → slot`;
   - at **every** frame-exit edge, `if (slot != null) { destroy(load slot); }`.
   The only cross-block values are the slot pointer (dominates by construction)
   and the loaded pointer-or-null — the original SSA `new` value never crosses a
   block edge, satisfying the verifier.
   - **Unify the two mechanisms** so a given alloc appears in **exactly one**:
     dominating scopes use the direct value-list path (unchanged); non-dominating
     scopes use slots. Invariant: *value-list and slot-set are disjoint*. (Avoids
     the double-free between the two free paths — review correctness #6.)
   - **`break`/`continue`**: walk the `scope_allocs` frames between the current
     depth and the loop's push-point depth and run their cleanup (null-guarded for
     slot entries, direct for value-list entries) **before** the `br` — mirroring
     `free_all_scopes`, restricted to a depth range (review integration #5). The
     `loops` stack records the scope-frame depth at loop entry for this.

### Alternatives considered & rejected

- **Process-export of `newbf_alloc`/`newbf_free` for the JIT.** *Rejected (was
  the draft's choice).* On Windows `DynamicLibrarySearchGeneratorForProcess`
  resolves through PE export tables; a Rust `#[no_mangle]` symbol in the host EXE
  is not exported by default (`/INCLUDE` keeps a symbol; it does not export it).
  Verified: no host-EXE symbol is JIT-resolved anywhere today. **ORC absolute
  symbols (A0)** is robust, host-link-independent, covers all three JIT hosts in
  one edit, and is the mechanism every future runtime helper (reflection, FFI)
  will reuse.
- **Wrap `malloc`/`free` in `newbf-llvm` lowering.** Rejected: splits alloc
  policy across sema+llvm, risks the SSA-dominance trap, needs a debug/release
  switch in codegen. The symbol rename keeps all policy in `newbf-runtime` and
  emits identical IR.
- **New IR instructions `AllocGuard`/`FreeGuard`.** Rejected for v1: bloat with no
  behavioral gain over a renamed `Call` + constant args. `IrType: Copy` and the IR
  surface stay unchanged.
- **16-byte debug object header (`BfObjectFlags`).** Rejected: changes layout
  debug-vs-release, breaks ABI stability + itables/reflection layout. The
  side-table ledger keeps the header at 8 bytes always.
- **Recycling freed pages (faithful StompAlloc).** Rejected for v1: makes UAF only
  probabilistic and re-introduces guard-injected UAF on page reuse. **Quarantine**
  (never recycle) + ledger tombstones gives determinism; the process-recycle API
  bounds address growth for the corpus harness.
- **Deferred-cleanup blocks per scope.** Rejected vs. null-guarded slots:
  multiplies CFG edges per `return`/`break`/`continue`. A null-init slot is one
  alloca + one branch per exit and is obviously SSA-correct.
- **Full borrow-checker for delete-flow.** Rejected: huge and false-positive-prone
  against the zero-FP mandate. The 4-state intraprocedural lattice is the right v1.
- **`cfg!(debug_assertions)` strip.** Rejected: ties the guard to the runtime's own
  build profile, not the target program's. A runtime mode flag (A5) decouples it.

## 4. Representation / IR / runtime / ABI changes

**IR: none structural.** No new `InstKind`, no `IrType` change (`Copy`
preserved). The changes are the **callee name** + two constant args at alloc
sites:
- `newbf_alloc(size: i64, type_id: i32, site_id: i32) -> ptr` replaces `malloc`.
- `newbf_free(ptr: ptr) -> void` replaces `free`. **No `−8` arithmetic on the
  free side** (the ledger maps user-ptr → base); the array path frees the
  elements pointer directly.
- `type_id = StructId.0` for objects, **`-1`** for arrays/closure-env/raw.

**Optional module metadata (Task 7):** `pub alloc_sites: Vec<AllocSite>` on
`newbf_ir::Module` (alongside `comptime`); backend emits `__newbf_alloc_sites` +
count; runtime resolves `site_id → "<function> @ file:line"`. Debug-only; release
omits it. First slice omits the field.

**Runtime structures (`newbf-runtime`, new module `guard`):**

```rust
struct AllocHeader { num_pages: u32, magic: u32, site_id: u32, gen: u32 } // at page start
enum  State  { Live, Freed }
enum  Phase  { App, Comptime }
struct AllocMeta { base: usize, size: usize, site_id: u32, gen: u32,
                   state: State, free_site: u32, phase: Phase }
struct StompAlloc {
    ranges: Vec<Range>,                 // reserved VM block + committed-page BitSet
    live:   HashMap<usize, AllocMeta>,  // user-ptr (base|elements) -> meta (tombstones kept)
    next_gen: u32,
    stats:  Stats,                      // live_allocs/live_bytes/total_allocs/total_frees
    mode:   AtomicU8,                   // Stomp | Thunk (A5)
}
```

**OS shim (`guard::vm`)** isolating JIT-vs-stomp VM interaction (PLAN §2.5b):

```rust
trait Vm { fn reserve(&self, bytes)->*mut u8; fn commit(&self,p,bytes);
           fn decommit(&self,p,bytes); fn release(&self,p,bytes); }
```

Windows impl = `VirtualAlloc`/`VirtualFree`; an `mmap`/`mprotect` impl keeps the
crate buildable/testable off-Windows (matching crash_dump's `cfg` split). The
stomp reservation is **disjoint from the ORC RTDyld code memory** (jit_mm.rs owns
that), so guard pages cannot collide with JIT'd code (§2.5b). A
**`newbf_guard_reset()`** API releases all ranges + clears the ledger, for the
in-process corpus harness that runs ~204 programs in one process (review
correctness #8, integration #7).

**Concurrency / re-entrancy (review missing).** v1: a single global `Mutex`
guards `live`/`ranges`/`stats`. The **crash-dump path is lock-free** (atomics
only) so a fault while the alloc lock is held cannot deadlock the dump. The
abort-on-double-free path **publishes atomics, releases the lock, then aborts**.
`update_guard_metrics` is called on the slow path (after lock release) or
periodically — not under the lock — per the crash_dump.rs:56–57 cadence note.

**Crash-dump join: no new types.** Reuse `note_memory_guard_installed` (once at
init), `update_guard_metrics` (after ops), `note_free(addr)` (on free). Mangling
unchanged. **`StructTable` gains no lifetime** (invariant preserved). `scope_allocs`
frame entries gain a `Span` + an `is_scope`/slot discriminator (owned data).

**Cargo / crate-type.** `newbf-runtime`: add `crate-type = ["rlib", "staticlib"]`
(rlib for the `newbf-llvm → newbf-runtime` dep that A0 needs; staticlib for the
AOT link). New edges: `newbf-llvm → newbf-runtime` (clean, no cycle) and
`newbf-comptime → newbf-runtime` (for the phase bit, A6). `newbf-tests` does **not**
need a runtime dep (A0 means `OrcJit::from_ir` injects the symbols itself).

## 5. Sema / parser / comptime / runtime / codegen changes

**Parser:** **no changes** for v1 (`new`/`delete`/`scope` already parse;
`new:allocator`/`[AllowAppend]` are §10).

**JIT (`newbf-llvm/jit.rs`) — A0, the prerequisite:** in `OrcJit::from_ir`, before
`LLVMOrcLLJITAddLLVMIRModule`, register `newbf_alloc`/`newbf_free`/
`newbf_install_crash_handler` via `LLVMOrcAbsoluteSymbols` against the main
JITDylib (addresses from the `newbf-runtime` Rust items). Add the
`newbf-llvm → newbf-runtime` dep. This is the single seam that makes the rename
resolvable in the run-corpus JIT, the comptime per-call JIT, and the driver.

**Sema (`lower.rs`) — Track A rename (value-preserving):** one `heap_alloc(size,
AllocKind)` helper; route all six sites (5603 `Raw`, 7256 `Array`, 7395/7468/7492
`Object`) through it; replace the three `free`s with `newbf_free`, dropping the
`−8` reconstruction at 7557 (free the elements pointer; ledger maps it).
`emit_destroy` ends in `newbf_free(v)`.

**Sema (`lower.rs`) — Track B3 (behavior-changing scope fix):** `scope_allocs`
frame extension (per-site null-guarded slots + entry-block null-store via a new
`FunctionBuilder` entry-alloca API); unify dominating (value-list) vs.
non-dominating (slot) so each alloc is in exactly one; `lower_delete`
de-registers an explicitly-deleted scope binding; `break`/`continue` run the
depth-range frame cleanup before branching.

**Sema (`analyze` + new `ownership.rs`) — Track B1/B2 (additive diagnostics):**
`check_delete_flow(files, graph, interner) -> Vec<Diagnostic>`, called from
`analyze` after `resolve_and_check`, appended to `Program.diagnostics`. Builds a
per-body minimal type map (B0), runs the 4-state lattice, emits `warning`-class
diagnostics. No IR, no llvm — clean crate boundary. Diagnostics do **not** fail
`lower_program` or the run-corpus (which ignores them).

**Comptime:** symbols resolve via A0 (the comptime JIT is just another
`OrcJit::from_ir`). `eval_const_i64` wraps its call in
`newbf_guard_enter_comptime()`/`_exit_comptime()` (A6) so comptime allocs are
phase-tagged and excluded from leak reports. Delete-flow treats `[Comptime]`
bodies like any other (pre-JIT AST). Add `newbf-comptime → newbf-runtime`.

**Codegen — AOT path:** `link_executable` adds the `newbf-runtime` **staticlib**
to the lib list (the aot.rs:79–82 TODO); link `/ENTRY:newbf_entry` (the
runtime-provided bootstrap, A7) which calls `newbf_install_crash_handler` +
`newbf_set_guard_mode(profile)` then the codegen `main`. The driver selects
Stomp/Thunk by the **target program's** requested profile (A5). Both paths get
the guard with identical IR; only the linked definition / mode flag differs.

## 6. Interactions

- **`$Func` / closures (this wave):** a closure capturing an owning `new` pointer
  is the one case delete-flow can't follow → rule B2 `Dropped`s it (no false
  positive). The **closure env (lower.rs:5603) is now guarded** (routed through
  `heap_alloc(Raw)`), so a UAF on a captured value faults; it remains a
  compiler-synthesized allocation excluded from named-leak reporting (it is a
  pre-existing never-freed leak — when/if closure envs get freed is post-v1).
- **itables / interface dispatch (this wave):** `delete` of an interface-typed
  `Ref(iface_id)` must NOT reach `emit_destroy` (which indexes `structs.bases[id]`
  assuming a concrete class — would mis-walk or panic). `lower_delete` **explicitly
  detects an interface-typed Ref and takes the bare `newbf_free` branch (no
  dtor)** for v1 (memory-correct, leaks resources the dtor would free); add an
  assertion in `emit_destroy` that `id` is a concrete class (fail loud if an
  interface id ever reaches it — review correctness/integration minor). Before
  Task 3, grep the run-corpus to confirm no program deletes an interface local
  (the "not exercised" claim). Routing the dtor via the `$header` vtable slot
  lands with virtual dtors (§10). UAF protection is *strengthened* by itables: a
  stale interface call dereferences the `$header` vtable in a quarantined page →
  deterministic fault.
- **Owner-mangling / generics (this wave):** delete-flow runs on the generic AST,
  ownership is per-binding (monomorphization-transparent). The runtime guard is
  monomorph-agnostic (bytes + size); `type_id` is the monomorph's `StructId`
  (distinct per specialization — what the ledger wants).
- **Two-phase args (this wave):** unaffected — the rename is downstream of arg
  resolution; `lower_new`'s pending-arg fork is untouched.
- **Object `$header`:** unchanged at 8 bytes. Guard metadata is out-of-band
  (side table), preserving release zero-overhead and itables/reflection layout.
- **Diagnostics model:** delete-flow appends to `Program.diagnostics`; lowering
  stays sink-free; the run-corpus (behavior-only) can't regress from new warnings.

## 7. Risks & mitigations

- **JIT symbol resolution (was the #1 blocker).** Mitigated by A0 (ORC absolute
  symbols) + the Task-0 smoke test that proves resolution **before** the rename.
  No reliance on PE export; one seam covers all three JIT hosts.
- **Array/String ABI vs page-end alignment.** Mitigated by `AllocKind` (front-
  align Array/Raw, page-end-align Object) + ledger-keyed free (no `−8` on the
  free side). The array delete frees the elements pointer; the ledger maps it.
- **LLVM "instruction does not dominate all uses".** The scope fix is the riskiest
  part. Mitigated by the **per-site null-init slot** (entry-block alloca +
  explicit `store null`): only the slot pointer and loaded ptr-or-null cross
  blocks. Acceptance puts scope-in-if + early-return + break/continue programs in
  the **verify-corpus** (LLVM-clean), since the bug is a verifier failure — the
  existing 154 don't exercise conditional scopes (review planning #5).
- **UAF determinism overstatement.** Resolved by **quarantine** (never recycle) +
  **ledger tombstones** as the authoritative signal; the §1 guarantee text is
  scoped to "deterministic for the run" with the ledger as truth.
- **Double-free / page-reuse aliasing & header-deref-on-free.** Mitigated by
  ledger-first lookup (never deref a decommitted header) + persistent tombstones
  + no page recycling.
- **In-process corpus harness vs global ledger.** Mitigated by `newbf_guard_reset()`
  per guard-corpus program and **opt-in atexit reporting** (suppressed under the
  value-checking harness via an install flag) — the value-checking run-corpus must
  not trip the leak report (review correctness #8, integration #7).
- **Comptime allocs reported as leaks / symbol resolution.** Mitigated by A0
  (resolution) + A6 (phase tagging excludes comptime allocs from reports).
- **Leak ratchet vs. corpus that genuinely leaks (`prelude_probe`, `list_hof`).**
  Cannot have both "zero new corpus diagnostics" and "leak rule fires on
  prelude_probe". Resolved by **Task 5.5**: fix the leaking corpus programs to
  `delete`/`scope` what they `new` (they are buggy as written; the run-corpus is
  the behavioral gate, not a leak-policy gate), making the ratchet honest; the
  ratchet is then "zero **false** positives" with the fixed corpus clean.
- **Release strip untested.** Add a release-profile acceptance to Task 1 (Thunk
  mode exposes `newbf_alloc` as a malloc adaptor) and one release-profile AOT
  link+run (review minor).
- **Harness can't catch a fault in-process.** The SEH filter returns
  `CONTINUE_SEARCH` → a UAF/abort kills the process, not one test. The guard-corpus
  harness therefore uses a **child-process model** (spawn a small runner exe per
  guard program, inspect exit code / WER), not a sibling thread (review
  integration, missing).

## 8. Testing strategy

**Gates that must stay green at every task boundary:** parser-corpus (154/154),
verify-corpus (154/154, LLVM-clean), run-corpus (~204, JIT value-checked). The
rename (Task 2) touches the IR all allocating programs emit — its real gate is the
**run-corpus** (arrays + closures exercised), and it is **expected RED between
Task 2 and Task 3 only if A0 is not yet in**; since Task 0 lands A0 first, there
is **no red window** (review planning #1/#2: Task 0 → smoke-test-proven seam →
rename → wire).

**Task 0 — symbol resolution proof.** A `newbf-llvm` test: build a module whose
function calls `newbf_alloc(16,-1,0)`/`newbf_free`, JIT it, run it, assert it
resolves and does not fault. This is the de-risking gate the whole feature stands
on.

**Track A — runtime guard.** Rust unit tests in `newbf-runtime`: stomp
alloc/free/decommit, **size-0 and size%PAGE edge cases**, quarantine (a freed
address never returns from a later alloc), double-free→tombstone, wild-free,
ledger counts, `newbf_guard_reset`, Thunk-mode (release) path. Plus the
**JIT+stomp smoke test** (allocate in JIT'd code, free, assert the next read
faults — via the child-process runner).

**Track A — guard_corpus (separate harness, child-process model).** Each program
runs in a spawned runner exe; the harness asserts on the child's outcome:
`uaf_after_delete.bf` → ACCESS_VIOLATION; `double_free.bf` → guard abort
"double free"; `overrun_writes_guard_page.bf` (object forward overrun) →
ACCESS_VIOLATION; `leak_one_node.bf` → child runs clean, then `newbf_guard_report`
(or a sentinel exit code) shows `live==1` with the right site; `no_leak_balanced.bf`
→ `live==0`. **`newbf_guard_reset()` between programs** so the global ledger is
clean per run.

**Track A — JIT vs AOT parity.** One `newbf-llvm` AOT test: link `double_free.bf`
against the debug runtime staticlib, run, assert abort/non-zero exit;
`no_leak_balanced.bf` → clean exit. One **release-profile** AOT link+run to prove
the Thunk strip.

**Track B — compile-time.** New `ownership.rs`/`delete_flow.rs` tests asserting
`Program.diagnostics`: `provable_double_free.bf` (incl. a `scope p; delete p;`
case) → one diagnostic; `provable_leak.bf` → one "leak" diagnostic.
**No-false-positive ratchet:** the **fixed** user run-corpus + corlib-slice
produce **zero** delete-flow diagnostics (Task 5.5 must land first). Targeted
negatives stay silent: `delete_on_all_paths.bf`, `moved_by_return.bf`,
`passed_as_arg_still_owned_then_deleted.bf`, `scope_auto_freed.bf`,
`captured_by_closure.bf`, `string_interp_not_tracked.bf`.

**Track B3 — scope cleanup.** Add to the **verify-corpus** (LLVM-clean):
`scope_in_if_branch.bf`, `scope_in_both_if_branches.bf` (distinct objects, one
slot each), `scope_with_early_return.bf`, `scope_in_if_in_while_break.bf`. Add to
the **run-corpus** (value-checked, a dtor bumps a static counter; `Main` returns
the count): the dtor fired **exactly once** on each exit edge (fallthrough,
return, break, continue) and **never twice**.

**Comptime.** One comptime test that `new`s/`delete`s internally and folds
correctly (proves A0 resolves in the comptime JIT and A6 keeps it out of leak
reports); existing comptime tests stay green.

## 9. Task breakdown (ordered, agent-assignable, no red window)

**FIRST SLICE = Tasks 0–3** (a deterministic runtime UAF/double-free guard,
end-to-end, all gates green, observably tested).

0. **JIT absolute-symbol seam + resolution smoke test.** Files:
   `newbf-llvm/src/jit.rs`, `newbf-llvm/Cargo.toml` (+`newbf-runtime` dep),
   `newbf-runtime` (minimal `newbf_alloc`/`newbf_free` C exports — may be plain
   malloc/free thunks at this point). *Depends on: nothing.* **Accept:** the Task-0
   smoke test JITs a module calling `newbf_alloc`/`newbf_free` and runs without
   fault; all existing gates green. **This proves the load-bearing seam before any
   rename.**

1. **Stomp allocator + VM shim + ledger (runtime-only).** Files:
   `newbf-runtime/src/guard/{mod,vm,stomp,ledger}.rs`, `lib.rs`, Cargo.toml
   (`crate-type=["rlib","staticlib"]`). Port `StompAlloc.cpp` behind `Vm`
   (quarantine, size-0/page-multiple edge cases); ledger with tombstones;
   double-free/wild-free → abort+dump (ledger-first, no header deref);
   `newbf_set_guard_mode`, `newbf_guard_reset`, `newbf_guard_report_leaks`,
   `enter/exit_comptime`; publish via existing `note_*`/`update_guard_metrics`
   (lock-free dump); opt-in `atexit` report. Debug=Stomp, release=Thunk.
   *Depends on: 0 (so `newbf_alloc` already exists).* **Accept:** `cargo test -p
   newbf-runtime` green incl. alloc/free/decommit, **quarantine**, size-0,
   double-free tombstone, wild-free, ledger counts, reset, **release Thunk path**,
   and the JIT+stomp smoke test (child-process runner asserts the post-free read
   faults).

2. **Alloc-path symbol rename in sema (shape-aware helper).** Files:
   `newbf-sema/src/lower.rs` — `heap_alloc(size, AllocKind)`; route all six sites
   (5603 Raw, 7256 Array, 7395/7468/7492 Object); replace three frees with
   `newbf_free`; drop the `−8` reconstruction (free elements ptr). *Depends on: 0
   (seam resolves the symbols).* **Accept:** verify-corpus 154/154 LLVM-clean **and
   run-corpus ~204 green** (arrays/closures exercised; resolves via A0). No red
   window because 0 precedes 2.

3. **Wire the runtime into JIT + AOT hosts.** Files:
   `newbf-tests/tests/run_corpus.rs` (call `install_crash_handler` +
   `set_guard_mode` once; `newbf_guard_reset` if needed — symbols inject via A0,
   so **no new Cargo dep on the harness**); `newbf-driver` startup; `aot.rs`
   (runtime staticlib in the link list, `/ENTRY:newbf_entry` bootstrap A7); the
   guard_corpus child-process runner. *Depends on: 0,1,2.* **Accept:** run-corpus
   ~204 green; guard_corpus: `uaf_after_delete.bf`/`double_free.bf` observed as
   fault/abort, `no_leak_balanced.bf` ledger==0; one debug + one release AOT
   parity test.

4. **Scope cleanup on all exit edges (per-site null-guarded slots) + delete
   de-registration.** Files: `newbf-sema/src/lower.rs` (entry-alloca API,
   per-site slots + entry null-store, unify value-list/slot, `lower_delete`
   de-register, `break`/`continue` depth-range cleanup),
   interface-delete bare-free branch + `emit_destroy` concrete-class assertion.
   *Depends on: full first slice (0–3).* **Accept:** **verify-corpus** clean
   incl. the new scope-in-branch/both-branches/early-return/break programs;
   run-corpus value-checks dtors fire exactly once per exit edge; no regression.

5. **Delete-flow analysis: double-free first (incl. scope-delete).** Files:
   `newbf-sema/src/ownership.rs` (new), `lib.rs` (`check_delete_flow` from
   `analyze`; per-body type map). Implement the 4-state lattice; diagnose provable
   double-`delete` **and** `delete` of a `scope`-bound binding only. *Depends on:
   nothing structural (pure-ish AST + minimal type map).* **Accept:**
   `provable_double_free.bf` (incl. scope-delete) one diagnostic each; **zero**
   new diagnostics across the corpus; run-corpus unchanged.

5.5. **Corpus leak reconciliation.** Fix the genuinely-leaking corpus programs
   (`prelude_probe.bf`, `list_hof*.bf`, others found) to `delete`/`scope` what
   they `new`. *Depends on: 5 (the analysis identifies them).* **Accept:**
   run-corpus values unchanged (the fixes are behavior-neutral); the fixed corpus
   is leak-clean, making Task 6's ratchet honest.

6. **Delete-flow: provable leak.** Files: `ownership.rs`. Add exit-edge
   `Owned`-survivor → leak with the full `Dropped`/`Moved` rules (arg-pass stays
   `Owned`; only `return`/tracked-reassign move; capture/field-store/address-of
   drop; sugar allocations untracked). *Depends on: 5, 5.5.* **Accept:**
   `provable_leak.bf` one diagnostic; negatives silent; **zero** new (false)
   diagnostics across the fixed corpus.

7. **Site-id table + named fault/leak sites.** Files: `newbf-ir/module.rs`
   (`alloc_sites`), backend emit (`__newbf_alloc_sites`), `newbf-runtime` resolve
   `site_id→text`, crash-dump uses it; sema passes real `site_id`. *Depends on:
   2,3.* **Accept:** a UAF/leak report names `<function> @ file:line`; release
   omits the table; all gates green.

## 10. Open questions / decisions deferred

- **Virtual destructors through interfaces:** v1 frees-without-dtor on `delete` of
  an interface value (explicit bare-free branch + concrete-class assertion in
  `emit_destroy`). Routing the dtor via `$header` vtable slot lands with virtual
  dtors (slot-0-is-dtor convention to finalize against itables).
- **Closure-owned cleanup (capture lifetimes):** delete-flow drops captured
  bindings (no FP); the closure env (5603) is guarded but still never freed —
  *when* a closure releases captures is post-v1.
- **Per-JITDylib comptime allocation teardown:** v1 phase-tags comptime allocs
  (A6) so they don't pollute leak reports; precise per-dylib cleanup on comptime
  teardown is deferred (needs the comptime callback seam, PLAN §2.5c).
- **`[AllowAppend]` / custom allocators (`new:allocator`):** out of scope; the
  side-table + per-kind alignment is forward-compatible (append = larger size; an
  allocator id extends `AllocMeta`).
- **Array forward-overrun page protection:** v1 front-aligns arrays (ledger is the
  UAF/double-free net); page-end protection for array elements is a refinement.
- **Site-id representation:** `u32` index into a string table — revisit only if it
  grows large.
- **Leak report timing:** `atexit` auto-dump (opt-in / suppressed under the value
  harness) + an on-demand `newbf_guard_report_leaks()` for the IDE.
- **Debug/release granularity:** v1 whole-program via the runtime mode flag
  (per-target-program, set by the driver/entry stub). Per-type `[Debug]` control is
  future.
- **GC compatibility:** preserved — 8-byte header + out-of-band ledger + per-class
  field layout (GC.md §8) leave room for a later conservative-root mode.
