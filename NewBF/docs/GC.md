# NewBF — The GC "Escape Hatch": Conservative Roots, Precise Heap, and Plugging in NewGC

> Status: design note + **direction decided** (2026-05-31) — see "Direction" below.
> Sources: Beef + compiler at `E:\beef`; NewGC at `E:\NewGC`; NOD + NCL integrations — see §11.
> Companion: `CORETYPES.md` (the no-GC core data types).
> Bottom line this note argues: Beef demonstrates that you can get **precise heap tracing
> with zero safepoints**, which is exactly the substrate a NewGC-style collector wants — *if*
> you accept conservative roots (and therefore either non-moving GC, or mostly-copying with
> pinning).

NewBF ships **no GC** (manual `new`/`delete`, see `CORETYPES.md`). This note is about the
*optional* path: Beef has a real collector behind `BF_ENABLE_REALTIME_LEAK_CHECK`, and its
architecture answers a question we've fought before — **can a reclaiming collector avoid
"precise root safepoints all over the code"?**

---

## 0. TL;DR

Beef's collector is a **hybrid**:

- **Roots: conservative.** It scans registers (`GetThreadContext`) and the whole stack region
  word-by-word, validating each word against the heap before following it. **No `llvm.gcroot`,
  no `gc.statepoint`, no stack maps, no safepoint polls** — a repo-wide grep of the compiler
  finds *zero* of these.
- **Heap: precise.** Once a root pins an object, the graph is traced by **compiler-emitted
  per-type `GCMarkMembers` methods** that know each type's exact pointer fields (and array/list
  elements). The C++ collector knows nothing about layout — it calls back into generated code.
- **Non-moving mark-sweep**, on its own thread, preemptively suspending mutators
  (`SuspendThread`). Objects **never** change address.

The split is the whole point for us:

| Half | Beef approach | Reusable for NewGC? |
| --- | --- | --- |
| **Heap tracing** (the expensive, pervasive part) | precise, compiler-emitted, **safepoint-free** | **Yes — directly.** This is the gift. |
| **Root finding** | conservative scan + validate | Yes if non-moving / pinning; **No** if you want to move everything |

**The punchline:** the part we'd dread hand-writing — precise per-type heap tracing — Beef
gets *for free, without any safepoints*. The part that's "merely" conservative — roots — is the
part that forbids relocation. So the design fork isn't "manual vs safepoint-nightmare." There's
a third option: **conservative roots + precise heap → a reclaiming GC with no safepoints**,
either non-moving or *mostly-copying* (pin the few conservatively-rooted objects, evacuate the
precisely-reachable rest).

---

## Direction (decided 2026-05-31)

**1. NewBF stays no-GC by default — permanently.** Manual `new`/`delete` + `scope` is the
shipping memory model (MANIFESTO; `CORETYPES.md`). Strings/arrays are stdlib classes on the four
allocation primitives. This is the language's identity (the portfolio's manual-memory member),
not a provisional stance.

**2. An *optional* GC mode is on the roadmap, and its design is now fixed:** plug in **NewGC in
its `conservative-pin` mode — conservative roots + precise heap + mostly-copying, single-threaded.**
We deliberately take the path NOD kept only as a *fallback*, because it deletes the
precise-root/safepoint apparatus that made NOD's CLOS integration a nightmare. NewBF becomes
NewGC's first pure-conservative-roots client. The whole point: **the GC mode is a *runtime +
layout* job, not a *codegen-safepoint* job.**

Why this over the alternatives:
- **vs full-precise/moving (NOD's path):** would force compiler-emitted stack maps + safepoints +
  interior/derived-pointer tracking — the exact nightmare. Rejected unless measurements ever
  demand it (the Phase 3 escape hatch).
- **vs manual-only forever:** fine as the *default*, but an optional GC is cheap to add because
  the precise per-type pointer-field map falls out of the layout work we're doing anyway, and it
  unlocks GC-style Beef programs for those who want them.

**3. The plan, in order — each gated, none blocking the language:**
- **Phase 0 (now):** no GC. Finish the layout sprint (#60–63) + heap/`new`/`delete`. The layout
  sprint yields the precise per-type pointer-field map *for free* — that map **is**
  `BeefLayout::header_layout`.
- **Phase 1:** optional GC, conservative + pin-dominant, single generation. `BeefLayout` (6
  methods) + a conservative stack scan (`pin_pointers_in_ranges`) + alloc-triggers-`collect_auto`.
  No write barrier; interior-pointer risk moot. Proves the pipeline end-to-end.
- **Phase 2:** mostly-copying generational. Add the card write barrier; evacuate the
  precisely-reachable young majority, pin conservative-root targets. **Clear this blocker first:**
  interior-pointer pinning — wire NewGC's existing `object_start_at_or_before` (`evac.rs:603`)
  into the conservative pinner (likely our first upstream NewGC patch).
- **Phase 3 (only if measured):** precise roots (NOD's spill-slab statepoint scheme) — the escape
  hatch, reserved for if fragmentation/interior-pointers ever force full evacuation.

**4. Standing commitments (banked from NCL's scars):**
- Conservative roots specifically to avoid the manual `push_root` contract that rots into
  non-deterministic use-after-GC.
- A GC stressor + correctness oracle wired into `run-corpus` from the first GC commit.
- Structured, catchable GC errors from cycle 0 (fits our SEH/crash-dump ethos).
- No immortal "static area" crutch to dodge rooting.
- Vendor-pin a known-good NewGC revision (it's pre-0.1/unstable), as NCL does at `rev c500539`.

**Sequencing:** nothing here is on the critical path to running Beef programs. The GC is an
*additive* mode that rides the type-layout + heap machinery we build anyway. Details: §7 (NewGC
contract), §8 (mapping + phases), §9 (the two hard constraints), §10 (what we delete vs NOD).

---

## 1. How Beef finds roots — conservative, validated

`ConservativeScan` (`BeefRT/dbg/gc.cpp:876`) walks a memory region one aligned word at a time and
hands every word to `MarkFromGCThread`, under SEH so bad reads are swallowed:

```cpp
void* ptr = (void*)((intptr)startAddr & ~(sizeof(intptr)-1));   // align down
while (ptr < endAddr) {
    void* addr = *(void**)ptr;
    MarkFromGCThread((Object*)addr);     // validate-then-mark
    ptr = (uint8*)ptr + sizeof(intptr);
}
```

`MarkFromGCThread` (`gc.cpp:2736`) is the safety valve: it checks the value lands in the TCMalloc
range, resolves the owning span, **rejects interior/misaligned pointers** by re-deriving the
element base, and ignores words without the `ALLOCATED` flag or already marked. So a random
integer that merely *looks* like a heap address is filtered structurally. Residual risk is a
**false-positive retention** (an int that is coincidentally a valid object address keeps that
object alive) — a minor space leak, **never corruption**, *because the collector never writes
back through a root*.

**Registers are roots too.** `ScanThreads` (`gc.cpp:1596`) snapshots them via
`BfpThread_GetIntRegisters` → Win32 `GetThreadContext` (`Platform.cpp:2579`), captures `Rsp`, and
conservatively scans both the register file and `[Rsp, stackBase)`.

---

## 2. Suspension — preemptive, no safepoints

The GC runs on a dedicated thread and freezes mutators with OS `SuspendThread`
(`SuspendThreads`, `gc.cpp:2472`); there are **no cooperative safepoint checks** anywhere in the
generated code. Threads stop at arbitrary PCs — which is *why* roots must be conservative (the
live-pointer set at a random PC is unknown) and *why* objects can't move.

---

## 3. Non-moving — and the structural reason

Pure mark-sweep; freeing is in-place back to TCMalloc; no from/to-space, no forwarding slot, no
move write-barrier (the `BF_GC_INCREMENTAL` barrier path is compiled out). Objects' identity
*is* their address.

The non-moving constraint follows directly from conservative roots: to relocate an object you
must rewrite every reference to it, but a conservatively-discovered root is **ambiguous** (might
be a non-pointer), so overwriting it could corrupt data. Conservative roots are a one-way
street — readable, not rewritable. (Docs call Beef allocations "non-relocatable"; the *why* is
right here in the code.)

---

## 4. Precise heap tracing — the reusable gift

The base `Object.GCMarkMembers()` is empty; the compiler overrides it per type in
`BfModule::EmitGCMarkMembers` (`BfModule.cpp:20928`), walking `mFieldInstances`, chaining to the
base, and emitting a precise mark per field (`EmitGCMarkValue`, ~`20040`):

- reference field → `GC.Mark(obj)`
- `T[N]` sized array → loop, mark each element (if `T` wants marking)
- nested struct → call *its* `GCMarkMembers`

Arrays/`List<T>` hand-write the same precisely, gated on the element type's `WantsMark` flag
(`List.bf:1010`, `Array.bf:450`). All of this precision needed **no safepoints and no stack
maps** — it's ordinary generated virtual methods reached through one runtime callback,
`Object_GCMarkMembers` (`BfObjects.h`). That single seam is what a foreign collector reuses to
get exact field enumeration.

---

## 5. The replaceable-collector contract

What generated code calls into / a collector implements:

- `Object_GCMarkMembers(obj)` — **the precise per-type tracer hook** (most important).
- `GC.Mark(Object)` / `GC.Mark(void*, size)` / `GC.Mark!<T>` mixin — precise + conservative mark
  entry points; `Mark!` is compile-time dispatched (class → mark; struct → mark members;
  sized-array → per-element; unmanaged → nothing).
- `GC.AddRootCallback` / `CallRootCallbacks` — user root registration each cycle.
- `AddStackMarkableObject` / `RemoveStackMarkableObject` — emitted around `scope` objects whose
  type wants marking, so a stack object's *referents* stay live (the stack object itself carries
  `StackAlloc` and is never freed by the GC; reclaimed by unwind).
- append sub-objects: marked via `Dbg_MarkAppended` walking the append chain; carry `AppendAlloc`,
  not independently freed.
- Sweep reads TCMalloc span metadata + per-object header flag byte (`mObjectFlags`: 2-bit mark id
  + `ALLOCATED`/`STACK_ALLOC`/`APPEND_ALLOC`/`DELETED`).

With leak-check off, the whole surface degrades to no-op stubs — shipping Beef has no collector.

---

## 6. Reconciling with our NCL lesson

Our standing note from NCL: *"conservative scanning fails non-deterministically; precise roots
from the JIT are the structural answer."* That conclusion was **correct for a moving/relocating
collector** — there, an ambiguous root is fatal (you'd move the object and corrupt the
look-alike integer), and false-positive retention compounds with compaction goals.

Beef doesn't contradict it; it sits on the other side of the fork: **it doesn't move.** With a
non-moving collector, the conservative failure mode collapses to "occasional extra retention,"
which careful pointer *validation* (in-range + valid span + element-aligned + `ALLOCATED`) makes
rare and harmless. So the real decision is upstream: **moving ⇒ precise roots (safepoints);
non-moving (or pin-only-moving) ⇒ conservative roots suffice.** The safepoint nightmare is a
*consequence of choosing to move*, not an unavoidable tax on having a GC.

---

## 7. NewGC's client contract — VERIFIED (the mode we want is the default)

Confirmed by reading `E:\NewGC`. The configuration NewBF wants — **conservative roots + precise
heap + mostly-copying, single-threaded** — is NewGC's *native, default, tested* mode: the
`conservative-pin` Cargo feature (on by default), built explicitly "for JITs without statepoint
stack maps." The earlier §7 "three paths" resolve: **(B) mostly-copying is real and tested**;
(A) is "single-generation / pin-dominant, no evacuation pressure"; (C) full-precise is the
`--no-default-features` route we won't take.

**What the client implements** — a `HeapLayout` impl (a zero-sized type, monomorphized, no
`dyn`), six small methods (`traits.rs:117`):
- `classify(raw) -> WordKind` — per-cell tag dispatch (`Immediate` / `PointerCons` /
  `PointerHeader` / `Forwarded`); the safety boundary, called on every cell read.
- `header_layout(cell) -> ObjectLayout` — decode our object header into
  `{ total_cells, pointer_cells_start, pointer_cells_end }`. **This is the precise-heap field
  map — the `GCMarkMembers` equivalent.**
- `make_forward`, `rewrite_pointer_addr`, `make_pointer`, `FILL_WORD`.

**What NewGC provides** (all implemented + tested): `try_alloc_boxed_in` / `try_alloc_cons_in` /
`try_alloc_large`; generational mark-evacuate `collect_minor/major/full/auto`; precise root
visiting via a `FnMut(&mut PageEvacuator)` closure (`evac.visit(&mut word)`); **conservative root
pinning** `pin_pointers_in_ranges(gen, &stack_ranges)` with a six-gate validator (tag → self-stack
→ page → generation → start-bit → record); **pin-in-place + flip-page** mostly-copying (pinned
objects keep their address, everything else evacuates); a card write barrier `mark_card_at(slot)`;
`should_collect()` trigger policy. `TinyLayout` (`tiny_layout.rs`) is the minimal reference impl.

STW: single-threaded ⇒ we call `collect_*` synchronously under `&mut`; **NewGC does not suspend
us and expects no safepoint API.** (A multi-thread safepoint handshake exists but is unneeded.)

---

## 8. The NewBF mapping — what we build, and the phased path

The work is small: the hard half (precise heap) *is* the layout-sprint work we're already doing,
and the painful half (precise roots/safepoints) is *deleted* by going conservative.

**What NewBF must build:**
1. `BeefLayout: HeapLayout` — six tiny methods. `header_layout` reads the Beef object header (the
   `ClassVData*` from `CORETYPES.md`) and returns the pointer-cell range. The layout sprint
   already computes which fields are references.
2. A conservative stack scanner: at a collection trigger,
   `pin_pointers_in_ranges(G0, &[(rsp, stackBase)])`. Single-threaded ⇒ the one stack is current
   and quiescent; we already own the Windows stack-bounds/SEH plumbing from the crash-dump work.
   No `GetThreadContext`-of-other-threads, no suspension.
3. `new` → `try_alloc_boxed_in` + write our `ClassVData*` header into cell 0.
4. (Generational only) emit `mark_card_at(slot)` after reference-field stores.
5. Drive `collect_auto(...)` at allocation when `should_collect()`.

**Phased migration:**
- **Phase 0 (now / default): no GC.** Manual `new`/`delete` (`CORETYPES.md`). Ship the language.
- **Phase 1: optional GC, conservative + pin-dominant.** `BeefLayout` + conservative stack pin +
  single-generation `collect_minor` (nothing evacuates across gens ⇒ no write barrier; pinning
  dominates ⇒ interior-pointer risk is moot). Proves the pipeline end-to-end.
- **Phase 2: mostly-copying generational.** Add the card barrier; NewGC evacuates the
  precisely-reachable young majority while pinning conservative-root targets. Buys compaction +
  generational *without safepoints*. Resolve the interior-pointer constraint (§9) first.
- **Phase 3 (only if measured): precise roots.** Adopt NOD's spill-slab statepoint scheme (§10)
  if fragmentation/interior-pointers force full evacuation. The escape hatch, reserved.

---

## 9. The two hard constraints (honest)

Both are real and must be designed around before trusting a moving GC mode:

1. **Interior pointers don't pin.** NewGC's conservative pinner rejects any stack word that isn't
   an *object-start* address (the start-bit gate, `pin.rs:225`). Beef code routinely holds
   interior pointers — `&array[i]`, a `Span`'s `mPtr`, `&list[i]`. If a slot holds *only* an
   interior pointer with no live base pointer to the same object, that object is neither pinned
   nor (if precisely unreachable) kept — it could be moved/freed under us. Mitigations: (a) keep
   an object-start pointer live alongside any interior pointer (LLVM usually does, not
   guaranteed); (b) wire NewGC's internal `object_start_at_or_before` (`evac.rs:603`, already
   exists) into the conservative pinner so interior hits pin the containing object — the clean fix
   and probably the first NewGC patch we'd want; (c) Phase 1 (pin-dominant) sidesteps it.
   **This is the #1 thing to settle before Phase 2.**
2. **Contiguous pointer fields per object.** `ObjectLayout` is a single `[start, end)`
   pointer-cell range, not a bitmap. Beef structs interleave pointer/non-pointer fields. Since
   *we control layout*, the clean answer is **field-reorder so references are grouped** (Beef's
   `[Ordered]` types are the rare exception); fallbacks are over-scan (`all_pointers`, relying on
   non-pointer words classifying as `Immediate` — NOD does exactly this for user classes) or
   `opaque`. Decide per-type at layout time.

---

## 10. What we *delete* vs NOD — and NCL's process lessons

**NOD runs NewGC in full-precise/moving mode (`DylanLayout`), and it was the "nightmare."** It
does *not* use LLVM `gc.statepoint`; it hand-rolls precise roots: a global backward-liveness
fixpoint, a per-function entry-block spill **slab**, spill-before-call / reload-after-call
brackets, GC-param "home" allocas, phi-before-reload ordering, per-site JIT+AOT safepoint metadata
tables, and safepoint **polls** at loop headers. A single-threaded NewBF on conservative roots
**deletes all of it.** NOD even *kept* a conservative pinner as a fallback; the precise scheme
exists only because Dylan chose a fully-moving heap. The CLOS-specific costs (dispatch-as-
safepoint, multiple-value-return root buffer, `make`/`initialize`) don't apply to Beef at all.

| Precise/NOD burden | NewBF (conservative, single-thread) |
| --- | --- |
| global liveness fixpoint + `safepoint_roots` | — gone |
| spill-slab + begin/end_safepoint + reload | — gone |
| per-site JIT/AOT safepoint tables | — gone |
| GC-param home allocas, phi-before-reload | — gone |
| safepoint poll injection at loop headers | — gone (single thread, sync GC) |
| precise `HeapLayout` (`header_layout`) | **kept** (≈ the layout-sprint work) |
| allocation triggers a collection | **kept** (sync, at the alloc site) |
| write barrier on ref stores | **kept only if generational** |

**NCL is literally NewGC now** (git dep, pinned `rev c500539`); its page-heap *was extracted into*
NewGC. It drives a **hybrid** (precise push/pop shadow-stack + conservative stack-pin backstop)
because it's multi-threaded *and* moving. Lessons banked for our GC sprint:
- **The manual precise-root contract rots.** NCL's `emit_expr` co-locates codegen and root
  discipline; one missed `push_root` around an allocating call is a use-after-GC that crashes far
  from the cause, non-deterministically. *Conservative roots remove this whole failure class* —
  the single best reason to take the conservative path.
- **Component GC tests prove mechanics, not correctness** — bugs cluster at layer boundaries
  (TLAB vs allocator slab size; recycled-page zeroing). Wire a *stressor workload + correctness
  oracle* into CI from day 0 (`run-corpus` is the natural home — add a GC-stress program with a
  known answer). Structured GC errors from cycle 0 (NewGC's `GcStallError`→catchable condition
  fits our SEH/crash-dump ethos).
- **Don't park long-lived objects in an immortal "static area"** to dodge rooting — NCL did
  (closures) and hit static-area exhaustion that disabled features.
- The reported NCL "hang" is mostly *not* a GC-core deadlock: the most literal one is CLOS
  `FORMAT`-from-worker racing a non-thread-safe method cache (pure Lisp-side); the GC-side "stall"
  is mid-evacuation OOM (resource-flow, now a catchable condition). Single-threaded NewBF avoids
  the former entirely; the latter says **size the heap generously and trigger early** (NewGC
  panics/poisons on mid-evac OOM).

---

## 11. Source map

**Beef GC** (conservative-root + precise-heap + non-moving reference):
- Roots/scan: `BeefRT/dbg/gc.cpp` — `ConservativeScan` :876, `MarkFromGCThread` :2736,
  `ScanThreads` :1596, `SuspendThreads` :2472, `Sweep` :1161.
- Register capture: `BeefySysLib/platform/win/Platform.cpp` — `BfpThread_GetIntRegisters` :2579.
- Object header/callbacks: `BeefRT/rt/BfObjects.h` — `mObjectFlags` ~152, `BfRtCallbacks` ~84.
- Precise marking emission: `IDEHelper/Compiler/BfModule.cpp` — `EmitGCMarkMembers` :20928,
  `EmitGCMarkValue` ~20040; `WantsGCMarking` in `BfResolvedTypeUtils.cpp` :2678.
- Negative result: zero `gcroot`/`statepoint`/`safepoint`/`stackmap` in `IDEHelper/Compiler`.

**NewGC** (`E:\NewGC\crates\newgc-core`): `HeapLayout` trait `src/traits.rs:117`
(`ObjectLayout` :71); conservative pinner `src/page_heap/pin.rs` (`pin_pointers_in_ranges` :119,
six-gate `pin_range_one` :171, interior-reject :225); evacuator `src/page_heap/evac.rs`
(`visit` :291, pin-skip copy :948, `object_start_at_or_before` :603); alloc
`src/page_heap/alloc.rs` (:304, :438); barrier/trigger `src/page_heap/space.rs`
(`mark_card_at` :833, `should_collect` :596); reference impls `src/tiny_layout.rs`,
`src/lisp_layout.rs`; default feature `Cargo.toml:8` (`conservative-pin`);
docs `docs/conservative-pinning.md`.

**NOD precise integration** (`E:\NewOpenDylan\NewOpenDylan`): `nod-runtime/src/dylan_layout.rs`
(HeapLayout impl), `wrapper.rs` (object header), `classes.rs` (per-class layout fns; over-scan
policy :415); root machinery `heap.rs` (`ROOT_STACK` :400, `visit_roots` :1828); codegen
`nod-llvm/.../codegen.rs` (spill-slab :4692, begin/end_safepoint :4482/:4608, poll emission :2374,
card barrier :3971); liveness `nod-dfm/src/liveness.rs:57`; backend feature
`nod-runtime/Cargo.toml:36`.

**NCL** (`E:\CL\NewCormanLisp`): `src/ncl-runtime/src/gc.rs:17` (`Heap = PageHeap<LispLayout>`),
`Cargo.toml:16` (newgc git dep `rev c500539`); `mutator.rs` (coordinator / roots / barrier
façade); JIT root emit `src/ncl-llvm/src/lib.rs:1705` (`emit_safepoint_wrap`); lessons
`docs/GC_LESSONS.md`, `docs/gc_bughunt_tinyleak.md`.
