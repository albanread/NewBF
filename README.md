# NewBF

A from-scratch **Rust + LLVM** compiler for the
[Beef programming language](https://www.beeflang.org/) — JIT-first for the inner
loop (REPL, hot reload), AOT-native for shipping, with **manual memory
management and no GC** (Beef's defining trait). NewBF re-implements the
*language*, not Beef's C++ compiler.

It's the manual-memory member of a portfolio of from-scratch Rust+LLVM language
implementations.

## Status

**~60% complete** toward the full Beef language. Three waves of work are done
or nearly done; the remaining tail is generic-constraint enforcement, the broad
standard library port, delegates + events, FFI breadth, mixed optimization, and
hot code swapping.

The full pipeline — lexer → parser → sema → typed SSA IR → LLVM 22 →
**ORC JIT** *and* **AOT `.exe`** — compiles and runs real Beef. 245-program
JIT-and-run corpus, all under the live memory-safety guard; 160/160 LLVM verify.

**What works:**

*Type system.* Value `struct`s, heap `class`es (manual `new`/`delete`),
single inheritance, vtables, `abstract` / `override`, upcasts. Interfaces as
**generic constraints** (monomorphization) *and* as **dynamic dispatch targets**
(itable slots in the vtable header, `is`/`as` through an interface reference).
Generic types and **generic methods**, including generics on generic owners
(`List<T>.Map<R>()`), transitive monomorphization, const generics.
Type-based overload resolution. Target-typed construction — `.Case(x)`,
`.{ f=v }`, `.(args)` as call arguments, two-phase overload with pending-arg
shapes. Explicit numeric casts, `??` null-coalescing, `?.` null-conditional
member access.

*Language surface.* Full control flow (`if`/`while`/`for`/`foreach`/`switch` +
ternary `?:`), `break`/`continue` with labels. `defer` (LIFO, fires on every
exit edge). `scope` allocations. Properties (computed + auto, compound `+=`).
`ref`/`out` parameters. Tuples, `params` arrays (variadic). User-defined
operator overloading (binary/unary/compound). User-defined indexers.
`base.Method()`, implicit base-ctor/dtor chaining. Struct/object/array/
collection initializers.

*Algebraic data types.* Payload `enum`s (tagged-union structs), `switch` with
payload binding, `when` guards, negated `not case`, methods + computed
properties on enums. Generic `Option<T>`, `Result<T,E>`. Target-typed
`.Case(x)` in call arguments.

*Functions as values.* Function pointers, lambdas with heap-env capture,
inline-lambda-as-argument, bound instance method-refs, closures that return
closures. `Map`/`Filter`/`Fold` in corlib use these directly.

*Manual memory.* `new`/`delete`, `scope` allocations, `defer delete`.
**Debug guard** (Wave 2): quarantining stomp allocator + tombstone ledger — UAF
faults deterministically, double-free aborts with a named site
(`<fn> @ file:line`), live in both JIT and AOT. Compile-time delete-flow pass:
provable double-free + leak, zero false positives across 401 `.bf` files.

*Comptime.* Width-correct const-fold. `[EmitGenerator]` emits Beef source that
splices back into resolution via an `extension Owner` fixpoint, termination-
guarded. The emission sandbox carries full reflection metadata.

*Runtime reflection.* `typeof(T)`, dynamic `GetType()`, field + method
metadata (`FieldInfo`/`MethodInfo`), policy-gated strip (`[Reflect]`/
`[AlwaysInclude]`). All via an in-module LLVM-emitted accessor — no Rust runtime
crate needed.

*Mixins + error handling.* Hygienic AST→IR splices reusing the live `Lowerer`
(control-flow escape + SSA dominance free). `Result<T,E>` prelude. `Try!`
early-return end-to-end.

*Corlib.* `Internal` (FFI floor), `Console.WriteLine`, `Math`, `String`
(`IndexOf`/`Contains`/`Substring`/`Split`/`Replace`/`Sort`), heap arrays
(`T[]`, `.Count`, `delete`), `List<T>` (full CRUD, `Map`/`Filter`/`Fold`),
`Option<T>`, `Result<T,E>`, generational `Pool<T>` + `Handle<T>`,
compile-time `sizeof(T)`.

**Wave 3 — in progress** (generic-constraint enforcement, iterator protocol
`GetEnumerator`/`MoveNext`/`Current`/`Dispose`, comptime reflection
`typeof(T).GetFields()` inside an emit generator, custom attributes).

**Remaining (Waves 4+).** Delegates + events; full generic-constraint
enforcement across the type system; the broad corlib port (`System.IO`,
`Threading`, `Net`, …); full FFI / Win32 metadata breadth; mixed per-method
optimization levels; hot code swapping; the iGui IDE (`newbf-ide`).

Behaviour is checked by a JIT-and-run corpus — compile each program, run it,
assert its result — alongside the LLVM verifier corpus and a child-process
guard harness (fault/abort programs whose results the in-process harness can't
observe). The reasoning behind each step is logged in
[`NewBF/docs/journals/`](NewBF/docs/journals).

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
