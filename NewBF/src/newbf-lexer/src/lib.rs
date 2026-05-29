//! `newbf-lexer` — the NewBF tokenizer.
//!
//! Turns Beef source into a *lossless* token stream: trivia (whitespace
//! and comments) are emitted as tokens too, so the spans tile the entire
//! input and concatenating each token's text reconstructs the source
//! exactly. [`format_tokens`] renders the schema-stable report behind
//! `newbf-driver dump-tokens`.
//!
//! The keyword set is lifted verbatim from upstream Beef's tokenizer
//! (`E:\beef\IDEHelper\Compiler\BfParser.cpp`). See SPRINTS.md Sprint 02.

mod lexer;
mod token;

pub use lexer::lex;
pub use token::{FileId, Keyword, SourceMap, Span, Token, TokenKind};

use std::fmt::Write;

/// Render a schema-stable, human-reviewable token report for `tokens`,
/// which must have been lexed from `src`. One line per token:
/// `index  kind  lo..hi  "text"`. This is the `dump-tokens` phase report.
pub fn format_tokens(src: &str, tokens: &[Token]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "tokens: {}", tokens.len());
    for (i, t) in tokens.iter().enumerate() {
        let _ = writeln!(
            out,
            "{:>5}  {:<22} {:>6}..{:<6} {}",
            i,
            format!("{:?}", t.kind),
            t.span.lo,
            t.span.hi,
            display_text(t.span.text(src)),
        );
    }
    out
}

/// Escape and truncate token text for the report so the output stays one
/// line per token and is deterministic.
fn display_text(text: &str) -> String {
    const MAX: usize = 30;
    let end = if text.chars().count() > MAX {
        text.char_indices().nth(MAX).map_or(text.len(), |(i, _)| i)
    } else {
        text.len()
    };
    let head = &text[..end];
    if end < text.len() {
        format!("{head:?} …(+{} bytes)", text.len() - end)
    } else {
        format!("{head:?}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lex `src` and return the non-trivia, non-EOF token kinds.
    fn kinds(src: &str) -> Vec<TokenKind> {
        lex(src, FileId(0))
            .into_iter()
            .filter(|t| !t.kind.is_trivia() && t.kind != TokenKind::Eof)
            .map(|t| t.kind)
            .collect()
    }

    /// Lossless invariant: concatenating every token's text == source.
    fn assert_roundtrip(src: &str) {
        let toks = lex(src, FileId(0));
        let mut rebuilt = String::new();
        for t in &toks {
            if t.kind == TokenKind::Eof {
                continue;
            }
            rebuilt.push_str(t.span.text(src));
        }
        assert_eq!(rebuilt, src);
    }

    #[test]
    fn keywords_vs_idents() {
        use Keyword::*;
        use TokenKind::Keyword as K;
        // `int`/`Foo`/`_bar` are identifiers; `class`/`return` are keywords.
        assert_eq!(
            kinds("class int Foo _bar return"),
            [
                K(Class),
                TokenKind::Ident,
                TokenKind::Ident,
                TokenKind::Ident,
                K(Return)
            ]
        );
    }

    #[test]
    fn verbatim_identifier_is_not_keyword() {
        assert_eq!(kinds("@class"), [TokenKind::Ident]);
    }

    #[test]
    fn numbers() {
        use TokenKind::{Float, Int};
        assert_eq!(
            kinds("0 123 0xFF 0b1010 1_000 1.0 .5 1e9 3.14f 1.0e-3"),
            [Int, Int, Int, Int, Int, Float, Float, Float, Float, Float]
        );
    }

    #[test]
    fn range_after_int_is_not_a_float() {
        use TokenKind::{DotDot, Int};
        // `1..2` must be Int, DotDot, Int — not a malformed float.
        assert_eq!(kinds("1..2"), [Int, DotDot, Int]);
    }

    #[test]
    fn member_access_after_int() {
        use TokenKind::{Dot, Ident, Int};
        assert_eq!(kinds("1.foo"), [Int, Dot, Ident]);
    }

    #[test]
    fn chars_and_strings() {
        use TokenKind::{Char, InterpStr, Str, VerbatimStr};
        assert_eq!(
            kinds(r#"'a' '\n' "hi" @"v\n" $"x{y}""#),
            [Char, Char, Str, VerbatimStr, InterpStr]
        );
    }

    #[test]
    fn operators_maximal_munch() {
        use TokenKind::*;
        assert_eq!(
            kinds("+ ++ += => -> :: .. ... == != <<= >> ?? ?. ??="),
            [
                Plus,
                PlusPlus,
                PlusEq,
                FatArrow,
                Arrow,
                ColonColon,
                DotDot,
                DotDotDot,
                EqEq,
                NotEq,
                ShlEq,
                Shr,
                QuestionQuestion,
                QuestionDot,
                QuestionQuestionEq,
            ]
        );
    }

    #[test]
    fn destructor_tilde_this() {
        use Keyword::This;
        use TokenKind::{Keyword as K, Tilde};
        assert_eq!(kinds("~this"), [Tilde, K(This)]);
    }

    #[test]
    fn comments_are_trivia_with_distinct_kinds() {
        let ks: Vec<_> = lex("// a\n/* b */\n/// c", FileId(0))
            .into_iter()
            .map(|t| t.kind)
            .filter(|k| k.is_trivia())
            .collect();
        assert!(ks.contains(&TokenKind::LineComment));
        assert!(ks.contains(&TokenKind::BlockComment));
        assert!(ks.contains(&TokenKind::DocComment));
    }

    #[test]
    fn nested_block_comment() {
        // A single block-comment token must span the whole nested comment.
        let toks = lex("/* a /* b */ c */x", FileId(0));
        assert_eq!(toks[0].kind, TokenKind::BlockComment);
        assert_eq!(toks[0].span.text("/* a /* b */ c */x"), "/* a /* b */ c */");
    }

    #[test]
    fn hello_world_shape() {
        use Keyword::Using;
        use TokenKind::{Ident, Keyword as K, Semicolon};
        assert_eq!(kinds("using System;"), [K(Using), Ident, Semicolon]);
    }

    #[test]
    fn roundtrip_holds_on_mixed_source() {
        assert_roundtrip(
            "using System;\nnamespace N {\n\tclass C { public int x = 0x1F; } // tail\n}\n",
        );
        assert_roundtrip("\t  \n\r\n  "); // pure trivia
        assert_roundtrip(""); // empty
        assert_roundtrip("€ stray unicode and π"); // non-ASCII stays char-safe
    }

    #[test]
    fn unknown_byte_keeps_spans_total() {
        // A backtick isn't a Beef token; it must still be covered.
        let toks = lex("a`b", FileId(0));
        assert!(toks.iter().any(|t| t.kind == TokenKind::Unknown));
        assert_roundtrip("a`b");
    }
}
