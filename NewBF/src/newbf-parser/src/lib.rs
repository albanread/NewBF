//! `newbf-parser` — the NewBF parser and AST.
//!
//! Parses the C#-shaped Beef grammar into a concrete parse tree, then
//! reduces it to a working AST (spans preserved throughout). Exposes
//! `dump-parse` and `dump-ast` reports.
//!
//! Lands in SPRINTS.md Sprints 03–04. Reference: `E:\beef\IDEHelper\
//! Compiler\BfReducer.cpp` and `BfAst.h`.
