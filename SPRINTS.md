# NewBF — Sprint Plan

*Drafted 2026-05-29. Companion to [`PLAN.md`](PLAN.md) (the 12-phase
roadmap) and [`MANIFESTO.md`](MANIFESTO.md) (the design commitments).*

## Preamble

The sprint cadence is **two weeks, one developer, one demo**. Each sprint
ends with something the user can run, not a tracker milestone.

- Sprint 01 produces `cargo run -p newbf-driver -- --version` and a
  workspace skeleton — cheap, demonstrable, unblocks everything.
- Sprints 02–08 cover PLAN.md phases 1–3: lexer → parser → AST → defs →
  the thin end-to-end JIT pipeline for the primitive kernel.
- Sprints 09–11 are the **manual-memory runtime + debug guard** (phase
  4) — Beef's signature, and the place NewBF most diverges from the GC'd
  siblings.
- Sprints 12–26 cover types/dispatch, generics, comptime, reflection, and
  error handling (phases 5–9).
- Sprints 27+ (FFI + Win32 metadata, corlib port, AOT, hot swap, the iGui
  IDE) are sketched only; their detail depends on the early sprints.

**Compiler first.** No IDE until the compiler can JIT and run non-trivial
Beef (manifesto core decision 13). Sprints 01–N are headless:
`newbf-driver` `dump-*` subcommands and `cargo test` are how we know we
are alive. The `newbf-ide` crate is scaffolded as a stub from Sprint 01,
but its iGui wiring activates only at the IDE sprint.

**Reports gate everything.** Every phase emits a deterministic, schema-
stable, human-reviewable report (manifesto core decision 12). Sprints
declare which reports they introduce and stabilize.

**Sibling-project leverage is the budget mechanism.** Where NewM2, NCL,
NewCP, NewOpenDylan, NewFactor, or WF64 already solved a problem, we lift
the code with attribution. Each sprint flags what is lifted vs. fresh.

---

## Sprint 01 — Workspace Skeleton
**Goal:** Compile an empty `newbf-driver` binary and print a version banner.
**Length:** 2 weeks · **Phase:** 0

### Deliverables
- [ ] Root `Cargo.toml`: `resolver = "3"`, `edition = "2024"`, workspace
      lints (`unsafe_op_in_unsafe_fn = "deny"`), shared
      `[workspace.dependencies]` with LLVM pinned-but-inactive
      (`inkwell = "=0.9.0"`, `llvm-sys = "=221.0.1"`).
- [ ] Empty crates: `newbf-lexer`, `newbf-parser`, `newbf-sema`,
      `newbf-comptime`, `newbf-ir`, `newbf-llvm`, `newbf-loader`,
      `newbf-runtime`, `newbf-winapi`, `newbf-corlib`, `newbf-ide`
      (stub bin), `newbf-test-matrix`, plus `tests/newbf-tests` and
      `tests/newbf-compat-tests`.
- [ ] `newbf-driver` CLI: `--version`, `--help`, stubbed `compile` /
      `repl` / `dump-tokens` subcommands.
- [ ] `LICENSE-MIT`, `LICENSE-APACHE`, `rustfmt.toml`, `.gitignore`.
- [ ] `docs/` stubs: `MEMORY.md`, `IR.md`, `COMPTIME.md`,
      `REFLECTION.md`, `REPORTS.md`.
- [ ] `README.md` linking MANIFESTO / PLAN / SPRINTS.

### Acceptance criteria
- `cargo build --workspace` clean on `x86_64-pc-windows-msvc`.
- `cargo run -p newbf-driver -- --version` prints
  `newbf-driver 0.0.1 (LLVM 22.1, pending)`.

### Sibling-project leverage
- Workspace `Cargo.toml` structure and lints from NewOpenDylan / NCL.
- Driver CLI shape from `newm2-driver`.

### Demo
`cargo run -p newbf-driver -- --version` from a fresh checkout.

---

## Sprint 02 — Lexer
**Goal:** Tokenize Beef source into a typed token stream, via `dump-tokens`.
**Length:** 2 weeks · **Phase:** 1

### Deliverables
- [ ] `newbf-lexer::lex` — state-machine lexer producing
      `Token { kind, span, text }`. Kinds: identifiers, keywords
      (`class`/`struct`/`interface`/`enum`/`namespace`/`using`/`new`/
      `delete`/`scope`/`defer`/`mixin`/`switch`/…), literals (int with
      `0x`/`0b` and `'_'` separators, float, char `'a'`, string +
      verbatim/interpolated), operators/punctuators, attributes `[...]`,
      generics `<...>`, comments.
- [ ] `Span { file_id: u32, lo: u32, hi: u32 }` + a file-id interner.
- [ ] `newbf-lexer::format_tokens` report; `newbf-driver dump-tokens`.
- [ ] `tests/newbf-tests` lexer fixtures from `beef-tests/samples`.

### Acceptance criteria
- Lexer round-trips a curated set of corlib `.bf` files (token kinds +
  text match hand-checked expectations).
- `dump-tokens` output is schema-stable.

### Sibling-project leverage
- Span/interner pattern from `newm2-lexer`.

### Demo
`newbf-driver dump-tokens beef-tests/samples/hello.bf`.

---

## Sprint 03 — Parser core (expressions + statements)
**Goal:** Parse Beef expressions and statements into a parse tree.
**Length:** 2 weeks · **Phase:** 1

### Deliverables
- [x] Pratt/precedence expression parser with Beef's exact precedence
      table (grounded in `BfAst.cpp`): unary/postfix, calls, indexers,
      member (`.`/`?.`), binary (incl. ranges `..<`/`...`, `is`/`as`,
      `<=>`), ternary, assignment, `new`/`scope`/`delete`/`ref`/… prefix
      forms with `:qualifier`.
- [x] Statement parser: block, expr, empty, `var`/`let` locals, `if`/
      `else`, `while`, `do`/`repeat`-`while`, C-style `for`, `for`-each,
      `return`, `break`, `continue`, `defer`. Never panics (error nodes +
      diagnostics, guaranteed progress).
- [x] `dump-parse` report + `newbf-driver dump-parse`.
- [x] Extensive tests: precedence/associativity matrix, per-construct
      expr+stmt, dangling-else, error recovery, 2000-iter no-panic fuzz.

**Deferred to Sprint 04** (need the type grammar / patterns): `switch`,
typed locals (`int x = …`; only `var`/`let` for now), and
generic-argument disambiguation in expressions (`Foo<T>(x)` — `<`/`>`
parse as comparisons until then). NewBF also folds Beef's raw-tree →
`BfReducer` step into a single AST-producing pass (documented divergence).

### Acceptance criteria
- [x] Expression/statement snippets parse to a schema-stable `dump-parse`
  AST; whole-file parsing waits for declarations (Sprint 04).

### Sibling-project leverage
- Parser scaffolding conventions from `newm2-parser`.
- Reference `BfReducer.cpp` for reduction decisions.

### Demo
`newbf-driver dump-parse beef-tests/samples/expr.bf`.

---

## Sprint 04 — Types, deferrals, and declarations
**Goal:** Land the type grammar (the keystone that unblocks everything
else), the Sprint-03 deferrals, and a declaration parser strong enough to
parse whole real files.
**Length:** 2 weeks · **Phase:** 1

### Delivered (Sprint 04a — the type-grammar core)
- [x] **Type AST + parser**: qualified paths with per-segment generic
      args, postfix suffixes (`*` pointer, `?` nullable, `[]`/`[,]`
      array, `[N]` sized), tuple types, `var`. Handles `List<List<int>>`
      via in-place `>>`→`>` splitting with rollback.
- [x] **Generic-argument disambiguation in expressions**: speculative
      parse + Roslyn-style follow-set (`Foo<T>(x)` → `Generic` + `Call`;
      `a < b > c` stays comparisons). Speculation rolls back any `>>`
      splits.
- [x] **Typed locals**: `int x = 5;`, `List<int> xs;`, `int* p;` — by
      speculative `type Ident (=|;|,)` lookahead; `a.b = c;` and `Foo();`
      remain expression statements.
- [x] **`switch` statement** with `case`/`default` arms (pattern as expr;
      richer patterns later).
- [x] Tests: 12 new tests covering type forms incl. nested generics,
      pointer/nullable/array composition; generic call vs. comparison
      disambiguation; typed locals vs. expr-stmts; switch.

### Delivered (Sprint 04b — declarations + dump-ast + corpus gate)
- [x] Declaration parser: `using` (simple / `using static` / `using A =
      B`), `namespace` (block + file-scoped), type decls (`class`/`struct`
      /`interface`/`enum`/`extension`) with modifiers, attributes
      `[Attr]` / `[A, B(x)]`, generic params + `where`-constraints, base
      list, and members — fields, methods (block + expression body +
      `;`-only), constructors `this(…)`, destructors `~this()`, properties
      with `get`/`set` accessors, nested types, enum cases with payloads
      and values. Error recovery at item/member boundaries.
- [x] `dump-ast` report (`format_ast`) + `newbf-driver dump-ast`.
- [x] Whole-file corpus parse gate: parses every `.bf` in `corlib-slice`
      (89) and `feature-suite/src/` (70). Hard gate: **no panics on 152
      real Beef files**. Soft gate: ≥5% clean parses; current run reports
      ~11% clean. Coverage grows incrementally — Beef-specific
      indexers, operators, mixins, attribute targets, and `get/set` body
      forms are open follow-on work that the no-panic gate keeps honest.
- [x] +14 declaration tests on top of Sprint 04a, now 44 parser unit
      tests + corpus + lexer suites, all green under build/clippy
      `-D warnings`/fmt/test.

### Demo
`cargo run -p newbf-driver -- dump-ast beef-tests/samples/hello.bf`
emits a clean `CompUnit` → `Using` → `Namespace (file-scoped)` → `class
Program` → `Method [public static] Main` tree.

---

## Sprint 05 — Definitions, namespaces, projects
**Goal:** Build the definition graph and resolve `using`/namespaces.
**Length:** 2 weeks · **Phase:** 2

### Deliverables
- [ ] `newbf-sema` definition builder: types, methods, fields, namespaces.
- [ ] `using`/namespace resolution; duplicate/missing diagnostics.
- [ ] `dump-defs` report.

### Sibling-project leverage
- Reference `BfDefBuilder.cpp` / `BfSystem.cpp`.

### Demo
`newbf-driver dump-defs beef-tests/samples/`.

---

## Sprints 06–08 — Primitive kernel JIT (phase 3)
**Goal:** Thin end-to-end pipeline: primitives, functions, control flow,
JIT-run "hello world."

- **06 — Typed SSA IR core (`newbf-ir`).** Smallest viable instruction
  set; `dump-ir` report.
- **07 — LLVM lowering + JIT (`newbf-llvm`).** Activate the pinned LLVM
  deps; integer/float arithmetic, branches, direct calls; `dump-llvm`
  report. Lift the JIT memory manager + Win64 SEH from NewM2.
- **08 — Hello world.** One FFI shim to stdout; `newbf-driver run` JITs
  and executes a primitive Beef program end-to-end.

**Demo (08):** `newbf-driver run beef-tests/samples/hello.bf` prints output.

---

## Sprints 09–11 — Manual-memory runtime + debug guard (phase 4)
**Goal:** Beef's signature runtime — manual memory with deterministic
debug-time safety.

- **09 — Heap + object layout + `new`/`delete` (`newbf-runtime`).** Object
  headers, allocator-qualified alloc/free, `scope` allocations, `defer
  delete`.
- **10 — The stomp allocator.** Guard-page-per-allocation + unmap-on-free
  for deterministic use-after-free / overrun faults (port of
  `BeefRT/rt/StompAlloc.cpp`). Virtual-memory shim behind an OS trait.
- **11 — Leak ledger + double-free guard + rich stack dumps.** Per-
  allocation site tracking; shutdown + on-demand leak report; double-free
  detection. **Leak and allocation reports.** The precise stack walker +
  symbolicated crash dump (manifesto core decision 16) lands here, joining
  the guard's allocation/free-site context onto a stomp fault. (SEH unwind
  registration itself ships with the JIT in Sprint 07.) Release builds
  strip the guard and fall through to a fast allocator.

**Demo (11):** a Beef program that leaks reports the leak with type + site;
a use-after-free faults at the offending access, not later.

---

## Sprints 12–26 — Types, generics, comptime, reflection, errors (phases 5–9, sketched)

| Sprints | Theme | Key reports |
| ------- | ----- | ----------- |
| 12–15 | Structs, classes, interfaces; fields, properties, ctors/dtors; inheritance + vtables + interface dispatch | `dump-types`, `dump-dispatch` |
| 16–18 | Generics + monomorphization; constraints; const generics | `dump-generic-instantiations` |
| 19–21 | Comptime (`newbf-comptime`): `[Comptime]`, const-eval, type generation | comptime trace |
| 22–23 | Reflection + attributes; metadata emission with strip policy | reflection-metadata |
| 24–26 | `Result`/`Try!`, payload enums + pattern matching, `defer`, `mixin` | — |

---

## Sprints 27+ — FFI, corlib, AOT, hot swap, IDE (phases 10–12, sketched)

- **27 — `newbf-winapi`.** Vendor `E:\windows_api` (SQLite + zstd) via a
  `build.rs` that emits a postcard+zstd blob + runtime lookup, mirroring
  `nod-winapi`. Snapshot in `data/windows_api.db`.
- **28+ — FFI.** Beef `[Import]`/`[CallingConvention]` interop over the
  NCL FFI machinery (calling-convention dispatcher, callback bridge,
  buffer marshalling).
- **corlib port.** Port `BeefLibs/corlib/src` into `newbf-corlib`
  incrementally; `tests/newbf-compat-tests` grows as the regression
  battery.
- **AOT.** Standalone native-executable emission + linking (first-class,
  not deferred). Mixed optimization via LLVM per-function passes;
  mixed-opt report.
- **Hot code swapping.** Generation-disciplined recompile-and-swap in
  `newbf-loader`.
- **The iGui IDE (`newbf-ide`).** Activate the iGui wiring: GUI thread +
  language worker + supervisor + interrupt hook, per NewFactor; embed the
  DocCrate doc pane (`doc-crate`/`docpane`), the `selkie` editor, the
  console, the inspector, the leak/allocation view, and the live phase-
  report viewer. iGui/DocCrate/selkie are vendored into `NewBF/crates/`
  as our own copies (forked from WF64/NewFactor), not path-linked.

**Demo:** the first non-trivial Beef program compiled, run, hot-swapped,
and inspected inside `newbf-ide.exe`.
