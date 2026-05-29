//! `newbf-lexer` — the NewBF tokenizer.
//!
//! Turns Beef source into a typed token stream (`Token { kind, span,
//! text }`) with an interned file id, and exposes a `format_tokens`
//! report for `newbf-driver dump-tokens`.
//!
//! Lands in SPRINTS.md Sprint 02. Reference: `E:\beef\IDEHelper\Compiler\
//! BfParser.cpp` (the lexing half).
