# NewBF

A from-scratch **Rust + LLVM** implementation of the
[Beef programming language](https://www.beeflang.org/) — JIT-first for the
inner loop (REPL, hot code swapping) and AOT-native for shipping, with
**manual memory management** and debug-time memory safety (real-time leak
detection, guaranteed use-after-free and double-deletion protection).

NewBF is the manual-memory member of a portfolio of from-scratch Rust+LLVM
language implementations (NewM2, NewCormanLisp, NewOpenDylan, NewFactor, …).
It re-implements the *language*, not Beef's C++ compiler.

## Design documents

The authoritative planning docs live one directory up, at the project root:

- [`../MANIFESTO.md`](../MANIFESTO.md) — the pinned design commitments.
- [`../PLAN.md`](../PLAN.md) — the language survey and 12-phase plan.
- [`../SPRINTS.md`](../SPRINTS.md) — the two-week sprint schedule.

Per-crate notes are in [`docs/`](docs): memory model, IR, comptime,
reflection, and the phase-report convention.

## Status

**Wave 1 (complete)** — the full Beef type system and language surface through
the dispatch frontier. 245-program JIT-run corpus clean under Stomp guard;
verify 160/160; 371 commits.

*Pipeline:* lexer → parser → sema → typed SSA IR → LLVM 22 → **ORC JIT**
*and* **AOT `.exe`**.

*Type system:* value `struct`s, heap `class`es, single inheritance, vtables,
`abstract` methods, `override`, upcasts + downcasts, `is`/`as`, virtual dispatch.
Interfaces — as generic constraints (monomorphization) *and* as dynamic dispatch
targets (vtable-header itable slots, `is`/`as` through interface). Generic types
and generic methods, including generics on generic owners (`List<T>.Map<R>()`),
const generics, transitive monomorphization. Type-based overload resolution.
Target-typed construction (`.Some(x)`, `.{ f=v }`, `.(args)` as call arguments,
two-phase overload with pending-arg shapes). Explicit numeric casts.

*Language surface:* full control flow (`if`/`while`/`for`/`foreach`/`switch` +
ternary `?:`), `break`/`continue` with labels; `defer` (LIFO, fires before every
exit edge); `scope` allocations. Properties (computed `get`/`set`, auto
`{ get; set; }`, compound `+=`). `ref`/`out` parameters. Tuples (`(A,B)` →
synthetic value structs). `params` arrays (variadic). `??` null-coalescing,
`?.` null-conditional member access + calls. User-defined operator overloading
(binary/unary/compound). User-defined indexers. `base.Method()`, implicit
base-ctor/dtor chaining. Struct/object/array/collection initializers.

*Algebraic data types:* payload `enum`s (tagged-union structs), `switch` with
payload binding, `when` guards, `not case`, computed properties on enums.
Heterogeneous payload variants. Generic `Option<T>`, `Result<T,E>`. Target-typed
`.Case(x)` in call arguments.

*Functions as values:* function pointers, lambdas (params + heap-env capture),
inline-lambda-as-argument, bound instance method-refs, closures that return
closures. `Map<R>`/`Filter`/`Fold` in corlib use these directly.

*Manual memory:* `new`/`delete`, `scope` allocations, custom allocators.

*Corlib:* `Internal` (FFI floor), `Console.WriteLine`, `Math`, `String`
(real `IndexOf`/`Contains`/`Substring`/`Split`/`Replace`/`Sort`),
heap arrays (`T[]` — indexing, `.Count`, `delete`),
`List<T>` (full CRUD, `Map`/`Filter`/`Fold`), `Option<T>`, `Result<T,E>`,
generational `Pool<T>` + `Handle<T>`, compile-time `sizeof(T)`.

---

**Wave 2 (complete)** — Beef's four distinctive features.

- **Memory-safety runtime guard** — quarantining stomp allocator + tombstone
  ledger; UAF faults deterministically (`ACCESS_VIOLATION`), double-free aborts
  (`__fastfail`) with named site (`<fn> @ file:line`). Live in **both JIT and
  AOT** (`.CRT$XCU` pre-`main` bootstrap). Compile-time delete-flow pass:
  provable double-free + leak, zero false positives across 401 `.bf` files.
- **Comptime** — width-correct const-fold; `[EmitGenerator]` emits Beef source
  that splices back into resolution via an `extension Owner` fixpoint;
  termination-guarded (round + byte caps); the emission sandbox carries full
  reflection metadata.
- **Runtime reflection** — `typeof(T)`, dynamic `GetType()`, field + method
  metadata (`FieldInfo`/`MethodInfo`), policy-gated strip (`[Reflect]`/
  `[AlwaysInclude]`), `System.Reflection`, golden format-report. All via an
  in-module LLVM-emitted accessor — no Rust runtime crate required.
- **Mixins + `Try!`/`Result`** — hygienic AST→IR splices reusing the live
  `Lowerer` (control-flow escape + SSA dominance free); `Result<T,E>` prelude;
  `Try!` early-return end-to-end.

---

**Wave 3 (in progress)** — generics maturity + metaprogramming.

- Generic constraints — classifier skeleton + ratchet pins (enforcement pass,
  `where T : IFoo` violations)
- Iterator protocol — `ListEnumerator<T>`, `GetEnumerator`/`MoveNext`/
  `Current`/`Dispose`, fifth `foreach` branch in sema
- Comptime reflection — `try_lower_emit_type_body` relaxed, struct-by-value
  sandbox confirmed (`typeof(T).GetFields()` inside an emit generator)
- Custom attributes — `AttrMeta`/`TypeMeta.attributes`, attribute collector

See [`docs/journals/`](docs/journals) for the full build log and
[`../SPRINTS.md`](../SPRINTS.md) for what's next.

## Building

Needs a **Rust** toolchain and **LLVM 22.1** (Windows x86-64 / MSVC only for now —
the runtime uses Win64 SEH and `link.exe`):

```
set LLVM_SYS_221_PREFIX=C:\path\to\llvm-22.1
cargo build --workspace
cargo test  --workspace
cargo run -p newbf-driver -- --version
```

`newbf-winapi` embeds a committed Win32 ABI snapshot, so no external data is
needed to build; set `NEWBF_WINDOWS_API_DB` to refresh it from a Win32-metadata
SQLite DB ([details](src/newbf-winapi/data/README.md)).

## Workspace layout

| Crate              | Role                                                        |
| ------------------ | ----------------------------------------------------------- |
| `newbf-driver`     | CLI, REPL, and `dump-*` phase-report subcommands.           |
| `newbf-lexer`      | Tokenizer.                                                  |
| `newbf-parser`     | Parse tree → reduced AST.                                   |
| `newbf-sema`       | Name + type resolution, generics, dispatch, ownership.      |
| `newbf-comptime`   | Compile-time execution engine.                              |
| `newbf-ir`         | Typed SSA mid-level IR.                                      |
| `newbf-llvm`       | LLVM lowering, JIT, AOT object/exe emission.                |
| `newbf-loader`     | Module graph, incremental compile, hot-swap generations.    |
| `newbf-runtime`    | Manual-memory runtime + debug guard, reflection, FFI.       |
| `newbf-winapi`     | Vendored Win32 API metadata (from `E:\windows_api`).        |
| `newbf-corlib`     | Beef-side standard library (`.bf` source).                  |
| `newbf-ide`        | Rust iGui two-thread IDE (Direct2D/DirectWrite MDI).        |
| `newbf-test-matrix`| Test orchestration.                                         |

The "Be" x86 backend from upstream Beef is intentionally dropped — LLVM is
the only code generator.

## License

`MIT OR Apache-2.0`, compatible with upstream Beef's MIT license. See
[`LICENSE-MIT`](LICENSE-MIT) and [`LICENSE-APACHE`](LICENSE-APACHE).
