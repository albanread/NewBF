# NewBF — Manifesto and Declaration of Intent

*Drafted 2026-05-29.*

The commitments behind NewBF. The plan in [`PLAN.md`](PLAN.md) is the
*how*; the sprints in [`SPRINTS.md`](SPRINTS.md) are the *when*; this is
the *what we will not compromise on*.

## What this is

NewBF is a from-scratch **Rust + LLVM implementation of the Beef
programming language** — a JIT-first *and* AOT-native compiler that runs
Beef source on modern 64-bit Windows (then Linux and macOS), with hot
code swapping, manual memory management, debug-time memory safety, and a
graphical IDE built on the portfolio's shared iGui stack.

It is not a port of Beef's *implementation*. It is a re-implementation of
Beef the *language*. We keep the language as
[beeflang.org](https://www.beeflang.org/docs/) defines it — the C#-shaped
syntax, value/reference type split, generics, interfaces, comptime,
reflection, manual memory with custom allocators, and the debug-build
safeties that make manual memory livable. We replace the compiler, the
backend, the runtime, the IDE, and the build story.

The reference tree is the upstream Beef compiler at
[`E:\beef`](file:///E:/beef) (MIT, BeefyTech LLC) — read-only inspiration
for grammar, semantics, the runtime model, and the corlib. We never
modify it.

## The original, and why we are reviving it

Beef is an open-source, performance-oriented compiled language built
hand-in-hand with its IDE, created by Brian Fiete (BeefyTech LLC, 2019).
Its syntax and semantics are most directly derived from C#, while
retaining the C ideals of bare-metal explicitness and no runtime
surprises, with modern niceties borrowed from Rust, Swift, and Go. Its
stated design goal is a fluid, pleasurable development experience for
high-performance real-time software — video games and engines above all.

What makes Beef distinctive, and worth a faithful re-implementation:

- **Manual memory management that is actually livable.** No GC. But the
  debug build detects leaks in real time, and *guarantees* protection
  against use-after-free and double-deletion. These safeties strip out of
  release builds for maximum speed. This is the single most important
  idea to inherit, and it is the exact inverse of the rest of our
  portfolio's tracing-GC bet.
- **Mixed optimization levels.** Optimization is selectable per-type and
  per-method, so performance-critical code runs at full speed inside an
  otherwise-debuggable program, in one binary.
- **Comptime.** Compile-time code execution (the `CeMachine` engine,
  `E:\beef\IDEHelper\Compiler\CeMachine.cpp`) runs Beef at compile time
  for const-eval, type generation, and reflection-driven codegen.
- **Hot code swapping.** The IDE recompiles methods and swaps them into
  the running, debugged process.
- **First-class reflection, custom allocators, append allocations,
  delegates/events, interfaces with default methods, enums with payloads,
  tuples, and built-in unit testing.**

The upstream compiler is a large C++ codebase (`IDEHelper/Compiler`,
`IDEHelper/Backend`, `BeefRT`) with two backends: its own fast x86
backend ("Be", `IDEHelper/Backend/Be*`) for snappy debug builds and an
LLVM backend for optimized release builds. The IDE itself is written *in
Beef*. We keep none of the C++; we keep the language and the corlib
shape.

## Core decisions

NewBF is **manual-memory-first**, **Rust-for-the-substrate**,
**LLVM-only**, **64-bit-first**, **Windows-first**, **JIT-and-AOT**, and
**phase-reportable-by-construction**.

1. **Rust for the native substrate, Beef for the surface.** The lexer,
   parser, type system, comptime engine, IR builder, optimizer, LLVM
   driver, JIT, AOT linker driver, runtime, and FFI plumbing are Rust —
   safe where possible, clearly-scoped `unsafe` where necessary.
   Workspace lint `unsafe_op_in_unsafe_fn = "deny"` is inherited
   portfolio-wide. Everything that is *Beef* — the corlib, the test
   corpus, user programs — is Beef source compiled by our compiler.

2. **No hand-written assembly.** Anything upstream Beef does in its own
   machine-code backend — call frames, stack maps, thunks — is lowered
   through LLVM IR or `core::arch` intrinsics, never `.asm` files.

3. **LLVM is the only code generator.** Beef's own "Be" x86 backend and
   COFF emitter are dropped. The pipeline is Beef → tokens → parse tree →
   reduced AST → resolved/typed AST → NewBF SSA IR → LLVM IR → machine
   code, JIT-first, with a reviewable textual report at every phase
   (core decision 12).

4. **64-bit from day one.** Pointer width, integer sizing, struct layout,
   FFI marshalling, and allocation headers all assume a 64-bit address
   space. No 32-bit build ships.

5. **Windows-first; Linux and macOS second; not Windows-only.** First
   target is `x86_64-pc-windows-msvc`. The OS-specific surface — virtual
   memory for the stomp allocator, threads, the IDE's windowing — lives
   behind thin Rust shims. wasm is explicitly out of scope for v1.

6. **Manual memory management. No garbage collector.** This is the line.
   The heap is explicitly managed: `new` / `delete`, scope-bound `defer
   delete`, append allocations, and first-class custom allocators
   (`new:allocator`). We do **not** consume the portfolio's shared precise
   tracing collector. Detailed in *The memory model* below.

7. **Debug-time memory safety is the signature runtime feature.** Real-
   time leak detection, guaranteed use-after-free protection, and
   guaranteed double-deletion protection — all on by default in debug,
   all strippable in release. This is the manual-memory analogue of the
   portfolio's precise GC: same goal (memory correctness during
   development), opposite mechanism. Detailed below.

8. **JIT-first inner loop *and* first-class AOT.** Both are shipped
   targets, not one deferred behind the other. The JIT drives the REPL,
   hot code swapping, and fast iteration. The AOT path runs the identical
   pipeline minus the live-install step and emits a standalone native
   executable. Beef is fundamentally an AOT language that ships native
   binaries; we honor that and do not relegate AOT to "v2".

9. **Mixed optimization levels survive.** Opt level is selectable per-type
   and per-method and realized through LLVM per-function optimization, so
   Beef's "debuggable program with a few red-hot methods at `-O3`"
   promise is preserved rather than flattened to a whole-program switch.

10. **Comptime is first-class.** A compile-time execution engine
    (`newbf-comptime`, modelling `CeMachine`) evaluates Beef at compile
    time for const-eval, `[Comptime]` methods, and type generation. It is
    a genuine interpreter — the one place in the system that is *not* the
    JIT — because that is what comptime is.

11. **Reflection is first-class.** Type metadata is emitted and queryable
    at runtime, with opt-in/strip controls per type (mirroring Beef's
    `[Reflect]` / always-include / strip policy). The reflection metadata
    a build emits is itself one of the human-reviewable phase reports.

12. **Every compiler phase emits a human-reviewable report, and the
    driver can stop after any phase.** This is a manifesto-level
    commitment, not a debugging convenience. Reports are deterministic,
    schema-stable, and diff-friendly; they are how we know the compiler is
    correct *before* there is an IDE, they gate the test suite, and they
    make every phase auditable in review. Detailed in *Reviewability* below.

13. **The IDE is a Rust iGui application with a UI thread and a language
    thread.** It is built on the portfolio's shared Direct2D/DirectWrite
    iGui stack (WF64 / NodIDE / DocCrate lineage), with the two-thread
    architecture proven in NewFactor. This is a deliberate divergence from
    NewOpenDylan, whose IDE is written in the target language. Detailed in
    *The IDE* below.

14. **No image format on disk.** Source on disk; compile to memory (JIT)
    or to a native binary (AOT); the compiled-artifact cache is
    non-canonical, regenerable, and deletable. Source files are what `git`
    sees.

15. **Open source, MIT, no proprietary inputs.** Implemented against the
    public Beef documentation and the MIT-licensed upstream source tree.
    NewBF ships under `MIT OR Apache-2.0`, compatible with Beef's MIT.

16. **Structured exception handling and rich stack dumps are a key
    requirement.** Win64 SEH is integrated end-to-end: the JIT registers
    unwind info (`RtlAddFunctionTable`, `.pdata`/`.xdata`) so OS-level
    unwinding, attached debuggers, and our own stack walker all see
    correct frames for JIT'd *and* AOT code. On a fault or panic the
    runtime produces a **rich, symbolicated stack dump** — a frame-by-
    frame backtrace with Beef source locations, recovered inlined frames,
    argument/local summaries where available, and the manual-memory
    guard's allocation-site context — written to the log and surfaced in
    the IDE crash view. This is load-bearing *because* the language is
    manual-memory: when the stomp allocator faults on a use-after-free,
    the dump must point at the offending frame and at the allocation and
    free sites of the poisoned object. Detailed in *Crash handling* below.

## The memory model

This section is, for NewBF, what the GC section is for every sibling
project — and it says the opposite thing.

**There is no garbage collector.** Allocation is explicit. The runtime
(`newbf-runtime`, pure Rust over a thin virtual-memory shim) provides:

- **`new` / `delete` with allocator awareness.** `delete obj` runs the
  destructor (`~this`) and frees through the owning allocator.
  `delete:null`, `new:allocator`, and `append` allocations
  (`[AllowAppend]`, trailing inline storage) are honored exactly as in
  `E:\beef\BeefLibs\corlib\src\Allocator.bf`.
- **Custom allocators are first-class**, exposed through the corlib
  `IRawAllocator` / `ITypedAllocator` interfaces and a bump allocator,
  not bolted on.

Three debug-default, release-strippable safety mechanisms — the port of
`E:\beef\BeefRT\rt\StompAlloc.cpp` and the BeefRT object bookkeeping:

- **Stomp allocator.** Each allocation is placed on its own guard
  page(s); freeing unmaps/poisons the pages. A use-after-free or
  out-of-bounds access then faults *deterministically at the offending
  instruction*, not later and elsewhere. This is the guarantee, not a
  heuristic.
- **Allocation ledger / leak reporter.** Every live allocation is tracked
  with its allocation site. On shutdown and on demand, unfreed
  allocations are reported by type and site — leaks surface in real time,
  not in a profiler weeks later.
- **Double-free / ownership guard.** Freed objects are marked; a second
  `delete` is caught and reported rather than corrupting the heap.

All three compile out of release builds, where allocation falls through
to a fast allocator (tcmalloc/jemalloc-class, as BeefRT bundles) for
maximum speed. This is core decision 7 made concrete.

**Beef's optional conservative GC** (`E:\beef\BeefLibs\corlib\src\GC.bf`)
is a documented, opt-in, secondary build mode for a later sprint. It is
not the default and is never load-bearing. NewBF is a manual-memory
language that happens to offer a GC mode, exactly as Beef is.

**What we still share with the portfolio.** Dropping the collector does
*not* mean dropping the shared substrate. We still lift the JIT code-
memory manager and Win64 SEH registration from NewM2, the loader/module-
graph shape from NCL/NewCP, the Windows FFI stack from NCL, the LLVM pin,
and every convention below. We share everything *except* the object-heap
collector — because Beef does not have one.

## Crash handling: SEH, unwinding, and stack dumps

Core decision 16, made concrete. For a manual-memory language the crash
report *is* the debugger of first resort, so this is a first-class
requirement, not a logging afterthought.

- **Win64 SEH, end to end.** Every JIT'd and AOT-compiled function carries
  unwind info. The JIT registers it with `RtlAddFunctionTable` (the
  mechanism lifted from NewM2's JIT memory manager); AOT writes ordinary
  `.pdata`/`.xdata`. The result: the OS unwinder, an attached debugger
  (WinDbg/Visual Studio), and our own stack walker agree on every frame.
- **A precise stack walker.** On a fault or panic the runtime walks the
  stack and maps each return address to a Beef source location through the
  same line tables that drive the debugger, recovering **inlined frames**
  rather than collapsing them, and summarising arguments/locals where
  layout permits.
- **Rich, symbolicated dumps.** A dump is a readable backtrace —
  `function (file:line)` per frame, inlined frames marked — plus, uniquely
  for NewBF, **the manual-memory guard's context**: on a stomp
  use-after-free fault, the allocation site *and* the free site of the
  poisoned object; on a leak, the allocation sites; on a double-free, both
  delete sites. The dump is itself one of the human-reviewable reports
  (core decision 12).
- **Unwinding runs `defer` and destructors.** The unwind path executes
  scope-bound `defer` blocks and `~this` destructors in order, so manual
  cleanup still happens on the way out of a faulting scope where it safely
  can.
- **The IDE consumes the dump.** The three-level supervisor recovery (SEH
  crash → dump → respawn) in *The IDE* below renders these dumps in a
  crash view with clickable source frames; headless builds write them to
  the log.

The stack walker + SEH registration live in `newbf-runtime` (with the JIT
registration path shared with `newbf-llvm`); the OS-specific unwind calls
sit behind the same thin shim as the rest of the runtime.

## Hot code swapping and live evaluation

The interactive premise, inherited from Beef's IDE and the portfolio's
JIT-first stance:

- **Recompile-and-swap per definition.** Changing one method recompiles
  that method and installs the new body into the running process under
  the loader's generation discipline (modelled on NCL's). The old body
  retires once no live frame can reach it.
- **The REPL is the JIT.** Evaluating an expression builds the same IR a
  compiled method would and runs through the same codegen path. There is
  no separate interpreter — except comptime, which genuinely is one.
- **AOT is the same pipeline.** The standalone-executable path differs
  only in the final step: emit object code and link, instead of
  installing into the live image. JIT and AOT share lexer, parser, sema,
  comptime, IR, optimizer, and LLVM lowering byte-for-byte.

## Reviewability — reports at every phase

Core decision 12, made concrete. The convention is NewM2's
(`format_*` dumps; the driver can stop after any phase) and we treat it as
load-bearing.

Each phase crate exposes report producers; `newbf-driver` exposes a
`dump-<phase>` subcommand per phase and an `--emit-reports <dir>` mode
that writes them all. Reports are deterministic and schema-stable so they
can be checked into fixtures and diffed in review.

| Phase            | Crate            | Report(s) suitable for human review                                   |
| ---------------- | ---------------- | --------------------------------------------------------------------- |
| lex              | `newbf-lexer`    | token stream with spans                                               |
| parse            | `newbf-parser`   | parse-tree dump; then reduced-AST dump                                |
| sema             | `newbf-sema`     | name-resolution, type/def table, generic-instantiation, dispatch, definite-assignment + delete-flow (ownership) reports |
| comptime         | `newbf-comptime` | comptime evaluation trace                                             |
| ir               | `newbf-ir`       | NewBF SSA IR dump                                                      |
| llvm             | `newbf-llvm`     | LLVM IR dump; per-method opt-level (mixed-optimization) report         |
| codegen          | `newbf-llvm`     | object/asm dump; emitted reflection-metadata report                   |
| runtime          | `newbf-runtime`  | leak report; live-allocation report (runtime artifacts, same format)   |

These reports are the substitute for an eat-our-own-dogfood IDE during
the headless phases (Sprints 01–N): `cargo test` plus a stable report
diff is how we know we are alive.

## The IDE

Core decision 13, made concrete — and the clearest place NewBF diverges
from NewOpenDylan.

The IDE is a **Rust application** built on a **vendored copy** of the
portfolio's iGui front-end (Direct2D / DirectWrite MDI) — forked from
WF64's IDE / NodIDE / DocCrate lineage and **owned in this repository**
(`NewBF/crates/{igui,docpane,selkie,doc-crate}`), *not* a path-linked
crate pointing at WF64. It is **not** written in Beef. (Beef's own IDE
*is* written in Beef; we deliberately choose the Rust iGui stack instead,
so the IDE shell is shared in *lineage* across the portfolio — but each
project owns its vendored copy and is free to diverge it.)

The architecture is NewFactor's, proven in
`E:\NewFactor\src\bin\newfactor_ui.rs` — one Windows process, three
cooperating threads:

```text
newbf-ide.exe  (one Windows process)
├── GUI thread        Direct2D MDI, Win32 message pump (igui)
│     ↕ IGuiEvent MPSC channel
├── IDE worker        receives events, drives the Session
│     ↕ Command / EvalResult channels
└── language worker   owns the NewBF compiler + JIT + manual-memory runtime
      eval output → write-char callback → GUI console pane
```

- **GUI thread**: owns the iGui MDI frame, the Win32 message pump, the
  frame palette, and the crash handler. Vendored into this repo as our
  own copy — we are free to edit it.
- **Language worker thread**: owns a `Session` — the compiler, the JIT,
  and the manual-memory runtime with its debug guard. Compilation and
  evaluation never block the GUI.
- **Supervisor**: wraps the worker in `catch_unwind` + an SEH crash
  handler, with three-level recovery — SEH crash → dump → respawn worker;
  Rust panic → report → respawn; `Session` dies → drop and recreate,
  keep going. State that can persist across a respawn does.
- **Interrupt hook**: the GUI thread can abort a long-running eval
  (Ctrl+B / Break) without routing through the worker's event queue,
  which is blocked inside the eval at exactly that moment.
- **Panes**: an embedded DocCrate doc browser (`doc-crate` / `docpane`)
  for language and corlib documentation, and the `selkie` editor widget,
  alongside the console, the inspector, the leak/allocation report view,
  and the phase-report viewer (the dumps above, shown live).

**Compiler-first.** No IDE until the compiler can JIT and run non-trivial
Beef. Sprints 01–N are headless: `newbf-driver` `dump-*` subcommands and
`cargo test` are how we know we are alive. The IDE crate is scaffolded as
a stub from the start so the architecture is materialized, but the igui
wiring is activated at its own sprint.

## Reuse across the sibling-compiler portfolio

NewBF is the latest project in a portfolio of from-scratch Rust+LLVM
language implementations that share infrastructure and conventions:

| Project        | Language                | Shares with NewBF                              |
| -------------- | ----------------------- | ---------------------------------------------- |
| NewM2          | Modula-2                | JIT memory manager + Win64 SEH; phase-report convention |
| NewCP          | Component Pascal        | loader / module-graph shape                    |
| NewCormanLisp  | Common Lisp             | Windows FFI stack; loader generations          |
| NewBCPL        | BCPL                    | workspace + lint conventions                   |
| NewFB          | FreeBASIC               | workspace + lint conventions                   |
| NewOpenDylan   | Dylan                   | manifesto + sprint conventions                 |
| NewFactor      | Forth/Factor            | **the iGui two-thread IDE harness**            |
| WF64 / NodIDE  | (tooling)               | **iGui (D2D/DWrite MDI), DocCrate doc pane, selkie editor** — vendored, not linked |

What we lift, with attribution:

- **JIT code-memory manager + SEH integration** — from NewM2's
  `newm2-llvm` JIT memory manager. Win64 `RtlAddFunctionTable`
  registration is solved once across the portfolio.
- **Loader / module-graph crate shape** — from NCL/NewCP: source-stamp
  invalidation, generation-pinned execution scopes, retired artifacts.
  This is what makes hot code swapping tractable.
- **Windows FFI stack** — from NCL's `win_ffi` / `win_callback` /
  `win_buffer` family: the calling-convention dispatcher, callback
  bridge, and buffer marshalling — the *machinery* of making a foreign
  call, used for Beef's `[CallingConvention]` / `[Import]` interop and the
  runtime's OS calls.
- **Win32 API metadata** — the API *surface* (constants and function
  signatures) is vendored from the shared
  [`E:\windows_api`](file:///E:/windows_api) repository (a SQLite
  `windows_api.db` + zstd `.pack`, derived from Win32 metadata) through a
  `newbf-winapi` crate whose `build.rs` reads a vendored snapshot in
  `data/windows_api.db` and emits a postcard+zstd blob with a runtime
  lookup — exactly as NewOpenDylan's `nod-winapi` does. The metadata
  repository is consumed read-only; we never modify it.
- **iGui + DocCrate + selkie + the two-thread IDE harness** — forked from
  WF64 and NewFactor and **vendored into `NewBF/crates/`** as our own
  owned copies (the self-contained `igui` → `docpane` → `selkie` set, plus
  `doc-crate`), *not* path-linked back to WF64. We are free to diverge
  them.
- **LLVM pin and `inkwell` version** — pinned to the portfolio major
  (currently LLVM 22.1 via `inkwell = "=0.9.0"`, `llvm-sys = "=221.0.1"`).
  Bumps are coordinated.
- **Phase-report visibility convention** — NewM2 set it; core decision 12
  makes it a NewBF commitment.
- **Test-harness convention** — runnable Beef alongside a Rust harness in
  `tests/` that drives the full pipeline and captures reports.
- **`unsafe_op_in_unsafe_fn = "deny"`** workspace lint.
- **Manifesto-as-design-constraint convention** — this document.

We do **not** share the AST, the IR opcodes, sema, or — uniquely among
the siblings — **the garbage collector**. Beef does not have one, so
neither does NewBF.

## Bootstrap

NewBF does **not** self-host, and neither does upstream Beef (its compiler
is C++; only its IDE is Beef). The NewBF compiler stays in Rust
permanently. The corlib (`E:\beef\BeefLibs\corlib\src\`) is ported as
*runnable Beef source* compiled by our compiler — not as compiler
bootstrap. The IDE is Rust (shared iGui), so it introduces no bootstrap
dependency on a working Beef compiler. We never consume Beef's IL,
`.bfbf`, or build artifacts.

## What NewBF is *not*

- **Not the Beef implementation.** We share the language, the MIT
  license, and the test spirit. We share no code with `IDEHelper`, the
  "Be" backend, `BeefRT` (C++), `BeefBoot`, or `BeefBuild`.
- **Not 32-bit.**
- **Not garbage-collected by default.** Faithful to Beef: manual memory,
  with an optional GC mode offered later, never as the default.
- **Not using Beef's own backend.** LLVM is the only code generator.
- **Not self-hosting.** Compiler in Rust, corlib in Beef.
- **Not an IDE written in Beef.** The IDE is a Rust iGui application — a
  deliberate divergence from NewOpenDylan, chosen so the IDE shell is
  shared across the portfolio.
- **Not a language-design vehicle.** We implement Beef as the upstream
  docs and source define it, not a dialect.

## Versioning policy

- **Rust:** stable channel; MSRV pinned in `Cargo.toml`, bumped quarterly
  across the portfolio.
- **LLVM:** pinned to the portfolio major (22.1), same as NewM2 / NCL.
  Bumps are coordinated and tracked in [`PLAN.md`](PLAN.md).
- **Beef language version:** tracked to the cloned `E:\beef` snapshot at
  the time of each milestone.
- **Cache key:** `(source hash, compiler version, codegen flags, opt
  level, LLVM version)`. Any component change invalidates the cache.

---

*This manifesto is committed ahead of code. The plan in
[`PLAN.md`](PLAN.md) is the schedule; this is the line we will not move.*
