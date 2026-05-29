# NewBF — Language Report and Implementation Plan

*Drafted 2026-05-29.*

Two halves. **Part 1** surveys the Beef language and its upstream
implementation, grounded in the source tree at
[`E:\beef`](file:///E:/beef) (MIT, BeefyTech LLC). **Part 2** is a
concrete plan for **NewBF**, a from-scratch Rust + LLVM compiler — JIT-
first and AOT-native — that runs Beef source, modelled on the shape of
the sibling-compiler portfolio (NewM2, NewCormanLisp, NewOpenDylan,
NewFactor).

Testing policy: when a test claims to validate the NewBF compiler, JIT,
runtime, or AOT output, the substantive workload must be *Beef* source.
Rust may orchestrate the test and capture the phase reports, but it must
not stand in for the Beef-side computation being validated.

---

## Part 1 — Beef: the language and the implementation

### 1.1 Heritage and identity

Beef is an open-source, performance-oriented, statically-typed compiled
language created by Brian Fiete (BeefyTech LLC) and released in 2019. Its
syntax and most semantics are derived from **C#**, while it deliberately
keeps the C ideals of bare-metal explicitness and "no runtime surprises,"
and borrows modern niceties from Rust (`defer`, value semantics, `mut`),
Swift, and Go. Its design goal (`README.md`) is a fluid development
experience for high-performance real-time software — games and engines.

Three things make Beef genuinely distinctive, and they are the reasons it
is worth a faithful re-implementation:

1. **Manual memory with debug-time safety.** No GC; `new`/`delete`,
   `scope` allocations, custom allocators — but the debug build detects
   leaks in real time and *guarantees* protection against use-after-free
   and double-deletion.
2. **Mixed optimization levels** per-type and per-method, in one binary.
3. **Comptime** — running Beef at compile time — and **hot code
   swapping** — recompiling methods into a running, debugged process.

The implementation is a large C++ codebase. The compiler proper is
`E:\beef\IDEHelper\Compiler\`; the runtime is `E:\beef\BeefRT\`; the
standard library (`corlib`) is written in Beef at
`E:\beef\BeefLibs\corlib\src\`; the IDE is itself written in Beef.

### 1.2 Type system

Beef has a C#-shaped nominal type system with a hard **value / reference**
split:

- **Value types** — `struct` (e.g. `StdAllocator` in
  `corlib/src/Allocator.bf`). Stored inline, copied by value, no heap
  identity. Primitives (`int`, `int32`, `uint8`, `float`, `double`,
  `char8`/`char16`/`char32`, `bool`) are value types, defined in corlib
  (`Int32.bf`, `Float.bf`, `Char16.bf`, …).
- **Reference types** — `class` (e.g. `SingleAllocator`). Heap-allocated,
  reference identity, single inheritance + interfaces, virtual dispatch.
- **Interfaces** with method signatures, default methods, and `mut`
  methods (`interface IRawAllocator { void* Alloc(int, int) mut; }`).
- **Enums**, including payload-carrying enums (tagged unions, Rust/Swift-
  style) in addition to C-style.
- **Tuples** (`Tuple.bf`), **nullable** value types (`Nullable.bf`),
  **`Result<T>` / `Result<T, E>`** (`Result.bf`), **`Variant`**
  (`Variant.bf`), `Span<T>` (`Span.bf`), `SizedArray` (`SizedArray.bf`).
- **Pointers** (`void*`, `T*`), **function pointers**, and **delegates**
  (`Delegate.bf`) + **events** (`Event.bf`).
- The root types are `Object` (`Object.bf`) for reference types and
  `ValueType` (`ValueType.bf`) for value types; `Type` (`Type.bf`) is the
  reflected metatype.

### 1.3 Memory model — the heart of the language

This is the single most important section, and it is the exact inverse of
the rest of our portfolio's tracing-GC bet.

**Allocation is explicit.** The primitives, seen throughout
`corlib/src/Allocator.bf`:

- `new T(...)` / `delete obj` — heap allocate / free (running `~this`).
- `new:allocator T(...)` and `delete:null obj` — allocator-qualified
  allocate / free.
- **`scope` allocations** — `scope T(...)` allocates with the lifetime of
  the enclosing scope and is freed automatically at scope exit; `scope::`
  targets an outer scope. This is how Beef makes manual memory ergonomic
  without a GC.
- **Append allocations** — `[AllowAppend]` constructors with `append`
  storage place trailing data inline with the object in a single
  allocation (`append uint8[size]*(?)`).
- **Custom allocators are first-class** — `IRawAllocator` /
  `ITypedAllocator` interfaces, `BumpAllocator.bf`, etc.
- `defer` / `defer delete` run cleanup at scope exit.

**Debug-time safety** is the runtime's signature, implemented in
`E:\beef\BeefRT\rt\`:

- **`StompAlloc.cpp` / `StompAlloc.h`** — the "stomp" allocator places
  allocations on dedicated pages and unmaps them on free, so a
  use-after-free or out-of-bounds access faults *deterministically* at
  the offending instruction. This is Beef's guaranteed UAF/overrun
  protection.
- Object bookkeeping in `BfObjects.h` / `Object.cpp` carries the per-
  object flags used for leak tracking and double-free detection.
- All of it strips out of release builds, which use a fast allocator —
  BeefRT bundles `TCMalloc`, `JEMalloc`, and `gperftools` under
  `BeefRT/`.

Beef *also* ships an **optional conservative GC** (`corlib/src/GC.bf`),
but it is opt-in and not the default; manual memory is the language.

### 1.4 Generics

C#-shaped generics that instantiate more like C++ templates:

- `class List<T>`, `where T : IComparable, class, struct, delete` and
  similar constraints.
- **Const generics** (`int` and other const parameters), **specialized
  instantiation** (each concrete `T` is monomorphized), and generic
  methods.
- Generic interfaces and generic constraints participate in dispatch
  resolution. The upstream resolver is the giant
  `BfModule.cpp` / `BfExprEvaluator.cpp` / `BfModuleTypeUtils.cpp`.

### 1.5 Interfaces, dispatch, and operators

- Interfaces may carry **default method bodies** and **static methods**.
- Classes use single inheritance + interfaces; dispatch is via vtables.
  Value types are monomorphized; interface dispatch on a struct may box
  or use a constrained/static call.
- **Operator overloading**, **properties** (get/set), **indexers**, and
  **method references** (`MethodReference.bf`).
- Name mangling and demangling for the ABI live in `BfMangler.cpp` /
  `BfDemangler.cpp`.

### 1.6 Comptime

Beef can execute Beef at **compile time**:

- The engine is `CeMachine.cpp` (with `CeDebugger.cpp`) — a bytecode
  interpreter that runs `[Comptime]` methods, const-evaluates
  expressions, and generates types/members during compilation.
- Comptime drives const folding, static configuration, and reflection-
  aware code generation. It is a genuine interpreter — the one part of
  the system that is not the native backend.

### 1.7 Reflection

- `Type.bf` and the `System.Reflection` namespace expose runtime type
  information: fields, methods, attributes, generic args.
- Reflection metadata emission is **policy-controlled** (`[Reflect]`,
  always-include, or strip) so release builds pay only for what they use.
- Reflection underpins dynamic dispatch, serialization, and the IDE's
  type inspection.

### 1.8 Control flow, statements, error handling

- `if` / `while` / `for` / `do` / `repeat` / `switch` (with pattern
  matching and payload binding on enums), `break` / `continue` with
  labels.
- **`defer`** (run at scope exit), **`using`**, **`mixin`** (hygienic
  statement/expression splices), scope blocks.
- Error handling via **`Result<T>` + `Try!`** (a mixin that early-returns
  the error), not exceptions in the C++ sense, though Beef has runtime
  fatal-error/`Runtime.FatalError` paths.

### 1.9 The compiler (IDEHelper/Compiler)

Upstream Beef's compiler is C++. Its stages, by file:

1. **`BfParser.cpp`** — lexing + parsing into a concrete syntax tree
   (`BfAst.h` / `BfAst.cpp`).
2. **`BfReducer.cpp`** — "reduces" the parse tree into the working AST.
3. **`BfDefBuilder.cpp`** — walks the AST to build *definitions*
   (`BfSystem.cpp`: projects, namespaces, type/method/field defs).
4. **`BfModule.cpp` (970 KB), `BfExprEvaluator.cpp` (935 KB),
   `BfStmtEvaluator.cpp`, `BfModuleTypeUtils.cpp` (583 KB),
   `BfResolvedTypeUtils.cpp`** — the semantic core: name resolution,
   type resolution, generic instantiation, dispatch, definite-assignment,
   the manual-memory ownership checks, and lowering to IR. This is the
   bulk of the compiler.
5. **`CeMachine.cpp`** — the comptime interpreter, invoked from the
   semantic core.
6. **`BfIRBuilder.cpp` / `BfIRBuilder.h`** — builds **Beef IR** (an
   SSA-shaped IR independent of backend).
7. **`BfIRCodeGen.cpp`** — lowers Beef IR to the chosen backend.
8. Back end, two choices:
   - **The "Be" backend** (`E:\beef\IDEHelper\Backend\Be*`):
     `BeModule` / `BeContext` / `BeMCContext` / `BeMCX86.h` — Beef's own
     fast x86 machine-code generator, with `BeCOFFObject.cpp` for object
     emission and `BeLibManger` for linking. Used for fast debug builds.
   - **LLVM** — the same Beef IR lowered to LLVM for optimized release
     builds.
9. `BfCompiler.cpp` orchestrates the whole pipeline; `BfAutoComplete.cpp`
   and `BfPrinter.cpp` serve the IDE (completion, reformatting);
   `BfContext.cpp` holds compile state.

`BeefBoot` (`E:\beef\BeefBoot\`) is the bootstrap compiler that builds
corlib headlessly; `BeefBuild` is the command-line build tool.

### 1.10 The IR

Beef IR (built by `BfIRBuilder`) is an SSA-shaped intermediate
representation with explicit value, type, and instruction nodes,
deliberately backend-independent so the same IR feeds either the "Be"
backend or LLVM. **NewBF lifts this two-layer shape** — a typed mid-level
IR that then lowers to LLVM — but targets LLVM only.

### 1.11 Runtime model (BeefRT)

`E:\beef\BeefRT\rt\` is the C++ runtime:

- `BfObjects.h`, `Object.cpp` — object header/layout, the root `Object`.
- `StompAlloc.cpp/.h` — the debug stomp allocator (§1.3).
- `Thread.cpp`, `ThreadLocalStorage.cpp` — threading + TLS.
- `Internal.cpp` — runtime intrinsics (`Internal.StdMalloc`,
  `UnsafeCastToObject`, …) that corlib calls.
- `Math.cpp`, `Chars.cpp`, `Test.cpp` — math, char tables, the unit-test
  runner.

The low-level OS abstraction (`Bfp*` — file IO, threads, process spawn,
sync) lives in `BeefySysLib` (`PlatformInterface.h`), per the upstream
`CLAUDE.md`. `MinRT` is a minimal runtime variant.

### 1.12 The standard library (corlib)

`E:\beef\BeefLibs\corlib\src\` is the Beef-side stdlib, written in Beef,
in the `System` namespace: `Object`, `ValueType`, `Type`, `String`,
`Array`, `Span`, `List` and friends (`Collections/`), `Math`, `Random`,
`DateTime`, `Result`, `Variant`, `Nullable`, `Delegate`/`Event`,
`Reflection/`, `IO/`, `Threading/`, `Numerics/`, `Text/`, `Net/`,
`Diagnostics/`, `Security/`, `FFI/`, `Interop.bf`, `Windows.bf`,
`Linux.bf`, `Test.bf`. This is the porting target for `newbf-corlib`.

### 1.13 Mixed optimization and hot compile

Two upstream features NewBF preserves through LLVM rather than a second
backend:

- **Mixed optimization** — attributes select opt level per type/method;
  performance-critical code is fast while the rest stays debuggable, in
  one binary.
- **Hot code swapping** — the IDE recompiles changed methods and installs
  them into the running, debugged process.

### 1.14 Why it matters, and why we re-implement it

Beef occupies a rare niche: the ergonomics of C# with the explicit
control of C, no GC, and tooling built around real-time iteration. The
upstream implementation is excellent but is a large C++ system with its
own bespoke backend and a Windows-centric, C++-built toolchain. A
Rust + LLVM re-implementation that keeps the language, ports the corlib,
and reuses our portfolio's JIT/loader/FFI/IDE infrastructure makes Beef
hackable on a modern, uniform toolchain — and gives us a manual-memory
member of the family to contrast with the GC'd siblings.

---

## Part 2 — NewBF implementation plan

A Rust workspace producing a JIT-first, AOT-capable Beef compiler on
Windows, designed to port to Linux/macOS, with LLVM as the sole backend.
We inherit the *shape* of the portfolio and diverge where Beef demands —
most of all in the runtime, which is manual-memory, not GC.

### 2.1 Workspace skeleton

Mapping upstream Beef components to NewBF (`newbf-*`) crates:

| Beef (C++) component                              | NewBF crate         | Role                                                                  |
| ------------------------------------------------- | ------------------- | --------------------------------------------------------------------- |
| `BfParser` (lexing half)                          | `newbf-lexer`       | Tokenizer → token stream with spans.                                  |
| `BfParser` + `BfReducer` + `BfAst`                | `newbf-parser`      | Parse tree, then reduced AST.                                         |
| `BfSystem` + `BfDefBuilder` + `BfModule` + `BfExprEvaluator` + `BfStmtEvaluator` + `BfModuleTypeUtils` + `BfResolvedTypeUtils` | `newbf-sema` | Projects/namespaces, definition building, name + type resolution, generics, dispatch, definite-assignment, delete-flow (ownership) checks. |
| `CeMachine` / `CeDebugger`                        | `newbf-comptime`    | Compile-time execution engine.                                        |
| `BfIRBuilder`                                     | `newbf-ir`          | Typed SSA mid-level IR (Beef-IR-shaped).                              |
| `BfIRCodeGen` (LLVM path) + JIT + AOT linking     | `newbf-llvm`        | LLVM lowering, JIT, AOT object/exe emission.                          |
| `BfCompiler` + CLI                                | `newbf-driver`      | Compiler driver, REPL, `dump-*` phase-report subcommands.             |
| (hot-compile generations; modelled on NCL/NewCP)  | `newbf-loader`      | Module/workspace graph, incremental compile, hot-swap generations.    |
| `BeefRT/rt` (`Object`, `StompAlloc`, threads)     | `newbf-runtime`     | Manual-memory runtime, debug guard (stomp/leak/double-free), reflection metadata, FFI machinery. |
| `BeefLibs/corlib`                                 | `newbf-corlib`      | Beef-side standard library (`.bf` source).                            |
| (`windows_api` → bindings, like `nod-winapi`)     | `newbf-winapi`      | Vendored Win32 API metadata for the FFI surface.                      |
| BeefIDE (Beef) → portfolio iGui                   | `newbf-ide`         | Rust iGui two-thread IDE (D2D/DWrite MDI).                            |
| (test infra)                                      | `newbf-test-matrix`, `tests/newbf-tests`, `tests/newbf-compat-tests` | Rust unit/integration tests + curated Beef sample regression. |
| **The "Be" x86 backend** (`Backend/Be*`)          | **— dropped —**     | LLVM is the only backend.                                             |

Repository layout (doubly-nested, as in NewOpenDylan):

```
E:\NewBF\
  MANIFESTO.md          pinned design constraints
  PLAN.md               this file
  SPRINTS.md            two-week sprint schedule
  specs\                per-sprint specs (NN-name.md)
  beef-tests\           curated Beef sample programs (faithful-tribute regression)
  NewBF\                the Rust workspace
    Cargo.toml
    README.md
    rustfmt.toml
    LICENSE-MIT  LICENSE-APACHE
    data\               vendored windows_api.db snapshot (winapi sprint)
    docs\  MEMORY.md  IR.md  COMPTIME.md  REFLECTION.md  REPORTS.md
    src\
      newbf-driver  newbf-lexer  newbf-parser  newbf-sema  newbf-comptime
      newbf-ir  newbf-llvm  newbf-loader  newbf-runtime  newbf-winapi
      newbf-corlib  newbf-ide  newbf-test-matrix
    tests\
      newbf-tests  newbf-compat-tests
```

Workspace `Cargo.toml`: `resolver = "3"`, `edition = "2024"`, license
`MIT OR Apache-2.0`, `workspace.lints.rust = { unsafe_op_in_unsafe_fn =
"deny" }`. LLVM is pinned in `[workspace.dependencies]` to the portfolio
major (`inkwell = "=0.9.0"` / `llvm-sys = "=221.0.1"`, feature
`llvm22-1`) but left **inactive** until the LLVM sprint, so the skeleton
builds without LLVM installed — exactly as NewOpenDylan's Sprint 01 does.

### 2.2 Manifesto inheritance, and where NewBF diverges

Inherited from the portfolio:

- Rust-for-the-substrate, LLVM-only, 64-bit-only, Windows-first.
- No hand-written assembly.
- JIT-first inner loop; non-canonical regenerable cache.
- Phase-report visibility convention (NewM2): `format_*` dumps, stop
  after any phase.
- `unsafe_op_in_unsafe_fn = "deny"`.
- JIT memory manager + Win64 SEH from NewM2; loader shape from NCL/NewCP;
  Windows FFI machinery from NCL; Win32 metadata from `E:\windows_api`
  via `newbf-winapi` (like `nod-winapi`); iGui + DocCrate + selkie + the
  two-thread IDE harness **vendored into `NewBF/crates/`** (forked from
  WF64/NewFactor, owned here — not path-linked).

Diverging where Beef requires it:

- **Manual memory, not GC.** `newbf-runtime` is a manual heap with a
  debug-time stomp/leak/double-free guard. We do **not** consume the
  shared precise collector. This is the defining divergence (manifesto
  core decisions 6–7).
- **AOT is first-class, not deferred.** Beef ships native binaries; the
  AOT executable path is a v1 target alongside the JIT, not a v2 add-on.
- **The IDE is Rust (iGui), not Beef.** A divergence from NewOpenDylan,
  chosen so the IDE shell is shared across the portfolio (manifesto core
  decision 13).
- **A comptime interpreter exists.** Unlike the "everything is the JIT"
  stance, `newbf-comptime` is a genuine compile-time interpreter, because
  that is what Beef's `CeMachine` is.
- **Value/reference split and monomorphized generics** drive a different
  IR and layout strategy than the dynamic siblings.

### 2.3 Phase plan

**Phase 0 — Workspace skeleton (Sprint 01).** Crates, `Cargo.toml`,
lints, `newbf-driver --version`, docs stubs, CI. No language features.

**Phase 1 — Lexer + parser + AST + reports (Sprints 02–04).** Tokenize
Beef; parse the C#-shaped grammar into a parse tree; reduce to an AST.
`dump-tokens`, `dump-parse`, `dump-ast` reports. Reference
`BfParser.cpp`, `BfReducer.cpp`, `BfAst.h`.

**Phase 2 — Definitions, namespaces, projects (Sprint 05).** Build the
definition graph (types, methods, fields, namespaces, `using`).
`dump-defs` report. Reference `BfDefBuilder.cpp`, `BfSystem.cpp`.

**Phase 3 — Minimal kernel: primitives, functions, `if`, control flow
(Sprints 06–08).** Typed SSA IR for the smallest viable subset; LLVM
codegen for integer/float arithmetic, branches, direct calls; JIT a
"hello world" through one FFI shim to stdout. End-to-end pipeline thin,
no heap objects yet. `dump-ir`, `dump-llvm` reports.

**Phase 4 — Manual-memory runtime + the debug guard (Sprints 09–11).**
`new`/`delete`, `scope` allocations, custom allocators, object layout,
and the stomp allocator + leak ledger + double-free guard in
`newbf-runtime`. Leak/allocation reports. This is where Beef's signature
runtime lands — the inverse of the GC sprints in the siblings.

**Phase 5 — Structs, classes, interfaces, single + interface dispatch
(Sprints 12–15).** Value/reference split, fields, properties,
constructors/destructors (`this`/`~this`), inheritance, vtables, interface
dispatch. `dump-types`, `dump-dispatch` reports. A real slice of corlib
(`Object`, `String`, primitives) compiles.

**Phase 6 — Generics + monomorphization (Sprints 16–18).** Generic
types/methods, constraints, const generics, specialized instantiation.
`dump-generic-instantiations` report.

**Phase 7 — Comptime (Sprints 19–21).** The `newbf-comptime` interpreter:
`[Comptime]` methods, const-eval, type generation. Comptime trace report.

**Phase 8 — Reflection + attributes (Sprints 22–23).** Metadata emission
with strip policy; the `System.Reflection` surface. Reflection-metadata
report.

**Phase 9 — Error handling, enums-with-payloads, pattern matching,
`defer`, `mixin` (Sprints 24–26).** `Result`/`Try!`, payload enums and
`switch` binding, `defer`, hygienic `mixin`.

**Phase 10 — FFI + Win32 metadata + corlib port (Sprints 27+,
open-ended).** Beef `[Import]`/`[CallingConvention]` interop over the NCL
FFI machinery; `newbf-winapi` vendoring of `E:\windows_api`; port enough
of corlib to run a representative subset of Beef sample programs.

**Phase 11 — AOT native executables + mixed optimization (open-ended).**
Standalone-exe emission and linking; per-type/per-method opt levels via
LLVM per-function passes; the mixed-optimization report.

**Phase 12 — Hot code swapping + the iGui IDE (open-ended).** Generation-
disciplined recompile-and-swap; then `newbf-ide` — the Rust iGui two-
thread IDE (GUI thread + language worker, supervisor + interrupt hook,
DocCrate doc pane + selkie editor + phase-report viewer + leak view).

### 2.4 Compiler architecture

```
.bf sources
     │
     ▼
 newbf-lexer       tokens (+ token report)
     │
     ▼
 newbf-parser      parse tree → reduced AST (+ parse/ast reports)
     │
     ▼
 newbf-sema        defs, namespaces, name + type resolution, generics,
                    dispatch, definite-assignment, delete-flow
                    (+ defs/types/dispatch/generic reports)
     │   ⇅ newbf-comptime  (compile-time execution; comptime trace report)
     ▼
 newbf-ir          typed SSA IR (Beef-IR-shaped) (+ IR report)
     │
     ▼
 newbf-llvm        IR → LLVM IR → { JIT  |  AOT object + link }
                    (+ LLVM IR / mixed-opt / asm reports)
     │
     ▼
 newbf-runtime     manual heap, stomp/leak/double-free guard, reflection
                    metadata, FFI machinery (+ leak/allocation reports)
```

Every arrow has a corresponding human-reviewable report, and
`newbf-driver` can stop after any phase (`dump-<phase>`) or emit them all
(`--emit-reports <dir>`). The reports are schema-stable and diffable so
they gate the test suite and are auditable in review (manifesto core
decision 12).

### 2.5 Integration risks

**(a) Manual-memory correctness checking.** Beef's ownership/delete-flow
analysis (who deletes what, no double-free at compile time where
provable) is real semantic work in `newbf-sema`, distinct from the
runtime guard. Budget for it; the runtime guard is the safety net, not
the substitute.

**(b) The stomp allocator under a JIT.** Guard-page-per-allocation needs
a virtual-memory shim and interacts with the JIT's own code memory. Keep
the page allocator behind a thin OS shim (the only OS dependency in
`newbf-runtime` besides threads) and test the JIT + stomp combination
early.

**(c) Comptime ↔ sema entanglement.** `CeMachine` is invoked *from* the
semantic core and can generate types that feed back into resolution. The
`newbf-comptime` ↔ `newbf-sema` boundary must allow re-entrancy without a
circular crate dependency — likely a trait-object callback from sema into
comptime.

**(d) Monomorphization explosion.** Specialized generic instantiation can
blow up code size and compile time. Cache instantiations in the loader;
make the instantiation report a first-class diagnostic.

**(e) Mixed optimization through LLVM.** Per-method opt levels mean
per-function pass pipelines and careful inlining boundaries; the JIT and
AOT paths must agree on the policy. Defer to Phase 11 but design the IR
to carry a per-method opt-level attribute from the start.

**(f) Hot code swapping.** Recompiling a method and swapping it into a
running process, under manual memory (no GC to relocate references),
means the loader's generation discipline must retire old code only when
no frame can reach it. Lift NCL's generation model; the absence of a GC
*simplifies* object identity but the code-retirement problem remains.

**(g) FFI metadata coverage.** `E:\windows_api`'s DB covers a subset;
`newbf-winapi` must classify Beef interop signatures against it and
degrade gracefully (opaque handle for unknown `H*` types), exactly as
`nod-winapi`'s `build.rs` does.

**(h) SEH + precise stack dumps under the JIT.** Unwind info must be
registered for JIT'd code (`RtlAddFunctionTable`) so the OS unwinder, a
debugger, and our stack walker agree (manifesto core decision 16). The
stack walker must map return addresses to Beef source through the
debugger's line tables, recover inlined frames, and — the NewBF-specific
part — attach the manual-memory guard's allocation/free-site context to a
stomp fault. SEH registration is lifted from NewM2; the symbolication and
guard-context join are fresh.

### 2.6 What to skip, what to preserve

**Essential (v1):** value/reference types, structs/classes/interfaces,
single + interface dispatch, generics + monomorphization, manual memory
with `new`/`delete`/`scope`/custom allocators, the debug stomp/leak/
double-free guard, comptime, reflection, `Result`/`Try!` error handling,
payload enums + pattern matching, `defer`/`mixin`, the JIT and the AOT
native-exe path, FFI, and a corlib subset.

**Deferred to v1.x:** the full `System.Net` / `Threading` surface,
`async`, the optional GC mode, wasm.

**Deferred to v2+:** Linux/macOS ports (planned, not built), whole-program
LTO beyond per-method mixed-opt.

**Dropped entirely:** the "Be" x86 backend and COFF emitter (LLVM only),
`BeefBoot`/`BeefBuild` (replaced by `newbf-driver` + `cargo`), the
C++ `BeefRT` (re-implemented in Rust as `newbf-runtime`), `BeefySysLib`'s
GUI layer (replaced by the portfolio iGui stack).

### 2.7 The bootstrap question

Upstream Beef's compiler is C++ and is **not** self-hosting (only its IDE
is Beef). NewBF takes the same line: the compiler stays Rust forever; the
corlib is ported as *runnable Beef source*; the IDE is Rust (shared
iGui), so it adds no bootstrap dependency. We never consume Beef's IL or
build artifacts. The `tests/newbf-compat-tests` regression battery — a
curated subset of Beef sample programs — is the dogfood substitute.

### 2.8 Tractability

**Single-week tasks:** the lexer, the token/AST report dumpers, LID-free
namespace/`using` resolution, the C3-free single-inheritance layout.

**Multi-month research+engineering:** the semantic core (resolution +
generics + dispatch, the upstream `BfModule`/`BfExprEvaluator` bulk),
comptime with feedback into sema, the manual-memory delete-flow analysis,
hot code swapping under manual memory, and the corlib port (large).

The promise, in the portfolio idiom:

> A developer who wrote Beef against the upstream IDE should be able to
> open their `.bf` in NewBF, hit compile, and watch it run — JIT for the
> inner loop, a native executable for release, with leaks and
> use-after-free caught deterministically in debug — on a modern Rust +
> LLVM toolchain.
