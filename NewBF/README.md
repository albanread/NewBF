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

**Sprint 01 — workspace skeleton.** Empty `newbf-*` crates and a driver
that prints a version banner. No language features yet.

```
cargo build --workspace
cargo run -p newbf-driver -- --version
# newbf-driver 0.0.1 (LLVM 22.1, pending)
```

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
