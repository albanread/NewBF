# Phase reports

Every compiler phase emits a **human-reviewable report**, and
`newbf-driver` can stop after any phase (manifesto core decision 12, the
NewM2 convention). Reports are deterministic, schema-stable, and
diff-friendly: they are how we know the compiler is correct before there
is an IDE, they gate the test suite, and they make every phase auditable
in review.

| Phase    | Crate            | `dump-*` subcommand            | Report                                   |
| -------- | ---------------- | ------------------------------ | ---------------------------------------- |
| lex      | `newbf-lexer`    | `dump-tokens`                  | token stream + spans                     |
| parse    | `newbf-parser`   | `dump-parse` / `dump-ast`      | parse tree; reduced AST                  |
| sema     | `newbf-sema`     | `dump-defs` / `dump-types` / `dump-dispatch` / `dump-generic-instantiations` | defs, types, dispatch, instantiations, definite-assignment + delete-flow |
| comptime | `newbf-comptime` | `dump-comptime`                | comptime evaluation trace                |
| ir       | `newbf-ir`       | `dump-ir`                      | typed SSA IR                             |
| llvm     | `newbf-llvm`     | `dump-llvm` / `dump-asm`       | LLVM IR; mixed-opt; asm/object           |
| runtime  | `newbf-runtime`  | (runtime)                      | leak report; live-allocation report      |

`newbf-driver --emit-reports <dir>` writes them all. Each `dump-*`
subcommand lands with its phase's sprint.
