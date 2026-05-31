# NewBF Standard Library — Organization & Plan

> Status: design note. Date: 2026-05-31.
> Companions: `CORETYPES.md` (how the core types are implemented), `GC.md` (the
> optional-GC `System.GC` stub), the engineering journal.
> This is the "pause for breath" before porting the real corlib: *how* the
> standard library is structured, compiled, layered, and phased.

So far we've validated the language with prototype classes written *inside*
corpus programs (`MiniString`, `Str`). The standard library is the real thing:
shared `.bf` source, compiled by our own compiler, that every program can use.
This note decides its shape before we start porting.

---

## 0. Two "corlibs" — don't confuse them

- **`beef-tests/corlib-slice/`** — 89 upstream Beef `.bf` files (`String.bf`,
  `Array.bf`, `Console.bf`, …), MIT, **read-only reference fixtures** used by the
  lexer/parser corpus. This is what we port *from* and check fidelity against.
- **`NewBF/src/newbf-corlib/bf/`** — **our** standard library: the subset we can
  actually compile and run, simplified where needed. This is what we *write*.

The slice is the spec; `newbf-corlib/bf` is the implementation.

---

## 1. The compilation model: a source prelude, composed at the AST

**Decision:** the stdlib is **`.bf` source prepended to the user's program**, not
a precompiled binary. Each program is compiled as: *corlib ASTs + user ASTs →
one module → lower once → JIT/AOT*.

Why:
- It matches the pinned principle *"compose source at the AST, not after
  lowering"* (MEMORY): parse each file, concatenate ASTs into ONE module, lower
  once. Per-file lowering then stitching would drop fields and invalidate
  per-module indices.
- It's the natural fit for **JIT-first** + whole-program lowering (the corpus
  harness already lowers a whole program at once).
- No separate-compilation ABI, no link step for the stdlib, no version skew.

The cost — recompiling the stdlib per program — is negligible at corlib's size
and trivially cached later (parse once, reuse the AST; or memoize the lowered
prelude module). Start simple: recompile.

**Mechanism (the `newbf-corlib` shim):** embed each `bf/*.bf` via `include_str!`
into an ordered `&[(name, source)]`. The driver/sema take that prelude, parse it,
and prepend its items before the user's. Embedding (not path-loading) keeps the
JIT + corpus self-contained — no filesystem assumptions, mirrors how
`newbf-winapi` embeds its snapshot.

```
newbf-corlib/
  bf/                 the stdlib source (System.*)
    Internal.bf       intrinsic/FFI floor
    Object.bf
    String.bf
    Console.bf
    ...
  src/lib.rs          pub fn prelude() -> &'static [(&str, &str)]  (include_str!)
```

Open: **always-prelude vs. opt-in.** Beef makes corlib implicit. Simplest:
always prepend the prelude (every program "sees" `System`). `using System;`
becomes a no-op resolution convenience. Revisit if it bloats tiny programs.

---

## 2. Layering (bottom-up)

Each layer depends only on those below, so we can bring them up in order:

1. **`Internal`** — the intrinsic/FFI floor: `Malloc`/`Free`/`MemCpy`/`MemSet`,
   `CStrLen`, the allocation bottom. These are `[Intrinsic]`/`[LinkName]` extern
   methods bound to real symbols (malloc/free/memcpy). Everything else allocates
   through here. *(Gating feature: extern-method binding + qualified static
   calls — see §4.)*
2. **`Object` + primitives** — the `Object` root (the `ClassVData*` header from
   `CORETYPES.md`), and methods on the primitive types (`int.ToString`, etc.).
3. **`String`** — the separate-buffer String (Milestone A), then append-alloc
   fidelity (Milestone B). The flagship type; see `CORETYPES.md §2`.
4. **Collections** — `Span<T>`, `List<T>`, `Array`. *(Gating: generics.)*
5. **IO** — `Console` (write via the Win32 FFI we already have).
6. **`System.GC`** — the inert stub (no collector by default), exactly as Beef
   does in release; see `GC.md`. Present so corlib that references `GC.Mark`
   compiles; does nothing unless we wire NewGC later.

---

## 3. Faithfulness to Beef

- **Namespaces + names match upstream** (`System.String`, `System.Collections.List<T>`)
  so ported code and user `using System;` line up, and the corlib-slice is a
  drop-in reference. (Mangling caveat in §4.)
- **Port, then simplify.** Start from the slice file, strip what we can't yet
  compile (append allocation, complex generics, attributes we ignore), keep the
  shape and the public surface. Mark divergences in-file.
- **Milestone A vs B** (`CORETYPES.md §7`): A = separate-buffer `String`/`List`
  (no `[AllowAppend]`); B = append-alloc for byte-identical layout/SSO. Ship A
  first — it only needs features we have or are adding.

---

## 4. Compiler features the stdlib forces (the gating list)

Writing real corlib turns "nice to have" into "blocking." Roughly in order:

| Feature | Needed by | Status |
| --- | --- | --- |
| **Qualified static calls** (`Type.Method()`) | `Internal.MemCpy`, `Math.Abs` | not yet |
| **`[Intrinsic]`/`[LinkName]` extern binding** | `Internal.*` → real symbols | not yet |
| **String literals → `String` objects** | `String s = "hi"` | not yet (`"…"` is a `char8*`) |
| **Generics / monomorphization** | `List<T>`, `Span<T>` | not yet |
| **Properties** (`get`/`set`) | `String.Length`, most types | not yet |
| **Operator methods** (`==`, `[]` indexer) | `String`, collections | not yet |
| **Overload by type** (not just arity) | many corlib methods | arity-only today |
| classes, ctors/dtors, methods, field-pointer indexing, char literals | the whole tower | **done** |

This table *is* the corlib roadmap: each row unblocks more of the slice. The
first three are the gate to a first-class `String`; generics is the gate to
collections.

---

## 5. Open decisions

- **Namespace mangling.** We currently mangle by *type nesting* only; namespaces
  pass through, so `System.String` mangles as `String.`. Fine while there's one
  `String` (first-wins), but a user `class String` would collide. Decision:
  accept first-wins for now; namespace-qualify mangled names when it bites.
- **Prelude caching.** Recompile per program now; memoize the parsed prelude (or
  the lowered prelude module) once the stdlib grows.
- **What's the minimum first slice?** `Internal` + `Object` + a real `String` +
  `Console.WriteLine` — enough to write "hello world" through the *stdlib* rather
  than a raw `puts`.
- **Error/Result types, `defer`, `scope`** — needed by realistic corlib; schedule
  with the breadth features.

---

## 6. Next steps

1. **Prelude plumbing** (small): `newbf-corlib::prelude()` + sema/driver prepend
   its parsed items before user code. Gate: a trivial `bf/Probe.bf` type is
   callable from a corpus program with no local definition.
2. **Qualified static calls** + **extern binding** → stand up `Internal`.
3. **String-literals-as-`String`** → a real `System.String` (Milestone A) the
   corpus can use as `String s = "hi"; s.Length`.
4. **`Console.WriteLine`** over the Win32 FFI → "hello world" through the stdlib.
5. Then generics → `List<T>`; properties/operators → ergonomic corlib.

The throughline: we've proven the *shapes* run (`Str`, `MiniString`). The stdlib
is wiring a shared, faithful version of those shapes into every program — and the
gating-features table is the order we unlock it.
