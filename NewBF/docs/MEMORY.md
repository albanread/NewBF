# Memory model

NewBF is **manual-memory, no GC** — the inverse of the rest of the
portfolio. The heap is explicit: `new`/`delete`, `scope` (scope-bound)
allocations, allocator-qualified alloc/free, append allocations, and
first-class custom allocators.

The signature runtime feature is the **debug memory guard**, on by default
in debug and stripped in release:

- **Stomp allocator** — guard-page-per-allocation, unmap on free, so
  use-after-free and out-of-bounds accesses fault deterministically at the
  offending instruction (port of `E:\beef\BeefRT\rt\StompAlloc.cpp`).
- **Leak ledger** — per-allocation site tracking + a real-time leak report.
- **Double-free guard** — freed objects are marked; a second delete is
  caught.

Release builds fall through to a fast allocator. Beef's optional
conservative GC (`corlib GC.bf`) is a later, opt-in mode, never the
default. Implemented in `newbf-runtime`. See MANIFESTO core decisions 6–7.
