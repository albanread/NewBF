# NewBF

A from-scratch **Rust + LLVM** compiler for the
[Beef programming language](https://www.beeflang.org/) — JIT-first for the inner
loop (REPL, hot reload), AOT-native for shipping, with **manual memory
management and no GC** (Beef's defining trait). NewBF re-implements the
*language*, not Beef's C++ compiler.

It's the manual-memory member of a portfolio of from-scratch Rust+LLVM language
implementations.

## Status

A working compiler. The full pipeline — lexer → parser → semantic analysis →
typed SSA IR → LLVM 22 → **ORC JIT** *and* **AOT `.exe`** — compiles and runs
real Beef:

- primitive types, expressions, and Beef operator precedence;
- full control flow: `if` / `while` / `for` / do-while / `switch`, `break` / `continue`;
- intra-program calls, direct and mutual recursion;
- **value `struct`s** — fields, member access, nested aggregates;
- **heap `class`es** with manual `new` / `delete` (no GC), reference fields, chained access;
- **constructors + destructors** and **instance methods** (`obj.Method()`, `this`);
- Win32 FFI through an embedded ABI oracle; SEH crash dumps with symbolicated
  stack traces; a compile-time const-evaluator.

Behaviour is checked by a JIT-and-run corpus — compile each program, run it,
assert its result — alongside a curated feature corpus that verifies clean under
the LLVM verifier. The reasoning behind each step is logged in
[`NewBF/docs/journals/`](NewBF/docs/journals).

Not yet: indexing / arrays, generics, inheritance / virtual dispatch, the
standard library. The *optional* GC direction (conservative roots + precise heap
via the sibling **NewGC** collector — no safepoints) is designed in
[`NewBF/docs/GC.md`](NewBF/docs/GC.md).

## Building

The Cargo workspace lives in [`NewBF/`](NewBF). You need a **Rust** toolchain and
**LLVM 22.1**. Currently **Windows x86-64 (MSVC)** only — the runtime uses Win64
SEH and links via `link.exe`; Linux/macOS ports come later.

1. Install LLVM 22.1 (a prebuilt release or your own build) and point `llvm-sys`
   at its prefix:

   ```
   set LLVM_SYS_221_PREFIX=C:\path\to\llvm-22.1
   ```

2. Build, test, and run from the workspace directory:

   ```
   cd NewBF
   cargo build --workspace
   cargo test  --workspace
   cargo run -p newbf-driver -- --version
   ```

The `newbf-winapi` crate embeds a committed Win32 ABI snapshot, so **no external
data is required** to build. (To refresh that snapshot from a Win32-metadata
SQLite DB, set `NEWBF_WINDOWS_API_DB` — see
[`NewBF/src/newbf-winapi/data/README.md`](NewBF/src/newbf-winapi/data/README.md).)

## Map

| Path | What |
| --- | --- |
| [`MANIFESTO.md`](MANIFESTO.md) | Pinned design commitments. |
| [`PLAN.md`](PLAN.md) | Language survey + 12-phase plan. |
| [`SPRINTS.md`](SPRINTS.md) | Sprint schedule. |
| [`NewBF/`](NewBF) | The Rust workspace ([crate map + details](NewBF/README.md)). |
| [`NewBF/docs/`](NewBF/docs) | Memory model, core types, the GC direction, IR, comptime, and the engineering [journal](NewBF/docs/journals). |
| [`beef-tests/`](beef-tests) | Curated Beef corpus + the JIT-run programs. |

## License

`MIT OR Apache-2.0`, compatible with upstream Beef's MIT license.
