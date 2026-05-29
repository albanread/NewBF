//! `newbf-runtime` — the NewBF runtime. **Manual memory, no GC.**
//!
//! This is the inverse of the rest of the portfolio's tracing-GC bet,
//! and it is Beef's signature. The runtime provides:
//!
//!   - **An explicit heap** — `new`/`delete`, `scope` (scope-bound)
//!     allocations, allocator-qualified alloc/free, and append
//!     allocations, with first-class custom allocators.
//!   - **The debug memory guard** (on by default in debug, stripped in
//!     release):
//!       * **stomp allocator** — guard-page-per-allocation, unmap on
//!         free, so use-after-free / overrun faults deterministically at
//!         the offending access (port of `E:\beef\BeefRT\rt\
//!         StompAlloc.cpp`);
//!       * **leak ledger** — per-allocation site tracking + a real-time
//!         leak report;
//!       * **double-free guard** — freed objects are marked and a second
//!         delete is caught.
//!   - **Reflection metadata** emission/lookup, and the **FFI machinery**
//!     (calling-convention dispatcher, callback bridge, buffer
//!     marshalling) lifted from NewCormanLisp.
//!
//! The only OS dependency is a virtual-memory + threads shim (the stomp
//! allocator needs guard pages). Release builds fall through to a fast
//! allocator. Beef's optional conservative GC (`corlib GC.bf`) is a later,
//! opt-in mode — never the default.
//!
//! Lands in SPRINTS.md Sprints 09–11. Reference: `E:\beef\BeefRT\rt\`.
