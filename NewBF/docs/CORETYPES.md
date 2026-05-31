# NewBF — Beef Core Data Types & the Allocation Contract

> Status: design note (research synthesis). Date: 2026-05-31.
> Sources: the upstream Beef corlib + compiler at `E:\beef` (read-only), cross-checked
> against the official docs (beeflang.org). Section 10 lists exact files/lines.
> Companion: `GC.md` (the optional-collector "escape hatch" and how a precise/NewGC-style
> collector could plug in).

This note records **how Beef implements `String`, `T[]`, `List<T>`, `Span<T>`/`StringView`
with no garbage collector**, and what NewBF must build to support them. It is the map for
the heap/`new`/`scope`/`append` sprints that follow the layout sprint.

---

## 0. TL;DR — the finding that shapes the work

`String`, `T[]`, `List<T>`, `Span<T>` are **not compiler built-ins**. They are ordinary
Beef classes/structs in corlib that rest on **four allocation primitives the compiler must
provide**. Nothing about them needs a GC — ownership is deterministic (`delete`, or
scope-bound auto-free).

This cleaves the work in two:

| Compiler / runtime (our Rust)                  | Stdlib (Beef `.bf` we compile)        |
| ---------------------------------------------- | ------------------------------------- |
| object header + vtable emission                | `String.bf`                           |
| `new` → malloc + ctor; `delete` → dtor + free  | `Array.bf` (`Array1<T>`)              |
| `scope` → alloca + scheduled dtor              | `Collections/List.bf`                |
| **append allocation** (`[AllowAppend]`)        | `Span.bf` / `StringView`             |
| `malloc`/`free`/`memcpy`/`memset` intrinsics   | (just Beef code, once primitives exist) |

**Beef has no production GC.** `System.GC` exists but the entire collector is behind
`#if BF_ENABLE_REALTIME_LEAK_CHECK` — a *debug leak-checker's* scanner; with that define off
it is inert stubs. A no-GC backend is fully Beef-compatible; we provide the `GC` API surface
as stubs exactly as Beef does in release. (The collector mechanism itself is interesting for
other reasons — see `GC.md`.)

---

## 1. The compiler ↔ stdlib contract: four primitives

### 1.1 Object header = one `ClassVData*`
Every heap object's first word (8 bytes on 64-bit, **release**) points to its type's
vtable/type-data. `new` stores it at `GEP(obj, 0, 0)`; `GetType()` reads the type-id out of it.

Beef's *debug* build uses a 2-word header and steals the low 8 bits of the vtable pointer for
object flags (`StackAlloc=0x08`, `AppendAlloc=0x10`, `Deleted=0x80`, plus GC mark bits). We do
**not** need the debug header — but `delete` still needs to know "heap object I free" vs
"scope/append object I must not free" (see §8, decision 1).

### 1.2 `new` / `delete`
- `new T(...)` → `call malloc(instSize)` → store vtable → run ctor.
- `delete x` → null-check → **virtual** dtor (`~this` is vtable slot 0, chains to base) → `call free(ptr)`.
- `malloc`/`free` link-names are configurable in Beef; default CRT.
- **There is no `realloc` anywhere in Beef.** Every grow is *alloc-new + memcpy + free-old*.
  Keeps our runtime surface tiny.

### 1.3 `scope` (stack lifetime)
`scope T(...)` → `alloca` **hoisted to the function entry block** (so loops don't grow the
stack) → store vtable → schedule the destructor LIFO at lexical scope exit.
`scope::` extends lifetime to the whole method; `scope:Label` to a labeled block. We already
emit `alloca`; the new piece is deferred-dtor scheduling (and the looped/dynamic-size reuse
path Beef calls `_CreateDynAlloc`, which we can defer by restricting `scope` to fixed sizes
initially).

### 1.4 Append allocation — the hard primitive, and the key enabler
`[AllowAppend]` lets a constructor do `append T[n]` so the **object header and its
variable-length payload land in one allocation**: `[fields][payload…]`. Mechanism (from the
compiler source):

1. For each `[AllowAppend]` ctor, synthesize a static twin `__CalcAppend` that runs the same
   body but only *sums appended byte sizes* (with alignment).
2. Inject a hidden `__appendIdx : ref int` cursor into both.
3. At the `new` site: call the twin (constant-fold when sizes are known → fixed-size alloc,
   no extra call) → `malloc(instSize + appendSize)` as one block.
4. Inside the ctor, each `append` carves `[idx, idx+size)` off the tail and bumps the cursor;
   `EmitAppendAlign` rounds the cursor up to each payload's alignment.

This single-allocation trick is what makes `String`'s SSO and `T[]`'s inline elements possible.
`[AllowAppend(ZeroGap=true)]` additionally guarantees no padding gap (and forbids subclasses
adding fields) so payload addressing can be pointer-free.

### 1.5 Intrinsics
`Internal.Malloc`/`Free` (→ `malloc`/`free`), `MemCpy`/`MemMove`/`MemSet`, `CStrLen`. The
collections use stride/alignment-aware `MemCpy`.

---

## 2. `String` — `[Ordered] class String`, UTF-8, owns its buffer

Three fields (lengths are **32-bit** by default; `BF_LARGE_STRINGS` → 64-bit):

```
mLength            : int32      // live byte count
mAllocSizeAndFlags : uint32     // low 30 bits = capacity; bit31 = DynAlloc; bit30 = StrPtr
mPtrOrBuffer       : char8*     // DUAL-PURPOSE
```

Flag constants: `cSizeFlags = 0x3FFFFFFF`, `cDynAllocFlag = 0x80000000`, `cStrPtrFlag = 0x40000000`.

`mPtrOrBuffer` is decoded by one accessor:

```
Ptr => (cStrPtrFlag set) ? mPtrOrBuffer : (char8*)&mPtrOrBuffer
```

Three storage states:
- **Inline / SSO** (`StrPtr=0`): characters start *at the address of the field itself* and flow
  into append-allocated bytes past the object. The "small-string optimization" **is** this
  scheme — there is no separate SSO struct.
- **External, borrowed** (`StrPtr=1, DynAlloc=0`): points at appended/borrowed memory; not freed
  (string literals, `Reference()`).
- **Heap-owned** (`StrPtr=1, DynAlloc=1`): grown via 1.5× realloc; the **only** state `~this` frees.

Other facts: characters are `char8` (UTF-8 bytes); `mLength`/capacity count **bytes**, not code
points. Buffer is **not** NUL-terminated until `CStr()`/`char8*` cast forces it
(`EnsureNullTerminator` writes `\0` at `Ptr[mLength]` *without* bumping `mLength`). Literals are
pooled and static — never `new`/`delete`d. Allocation routes through virtual `Alloc`/`Free`
hooks (`new:this`/`delete:this`) so a `String` can use a custom allocator.

---

## 3. `T[]` — `class Array1<T> : Array`, elements stored inline

```
Array:      int32 mLength            // total element count
Array1<T>:  T     mFirstElement      // element [i] lives at (&mFirstElement)[i]
```

`new T[n]` = **one block** of `offsetof(mFirstElement) + stride(T)*n` (note **stride** — the
aligned size — not `sizeof`). No separate element buffer; pure append allocation. `Ptr` is
`&mFirstElement`; bounds-checked index does `if ((uint)i >= (uint)mLength) ThrowIndexOutOfRange`.

Multi-dimensional `Array2<T>..Array4<T>` add `mLength1..` fields before `mFirstElement` and
address row-major (`((i0*L1+i1)*L2+i2)…`). **Start with `Array1<T>` (1-D) only; defer multi-dim.**

---

## 4. `List<T>` — `class`, separate heap buffer (no append needed)

```
mItems             : T*    // separate heap block (unlike Array's inline storage)
mSize              : int   // count
mAllocSizeAndFlags : int   // low bits = capacity; bit31 = DynAlloc
```

Flags: `SizeFlags = 0x7FFFFFFF`, `DynAllocFlag = 0x80000000` (one flag — `mItems` is always a
real pointer). Grows **2×** from a default capacity of 4 via alloc-new + memcpy + free-old (old
buffer freed *after* the insert, so you can `Add` a reference to an existing element). Frees
`mItems` in `~this` iff `IsDynAlloc`. A `[AllowAppend] this(int capacity)` ctor can co-allocate
the initial buffer with the object (then `DynAllocFlag` stays clear and the dtor doesn't free it).

**`List<T>` needs no append-allocation primitive for its buffer** — making it the easiest
owning collection to bring up first.

---

## 5. `Span<T>` / `StringView` — non-owning value structs

```
struct Span<T> { T* mPtr; int mLength; }   // borrows; allocates and frees nothing
```

`StringView` *is* `Span<char8>`. These own nothing; whoever owns the backing
`String`/`T[]`/`List`/buffer frees it. Implicit `T[] → Span<T>` conversion; `Slice` returns
another view. They are *value types* — implementable as soon as the layout sprint gives us
aggregate value structs (no heap needed).

---

## 6. Ownership model

Deterministic, no GC:
- **`delete x`** runs the dtor then frees. `delete:allocator x` routes to a custom allocator's
  `Free`/`FreeObject`. `delete:append x` / `delete:null x` run the dtor *without* freeing (for
  append/stack/mixin memory that something else reclaims).
- **`~this`** destructors; `delete` chains base dtors via vtable slot 0.
- **`defer delete x;`** pairs allocation with cleanup at the allocation site.
- **Convention (the important idiom):** take inputs as borrows (`StringView`/`Span<T>`), write
  outputs into a **caller-owned** `String`/collection. e.g. `void GetName(String outName) =>
  outName.Append("Brian")`. Avoids hidden allocations and ambiguous return ownership.
- Containers that own heap elements offer helpers (`DeleteContainerAndItems!`) — verify in corlib.

---

## 7. NewBF roadmap

Dependency chain against current state (control flow done; **layout sprint in progress**):

1. **Layout sprint (#60–63, now):** aggregate types + field GEP. *Prerequisite* — gives us the
   object header, `String`'s fields, `Array1<T>`'s inline element, and the `Span`/`StringView`
   value structs.
2. **Heap + object model:** object header, `malloc`/`free` runtime, `new`→malloc+ctor,
   `delete`→dtor+free, vtable emission. Unlocks `class` reference types.
3. **`scope`:** entry-block-hoisted `alloca` + LIFO deferred-dtor scheduling.
4. **Append allocation:** the `__CalcAppend` twin + `__appendIdx` + single-block sizing.
5. **Port the stdlib:** then `String.bf` / `Array.bf` / `List.bf` / `Span.bf` are just Beef we compile.

**Recommended two-milestone split** (gets strings working before the hardest primitive):

- **Milestone A — no append:** object header + `new`/`delete` + a **separate-buffer** `String`
  and `List<T>` + the `Span`/`StringView` value structs. Real, correct strings and dynamic
  lists. Not byte-identical to Beef's `String` (no SSO; two allocations) but semantically right.
  `List<T>` needs no append at all.
- **Milestone B — append allocation:** add `[AllowAppend]` → real `String` SSO + inline-element
  `T[]` → full Beef-ABI fidelity. Non-negotiable eventually (Path B faithfulness); A de-risks it.

---

## 8. Open decisions

1. **Delete-kind encoding.** `delete x` is dynamic, so the runtime must know free vs don't-free
   for scope/append objects. Beef uses header flag bits. Options: (a) an explicit flags byte in
   our header; (b) steal low vtable-pointer bits like Beef-debug; (c) lean on static knowledge
   at `delete` sites where possible. Leaning **(a)** — clean, no pointer-bit games.
2. **SSO now or later** — Milestone A skips it (separate buffer); B adds inline-at-`&field`.
3. **1-D arrays first** — ship `Array1<T>`; defer `Array2..4`.
4. **Width knobs** — pin `int_strsize`/`int_arsize` = 32-bit unless we want `BF_LARGE_*`.
5. **Header debug flags policy** — decide `mObjectHasDebugFlags` *before* freezing layout offsets
   (debug vs release header is 16 vs 8 bytes; shifts every field offset).

---

## 9. Unverified / to confirm

- `int_cosize` typealias (List's length type) wasn't located in the files read — assume it
  mirrors `int_arsize`; verify before pinning.
- The runtime free routine that inspects `AppendAlloc`/`StackAlloc` flags lives in the C++
  runtime (`BeefRT`), not corlib — we implement that ourselves in `delete`.
- `T[]` delete semantics and "container owns its elements" helpers are inferred from the uniform
  class/`new`/`delete` model, not quoted doc sentences — confirm in corlib.

---

## 10. Source map (when we implement)

**corlib** (`E:\beef\BeefLibs\corlib\src\`, flat layout — no `System\` subdir):
- `String.bf` — layout/flags ~42–75; append ctors ~77–117; Init/dtor/Alloc/Free/Ptr ~412–503;
  grow path (`CalcNewSize`/`CalculatedReserve`/`Realloc`) ~848–894; `EnsureNullTerminator` ~1211.
- `Array.bf` — `Array` base ~8–44; `Array1<T>` ~235–282; `Array2<T>` ~462–505.
- `Span.bf` — `Span<T>` ~6–63.
- `Collections/List.bf` — layout/flags ~38–70; dtor ~109–129; grow (`EnsureCapacity`/`Realloc`) ~333–363, 568–580.
- `Object.bf` — header ~8–15. `Internal.bf` — malloc/free/mem* intrinsics ~112–125; `ObjectAlloc` ~740.
- `Allocator.bf` — `IRawAllocator`/`ITypedAllocator` ~6–14. `GC.bf` — collector (see `GC.md`).

**compiler** (`E:\beef\IDEHelper\Compiler\`):
- `BfModule.cpp` — `AllocBytes` ~9721; `AllocFromType` ~9928 (heap/scope/array sizing ~10200–10485);
  `AppendAllocFromType` ~10698; `EmitAppendAlign` ~10650; `TryConstCalcAppend` ~17761;
  malloc/free builtins ~8395–8439.
- `BfDefBuilder.cpp` — `__CalcAppend` twin + `__appendIdx` synthesis ~2150–2195.
- `BfStmtEvaluator.cpp` — `delete` lowering ~4221.
- `BfExprEvaluator.cpp` — `CreateObject` ~16068; append-at-new-site ~16955; `ResolveAllocTarget` ~17305.
- `BfSystem.h` — `BfObjectFlags` ~270 (`StackAlloc=0x08`, `AppendAlloc=0x10`, `Deleted=0x80`).

---

## 11. See also

`GC.md` — Beef's optional-collector escape hatch (conservative-root + precise-heap marking,
non-relocatable), and the design space for plugging in a real reclaiming collector (e.g. NewGC)
*without* precise root safepoints.
