# beef-tests

NewBF's Beef test corpus: the faithful-tribute regression battery
(`tests/newbf-compat-tests`) plus lexer/parser stability fixtures
(`tests/newbf-tests`). Everything here is plain Beef that should behave
identically under upstream Beef and NewBF.

## Layout

| Dir | Files | Provenance | Use |
| --- | ----: | ---------- | --- |
| `samples/` | 1 | ours (hand-written) | small hermetic programs (`hello.bf`) |
| `feature-suite/` | 70 `.bf` | verbatim copy of `E:\beef\IDEHelper\Tests` | the upstream **compiler regression suite** |
| `corlib-slice/` | 89 `.bf` | top-level `E:\beef\BeefLibs\corlib\src\*.bf` | **lexer/parser round-trip** stability fixtures |

### `feature-suite/`

A verbatim snapshot of upstream Beef's compiler test workspace (a
BeefSpace): `src/` holds ~70 **feature-organized, self-checking** test
files — `Aliases`, `Anonymous`, `Append`, `Arrays`, `Bitfields`,
`Boxing`, `Cascades`, `Comptime`, `ConstEval`, `Constraints`,
`Delegates`, `Enums`, `ExtensionMethods`, `Floats`, `FuncRefs`,
`Functions`, … — using `[Test]` methods + asserts (Beef's built-in test
framework). Helper projects (`LibA`/`LibB`/`LibC`, `CLib`, `BeefLinq`,
`TestsB`) and the `BeefProj.toml` / `BeefSpace.toml` manifests are kept
so the suite stays intact.

This is the spine of `newbf-compat-tests`: **each sprint declares which
feature files it makes pass**, and we grow the green set as language
features land. The files self-check, so once NewBF can execute them the
pass/fail signal is automatic.

### `corlib-slice/`

The 89 top-level files of the Beef standard library (`Object`,
`ValueType`, `Type`, `String`, `Array`, `Span`, the integer/float/char
primitives, `Result`, `Nullable`, `Variant`, `Delegate`, `Event`,
`Math`, the allocator interfaces, …). These exercise a huge amount of
real Beef syntax — generics, operators, interfaces, attributes, custom
allocators — and are used in the early sprints for `dump-tokens` /
`dump-ast` round-trip stability **without needing to execute anything**.
(The full corlib is the porting target for `newbf-corlib`; this is just a
representative slice.)

## Provenance and license

These fixtures are copied **read-only** from the upstream Beef tree at
[`E:\beef`](file:///E:/beef) (MIT, © BeefyTech LLC) as it stood at clone
time. We never modify `E:\beef`; we snapshot fixtures here so the
regression corpus is reproducible and self-contained. Beef's MIT license
is preserved in [`UPSTREAM-LICENSE.txt`](UPSTREAM-LICENSE.txt).

## Status

Not yet wired into the Rust test crates — there is no lexer to feed them
to until SPRINTS.md Sprint 02. Sprint 02 consumes `corlib-slice/` for the
first round-trip fixtures; `feature-suite/` comes online as the compiler
grows enough to run it.
