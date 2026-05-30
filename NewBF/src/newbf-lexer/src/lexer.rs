//! The NewBF lexer: a lossless, byte-oriented state machine.
//!
//! `lex` turns Beef source into a `Vec<Token>` whose spans tile the whole
//! input (trivia included), terminated by an `Eof` token. Every branch
//! advances the cursor by at least one byte, and non-ASCII bytes outside
//! strings/comments are consumed a whole UTF-8 char at a time, so spans
//! never split a char and the byte coverage is total.

use crate::token::{FileId, Keyword, Span, Token, TokenKind};

/// Lex `src` into tokens tagged with `file`.
pub fn lex(src: &str, file: FileId) -> Vec<Token> {
    Lexer {
        b: src.as_bytes(),
        file,
        pos: 0,
        out: Vec::new(),
    }
    .run()
}

struct Lexer<'a> {
    b: &'a [u8],
    file: FileId,
    pos: usize,
    out: Vec<Token>,
}

impl Lexer<'_> {
    fn run(mut self) -> Vec<Token> {
        while self.pos < self.b.len() {
            let start = self.pos;
            let kind = self.next_kind();
            debug_assert!(self.pos > start, "lexer failed to advance");
            self.out.push(Token {
                kind,
                span: Span::new(self.file, start as u32, self.pos as u32),
            });
        }
        let end = self.b.len() as u32;
        self.out.push(Token {
            kind: TokenKind::Eof,
            span: Span::new(self.file, end, end),
        });
        self.out
    }

    #[inline]
    fn at(&self, n: usize) -> u8 {
        let i = self.pos + n;
        if i < self.b.len() { self.b[i] } else { 0 }
    }

    /// Lex one token starting at `self.pos`, advancing past it.
    fn next_kind(&mut self) -> TokenKind {
        let c = self.b[self.pos];
        match c {
            b' ' | b'\t' | b'\r' | b'\n' | 0x0c | 0x0b => self.whitespace(),
            b'/' if self.at(1) == b'/' => self.line_comment(),
            b'/' if self.at(1) == b'*' => self.block_comment(),
            b'#' => self.preproc_line(),
            b'0'..=b'9' => self.number(),
            // NB: a leading `.digit` is NOT a float here — `.` always
            // lexes as a punctuator so `a.0` / `a.1` tuple-field access
            // works. (`.5`-style floats are written `0.5` in Beef.)
            b'\'' => self.char_literal(),
            // Triple-quoted forms (`"""…"""`, `@"""…"""`, `$"""…"""`).
            b'"' if self.at(1) == b'"' && self.at(2) == b'"' => self.triple_string(TokenKind::Str),
            b'@' if self.at(1) == b'"' && self.at(2) == b'"' && self.at(3) == b'"' => {
                self.pos += 1;
                self.triple_string(TokenKind::VerbatimStr)
            }
            b'$' if self.at(1) == b'"' && self.at(2) == b'"' && self.at(3) == b'"' => {
                self.pos += 1;
                self.triple_string(TokenKind::InterpStr)
            }
            b'"' => self.string(TokenKind::Str),
            b'@' if self.at(1) == b'"' => {
                self.pos += 1;
                self.string(TokenKind::VerbatimStr)
            }
            b'$' if self.at(1) == b'"' => {
                self.pos += 1;
                self.string(TokenKind::InterpStr)
            }
            // `$@"…"` / `@$"…"` — verbatim interpolated strings.
            b'$' if self.at(1) == b'@' && self.at(2) == b'"' => {
                self.pos += 2;
                self.string(TokenKind::InterpStr)
            }
            b'@' if self.at(1) == b'$' && self.at(2) == b'"' => {
                self.pos += 2;
                self.string(TokenKind::InterpStr)
            }
            // `@ident` — a verbatim identifier (escapes a keyword).
            b'@' if is_ident_start(self.at(1)) => {
                self.pos += 1; // consume '@'
                self.ident_tail();
                TokenKind::Ident
            }
            _ if is_ident_start(c) => self.ident(),
            _ => self.punct_or_unknown(),
        }
    }

    fn whitespace(&mut self) -> TokenKind {
        while self.pos < self.b.len()
            && matches!(self.b[self.pos], b' ' | b'\t' | b'\r' | b'\n' | 0x0c | 0x0b)
        {
            self.pos += 1;
        }
        TokenKind::Whitespace
    }

    fn line_comment(&mut self) -> TokenKind {
        // already know b[pos]=='/' and b[pos+1]=='/'
        let doc = self.at(2) == b'/' && self.at(3) != b'/';
        self.pos += 2;
        while self.pos < self.b.len() && self.b[self.pos] != b'\n' {
            self.pos += 1;
        }
        if doc {
            TokenKind::DocComment
        } else {
            TokenKind::LineComment
        }
    }

    fn preproc_line(&mut self) -> TokenKind {
        self.pos += 1; // consume '#'
        while self.pos < self.b.len() && self.b[self.pos] != b'\n' {
            self.pos += 1;
        }
        TokenKind::PreprocLine
    }

    fn block_comment(&mut self) -> TokenKind {
        self.pos += 2; // consume "/*"
        let mut depth = 1u32;
        while self.pos < self.b.len() && depth > 0 {
            if self.b[self.pos] == b'/' && self.at(1) == b'*' {
                depth += 1;
                self.pos += 2;
            } else if self.b[self.pos] == b'*' && self.at(1) == b'/' {
                depth -= 1;
                self.pos += 2;
            } else {
                self.pos += 1;
            }
        }
        TokenKind::BlockComment
    }

    fn number(&mut self) -> TokenKind {
        let mut is_float = false;
        if self.b[self.pos] == b'0' && matches!(self.at(1), b'x' | b'X') {
            self.pos += 2;
            while self.pos < self.b.len() {
                let c = self.b[self.pos];
                // digit, `_`, or a `'` group separator (0xFFFF'FFFF)
                if c.is_ascii_hexdigit()
                    || c == b'_'
                    || (c == b'\'' && self.at(1).is_ascii_hexdigit())
                {
                    self.pos += 1;
                } else {
                    break;
                }
            }
        } else if self.b[self.pos] == b'0' && matches!(self.at(1), b'b' | b'B') {
            self.pos += 2;
            while self.pos < self.b.len() {
                let c = self.b[self.pos];
                if matches!(c, b'0' | b'1' | b'_')
                    || (c == b'\'' && matches!(self.at(1), b'0' | b'1'))
                {
                    self.pos += 1;
                } else {
                    break;
                }
            }
        } else if self.b[self.pos] == b'0' && matches!(self.at(1), b'o' | b'O') {
            self.pos += 2;
            while self.pos < self.b.len() {
                let c = self.b[self.pos];
                if matches!(c, b'0'..=b'7' | b'_')
                    || (c == b'\'' && matches!(self.at(1), b'0'..=b'7'))
                {
                    self.pos += 1;
                } else {
                    break;
                }
            }
        } else {
            self.digits();
            // fractional part: a '.' is only part of the number if a digit
            // follows (so `1.foo`, `1..2`, `1.` stay separate tokens).
            if self.b.get(self.pos) == Some(&b'.') && self.at(1).is_ascii_digit() {
                is_float = true;
                self.pos += 1;
                self.digits();
            }
            // exponent
            if matches!(self.at(0), b'e' | b'E') {
                let after = self.at(1);
                let exp_ok = after.is_ascii_digit()
                    || (matches!(after, b'+' | b'-') && self.at(2).is_ascii_digit());
                if exp_ok {
                    is_float = true;
                    self.pos += 1;
                    if matches!(self.at(0), b'+' | b'-') {
                        self.pos += 1;
                    }
                    self.digits();
                }
            }
        }
        // numeric suffix (f/F/d/D → float; u/U/l/L/n → int width markers)
        while self.pos < self.b.len() && self.b[self.pos].is_ascii_alphabetic() {
            if matches!(self.b[self.pos], b'f' | b'F' | b'd' | b'D') {
                is_float = true;
            }
            self.pos += 1;
        }
        if is_float {
            TokenKind::Float
        } else {
            TokenKind::Int
        }
    }

    fn digits(&mut self) {
        while self.pos < self.b.len() {
            let c = self.b[self.pos];
            // digit, `_`, or a `'` group separator (1'000'000)
            if c.is_ascii_digit() || c == b'_' || (c == b'\'' && self.at(1).is_ascii_digit()) {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn char_literal(&mut self) -> TokenKind {
        self.pos += 1; // opening '
        while self.pos < self.b.len() {
            match self.b[self.pos] {
                b'\\' => self.pos += 2, // skip escaped char
                b'\'' => {
                    self.pos += 1;
                    break;
                }
                b'\n' => break, // unterminated; stop at line end
                _ => self.pos += 1,
            }
        }
        TokenKind::Char
    }

    /// Triple-quoted string (`"""…"""`). Consumes the three opening
    /// quotes and reads until the next `"""`, with no escape processing.
    fn triple_string(&mut self, kind: TokenKind) -> TokenKind {
        self.pos += 3; // opening """
        while self.pos + 2 < self.b.len() {
            if self.b[self.pos] == b'"'
                && self.b[self.pos + 1] == b'"'
                && self.b[self.pos + 2] == b'"'
            {
                self.pos += 3;
                return kind;
            }
            self.pos += 1;
        }
        // Unterminated — consume the rest of the buffer.
        self.pos = self.b.len();
        kind
    }

    fn string(&mut self, kind: TokenKind) -> TokenKind {
        let verbatim = matches!(kind, TokenKind::VerbatimStr);
        let interp = matches!(kind, TokenKind::InterpStr);
        self.pos += 1; // opening "
        // Brace nesting inside an interpolated string's `{ expr }` holes. A
        // `"` only closes the string at brace-depth 0; inside a hole, nested
        // strings and braces are skipped so `$"{(c ? "a" : "b")}"` lexes as
        // one token. `{{` / `}}` are escaped literal braces.
        let mut depth = 0u32;
        while self.pos < self.b.len() {
            let c = self.b[self.pos];
            if interp && depth == 0 && c == b'{' {
                if self.at(1) == b'{' {
                    self.pos += 2; // `{{` literal brace
                } else {
                    depth = 1;
                    self.pos += 1;
                }
                continue;
            }
            if interp && depth > 0 {
                match c {
                    b'{' => {
                        depth += 1;
                        self.pos += 1;
                    }
                    b'}' => {
                        depth -= 1;
                        self.pos += 1;
                    }
                    // A nested string inside the interpolation hole.
                    b'"' => {
                        self.pos += 1;
                        while self.pos < self.b.len() {
                            match self.b[self.pos] {
                                b'\\' => self.pos += 2,
                                b'"' => {
                                    self.pos += 1;
                                    break;
                                }
                                _ => self.pos += 1,
                            }
                        }
                    }
                    _ => self.pos += 1,
                }
                continue;
            }
            match c {
                b'\\' if !verbatim => self.pos += 2, // escape
                b'"' => {
                    // In verbatim strings, "" is an escaped quote.
                    if verbatim && self.at(1) == b'"' {
                        self.pos += 2;
                    } else {
                        self.pos += 1;
                        break;
                    }
                }
                _ => self.pos += 1,
            }
        }
        kind
    }

    fn ident(&mut self) -> TokenKind {
        self.ident_tail();
        let text = std::str::from_utf8(&self.b[self.span_start()..self.pos]).unwrap_or("");
        match Keyword::from_ident(text) {
            Some(kw) => TokenKind::Keyword(kw),
            None => TokenKind::Ident,
        }
    }

    fn ident_tail(&mut self) {
        self.pos += 1; // first char already validated
        while self.pos < self.b.len() && is_ident_continue(self.b[self.pos]) {
            self.pos += 1;
        }
    }

    /// Recover the start of the current identifier run for keyword lookup.
    /// The identifier began where the last pushed token ended (or 0).
    fn span_start(&self) -> usize {
        self.out.last().map_or(0, |t| t.span.hi as usize)
    }

    fn punct_or_unknown(&mut self) -> TokenKind {
        use TokenKind::*;
        let c = self.b[self.pos];
        // Two/three-char operators are matched by lookahead, longest first.
        macro_rules! tok {
            ($len:expr, $kind:expr) => {{
                self.pos += $len;
                return $kind;
            }};
        }
        match c {
            b'(' => tok!(1, LParen),
            b')' => tok!(1, RParen),
            b'[' => tok!(1, LBracket),
            b']' => tok!(1, RBracket),
            b'{' => tok!(1, LBrace),
            b'}' => tok!(1, RBrace),
            b';' => tok!(1, Semicolon),
            b',' => tok!(1, Comma),
            b'#' => tok!(1, Pound),
            b'~' => tok!(1, Tilde),
            b'.' => {
                if self.at(1) == b'.' && self.at(2) == b'.' {
                    tok!(3, DotDotDot)
                } else if self.at(1) == b'.' && self.at(2) == b'<' {
                    tok!(3, DotDotLess)
                } else if self.at(1) == b'.' {
                    tok!(2, DotDot)
                } else {
                    tok!(1, Dot)
                }
            }
            b':' => {
                if self.at(1) == b':' {
                    tok!(2, ColonColon)
                } else {
                    tok!(1, Colon)
                }
            }
            b'?' => {
                if self.at(1) == b'?' && self.at(2) == b'=' {
                    tok!(3, QuestionQuestionEq)
                } else if self.at(1) == b'?' {
                    tok!(2, QuestionQuestion)
                } else if self.at(1) == b'.' {
                    tok!(2, QuestionDot)
                } else {
                    tok!(1, Question)
                }
            }
            b'+' => match self.at(1) {
                b'+' => tok!(2, PlusPlus),
                b'=' => tok!(2, PlusEq),
                _ => tok!(1, Plus),
            },
            b'-' => match self.at(1) {
                b'>' => tok!(2, Arrow),
                b'-' => tok!(2, MinusMinus),
                b'=' => tok!(2, MinusEq),
                _ => tok!(1, Minus),
            },
            b'*' => {
                if self.at(1) == b'=' {
                    tok!(2, StarEq)
                } else {
                    tok!(1, Star)
                }
            }
            b'/' => {
                if self.at(1) == b'=' {
                    tok!(2, SlashEq)
                } else {
                    tok!(1, Slash)
                }
            }
            b'%' => {
                if self.at(1) == b'=' {
                    tok!(2, PercentEq)
                } else {
                    tok!(1, Percent)
                }
            }
            b'^' => {
                if self.at(1) == b'=' {
                    tok!(2, CaretEq)
                } else {
                    tok!(1, Caret)
                }
            }
            b'!' => {
                if self.at(1) == b'=' && self.at(2) == b'=' {
                    tok!(3, StrictNeq)
                } else if self.at(1) == b'=' {
                    tok!(2, NotEq)
                } else {
                    tok!(1, Bang)
                }
            }
            b'=' => {
                if self.at(1) == b'=' && self.at(2) == b'=' {
                    tok!(3, StrictEq)
                } else if self.at(1) == b'=' {
                    tok!(2, EqEq)
                } else if self.at(1) == b'>' {
                    tok!(2, FatArrow)
                } else {
                    tok!(1, Assign)
                }
            }
            b'&' => match self.at(1) {
                b'&' => tok!(2, AmpAmp),
                b'=' => tok!(2, AmpEq),
                // Overflow arithmetic: `&+` `&-` `&*` and `&+=` etc.
                b'+' if self.at(2) == b'=' => tok!(3, AmpPlusEq),
                b'+' => tok!(2, AmpPlus),
                b'-' if self.at(2) == b'=' => tok!(3, AmpMinusEq),
                b'-' => tok!(2, AmpMinus),
                b'*' if self.at(2) == b'=' => tok!(3, AmpStarEq),
                b'*' => tok!(2, AmpStar),
                _ => tok!(1, Amp),
            },
            b'|' => match self.at(1) {
                b'|' => tok!(2, PipePipe),
                b'=' => tok!(2, PipeEq),
                _ => tok!(1, Pipe),
            },
            b'<' => {
                if self.at(1) == b'<' && self.at(2) == b'=' {
                    tok!(3, ShlEq)
                } else if self.at(1) == b'=' && self.at(2) == b'>' {
                    tok!(3, Spaceship)
                } else if self.at(1) == b'<' {
                    tok!(2, Shl)
                } else if self.at(1) == b'=' {
                    tok!(2, Le)
                } else {
                    tok!(1, Lt)
                }
            }
            b'>' => {
                if self.at(1) == b'>' && self.at(2) == b'=' {
                    tok!(3, ShrEq)
                } else if self.at(1) == b'>' {
                    tok!(2, Shr)
                } else if self.at(1) == b'=' {
                    tok!(2, Ge)
                } else {
                    tok!(1, Gt)
                }
            }
            b'@' => tok!(1, At),
            b'$' => tok!(1, Dollar),
            _ => {
                // Unknown byte. Consume a whole UTF-8 char so spans stay
                // on char boundaries even for stray non-ASCII input.
                self.pos += utf8_len(c);
                Unknown
            }
        }
    }
}

#[inline]
fn is_ident_start(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphabetic()
}

#[inline]
fn is_ident_continue(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphanumeric()
}

/// Byte length of the UTF-8 sequence whose lead byte is `c` (>= 1).
#[inline]
fn utf8_len(c: u8) -> usize {
    match c {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1, // continuation/invalid lead: advance one byte
    }
}
