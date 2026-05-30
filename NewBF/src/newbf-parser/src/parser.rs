//! The NewBF parser: a recursive-descent statement parser with a Pratt
//! (precedence-climbing) expression parser using Beef's exact operator
//! precedence. It never panics: malformed input produces `Error` nodes
//! plus diagnostics, and every loop is guaranteed to make progress.
//!
//! Scope (Sprint 03): expressions in full, and the statement core
//! (block/expr/if/while/do/for/return/break/continue/defer/`var`+`let`
//! locals). Deferred to Sprint 04 (they need the type grammar / patterns):
//! `switch`, typed locals (`int x = …`), and generic-argument
//! disambiguation in expressions (`Foo<T>(x)` — `<`/`>` parse as
//! comparisons for now).

use newbf_lexer::{FileId, Keyword, Span, Token, TokenKind, lex};

use crate::ast::*;

/// A parser diagnostic (an error or note at a source span).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Diagnostic {
    pub span: Span,
    pub message: String,
}

/// Parse a single expression from `src`.
pub fn parse_expr(src: &str, file: FileId) -> (Expr, Vec<Diagnostic>) {
    let mut p = Parser::new(src, file);
    let e = p.expr();
    if !p.at(TokenKind::Eof) {
        p.error("trailing tokens after expression");
    }
    (e, p.diagnostics)
}

/// Parse a statement-sequence fragment (a method body without braces)
/// until end of input. Used by `dump-parse` on snippet files; whole-file
/// parsing waits for declarations (Sprint 04).
pub fn parse_fragment(src: &str, file: FileId) -> (Vec<Stmt>, Vec<Diagnostic>) {
    let mut p = Parser::new(src, file);
    let mut stmts = Vec::new();
    while !p.at(TokenKind::Eof) {
        let before = p.pos;
        stmts.push(p.stmt());
        debug_assert!(p.pos > before, "stmt loop made no progress");
    }
    (stmts, p.diagnostics)
}

struct Parser<'a> {
    src: &'a str,
    toks: Vec<Token>,
    file: FileId,
    pos: usize,
    diagnostics: Vec<Diagnostic>,
    /// History of `>>`→`>` mutations made by [`Parser::close_gt`], so
    /// speculative parses (generic-arg disambiguation) can roll them back.
    splits: Vec<(usize, Token)>,
    /// When set, a trailing `{ … }` after an expression is *not* treated as
    /// an object initializer (it belongs to an enclosing construct — e.g. a
    /// constructor chain `: base(args) { body }`, where the `{` opens the
    /// ctor body, not an initializer of `base(args)`). Reset inside `()`/`[]`
    /// so initializers in arguments still parse.
    suppress_init: bool,
}

/// Snapshot of parser state used by speculative parses.
#[derive(Clone, Copy)]
struct Save {
    pos: usize,
    diag_len: usize,
    splits_len: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str, file: FileId) -> Self {
        let lexed = lex(src, file);
        let toks: Vec<Token> = crate::preprocess::preprocess(src, lexed)
            .into_iter()
            .filter(|t| !t.kind.is_trivia())
            .collect();
        Self {
            src,
            toks,
            file,
            pos: 0,
            diagnostics: Vec::new(),
            splits: Vec::new(),
            suppress_init: false,
        }
    }

    /// Compare the current token's text to a literal. Useful for the few
    /// places where contextual keywords (`get`, `set`) aren't lexer
    /// keywords proper.
    fn at_ident_text(&self, text: &str) -> bool {
        self.at(TokenKind::Ident) && self.cur().span.text(self.src) == text
    }

    fn eat_ident_text(&mut self, text: &str) -> bool {
        if self.at_ident_text(text) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn save(&self) -> Save {
        Save {
            pos: self.pos,
            diag_len: self.diagnostics.len(),
            splits_len: self.splits.len(),
        }
    }

    fn restore(&mut self, s: Save) {
        while self.splits.len() > s.splits_len {
            let (idx, original) = self.splits.pop().unwrap();
            self.toks[idx] = original;
        }
        self.pos = s.pos;
        self.diagnostics.truncate(s.diag_len);
    }

    // ── cursor ──────────────────────────────────────────────────────────

    fn cur(&self) -> Token {
        // `toks` always ends in Eof, and we never advance past it.
        self.toks[self.pos]
    }

    fn kind(&self) -> TokenKind {
        self.cur().kind
    }

    fn nth_kind(&self, n: usize) -> TokenKind {
        self.toks
            .get(self.pos + n)
            .map_or(TokenKind::Eof, |t| t.kind)
    }

    fn at(&self, k: TokenKind) -> bool {
        self.kind() == k
    }

    fn at_kw(&self, k: Keyword) -> bool {
        self.kind() == TokenKind::Keyword(k)
    }

    fn bump(&mut self) -> Token {
        let t = self.cur();
        if t.kind != TokenKind::Eof {
            self.pos += 1;
        }
        t
    }

    fn eat(&mut self, k: TokenKind) -> bool {
        if self.at(k) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn eat_kw(&mut self, k: Keyword) -> bool {
        if self.at_kw(k) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, k: TokenKind, what: &str) -> bool {
        if self.eat(k) {
            true
        } else {
            self.error(&format!("expected {what}"));
            false
        }
    }

    fn error(&mut self, message: &str) {
        self.diagnostics.push(Diagnostic {
            span: self.cur().span,
            message: message.to_owned(),
        });
    }

    /// Start offset of the current token (for building a node span).
    fn start(&self) -> u32 {
        self.cur().span.lo
    }

    /// Build a span from `lo` to the end of the most-recently consumed
    /// token.
    fn finish(&self, lo: u32) -> Span {
        let hi = if self.pos > 0 {
            self.toks[self.pos - 1].span.hi
        } else {
            lo
        };
        Span::new(self.file, lo, hi.max(lo))
    }

    // ── expressions ─────────────────────────────────────────────────────

    fn expr(&mut self) -> Expr {
        self.assignment()
    }

    /// Assignment is right-associative and lower than everything else.
    fn assignment(&mut self) -> Expr {
        let lo = self.start();
        let target = self.ternary();
        if let Some(op) = self.peek_assign_op() {
            self.bump();
            let value = self.assignment();
            return Expr::Assign {
                span: self.finish(lo),
                op,
                target: Box::new(target),
                value: Box::new(value),
            };
        }
        target
    }

    fn ternary(&mut self) -> Expr {
        let lo = self.start();
        let cond = self.binary(1);
        if self.at(TokenKind::Question) {
            self.bump();
            let then = self.assignment();
            self.expect(TokenKind::Colon, "`:` in conditional expression");
            let els = self.assignment();
            return Expr::Ternary {
                span: self.finish(lo),
                cond: Box::new(cond),
                then: Box::new(then),
                els: Box::new(els),
            };
        }
        cond
    }

    /// Precedence-climbing over binary operators. `min_bp` is the minimum
    /// binding power this call will accept.
    fn binary(&mut self, min_bp: u8) -> Expr {
        let lo = self.start();
        let mut lhs = self.unary();
        loop {
            // `not case` — Beef's negated case-test operator. `not` lexes as
            // an identifier; treat the pair as a `case` operator (the
            // negation is dropped for now). Same precedence as `case`.
            let not_case =
                self.at_ident_text("not") && self.nth_kind(1) == TokenKind::Keyword(Keyword::Case);
            let Some(op) = (if not_case {
                Some(BinOp::Case)
            } else {
                self.peek_binop()
            }) else {
                break;
            };
            let bp = op.precedence();
            if bp < min_bp {
                break;
            }
            if not_case {
                self.bump(); // not
                self.bump(); // case
            } else {
                self.bump();
            }
            // `is`/`as`/`case` take a type or pattern on the right; we
            // parse it as a unary expression (a type/pattern stand-in).
            let rhs = if matches!(op, BinOp::Is | BinOp::As | BinOp::Case) {
                self.unary()
            } else if matches!(op, BinOp::Range | BinOp::ClosedRange)
                && !Self::can_start_unary(self.kind())
            {
                // Open-ended range: `1...`, `a..<` with no upper bound (e.g.
                // the index `iList[1...]`). Placeholder empty-span operand.
                let hi = self.toks[self.pos - 1].span.hi;
                Expr::Ident(Span::new(self.file, hi, hi))
            } else {
                self.binary(bp + 1) // left-associative
            };
            lhs = Expr::Binary {
                span: self.finish(lo),
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        lhs
    }

    fn unary(&mut self) -> Expr {
        let lo = self.start();
        // Leading range / spread prefix: `..expr` (spread/append, e.g.
        // `f(.. new T())`), `..<n` / `...n` (from-start range, e.g.
        // `a[..<n]`). Consume the operator; the operand carries the bound.
        // A bare operator (`a[...]`) yields a placeholder.
        if matches!(
            self.kind(),
            TokenKind::DotDot | TokenKind::DotDotLess | TokenKind::DotDotDot
        ) {
            self.bump();
            if Self::can_start_unary(self.kind()) {
                return self.unary();
            }
            let hi = self.toks[self.pos - 1].span.hi;
            return Expr::Ident(Span::new(self.file, hi, hi));
        }
        // Paramless / inferred-param lambda: `=> expr` / `=> { … }`
        // (also the operand of `scope => …`).
        if self.at(TokenKind::FatArrow) {
            return self.lambda_body(lo);
        }
        if let Some(op) = self.peek_unary_op() {
            self.bump();
            let operand = self.unary();
            return Expr::Unary {
                span: self.finish(lo),
                op,
                operand: Box::new(operand),
            };
        }
        if let Some(kw) = self.peek_prefix_kw() {
            self.bump();
            // optional qualifier: `new:alloc`, `delete:null`, `scope:mixin`,
            // or `scope::` (outer/enclosing-scope marker).
            let qualifier = if self.eat(TokenKind::ColonColon) {
                None // `scope::` — enclosing scope; no named qualifier
            } else if self.eat(TokenKind::Colon) {
                let q = self.cur().span;
                if matches!(self.kind(), TokenKind::Ident | TokenKind::Keyword(_)) {
                    self.bump();
                    Some(q)
                } else {
                    self.error("expected qualifier after `:`");
                    None
                }
            } else {
                None
            };
            let operand = self.unary();
            // Allocation types can carry trailing pointer suffix that the
            // expression grammar doesn't see: `new uint8[size]*`.
            if matches!(
                kw,
                PrefixKw::New | PrefixKw::Scope | PrefixKw::Append | PrefixKw::Box
            ) {
                while self.at(TokenKind::Star) {
                    self.bump();
                }
                // Constructor args after a pointer-suffixed allocation
                // type: `append uint8[size]*(?)`.
                if self.at(TokenKind::LParen) {
                    self.bump();
                    let _ = self.arg_list(TokenKind::RParen);
                    self.expect(TokenKind::RParen, "`)` to close allocation args");
                }
                // Object/collection initializer: `new T() { a = 1, b }`.
                if self.at(TokenKind::LBrace) {
                    self.consume_initializer();
                }
                // Allocation destructor: `scope T() ~ delete _;`,
                // `scope [&]() => {…} ~ { b++ };` — runs on scope exit. Block
                // or expression form; consumed for now.
                if self.eat(TokenKind::Tilde) {
                    if self.at(TokenKind::LBrace) {
                        self.skip_balanced(TokenKind::LBrace, TokenKind::RBrace);
                    } else {
                        let _ = self.expr();
                    }
                }
            }
            return Expr::Prefix {
                span: self.finish(lo),
                kw,
                qualifier,
                operand: Box::new(operand),
            };
        }
        let primary = self.primary();
        self.postfix(primary)
    }

    fn postfix(&mut self, mut e: Expr) -> Expr {
        let lo = e.span().lo;
        loop {
            match self.kind() {
                // `.` member access, and `->` pointer-member access
                // (`rcStr->Length`) — treated identically here.
                TokenKind::Dot | TokenKind::Arrow => {
                    self.bump();
                    // Beef friend-access list `.[Friend]Name` — consume
                    // and discard the bracketed attribute set for now.
                    if self.eat(TokenKind::LBracket) {
                        while !self.at(TokenKind::RBracket) && !self.at(TokenKind::Eof) {
                            self.bump();
                        }
                        self.eat(TokenKind::RBracket);
                    }
                    // `a.0` / `a.1` — tuple field access (numeric member).
                    // `inst.this(…)` — explicit constructor call; `.base`
                    // member — accept the `this`/`base` keywords as names.
                    let name = if self.at(TokenKind::Int)
                        || self.at_kw(Keyword::This)
                        || self.at_kw(Keyword::Base)
                    {
                        self.bump().span
                    } else {
                        self.expect_ident_span("member name")
                    };
                    e = Expr::Member {
                        span: self.finish(lo),
                        base: Box::new(e),
                        name,
                        conditional: false,
                    };
                }
                TokenKind::QuestionDot => {
                    self.bump();
                    let name = self.expect_ident_span("member name");
                    e = Expr::Member {
                        span: self.finish(lo),
                        base: Box::new(e),
                        name,
                        conditional: true,
                    };
                }
                // `global::Foo` / `A::B` — `::` namespace qualifier,
                // treated like `.` for parsing.
                TokenKind::ColonColon => {
                    self.bump();
                    let name = self.expect_ident_span("name after `::`");
                    e = Expr::Member {
                        span: self.finish(lo),
                        base: Box::new(e),
                        name,
                        conditional: false,
                    };
                }
                // `obj..Method()` — Beef cascade operator; parses like a
                // member access (cascade semantics resolved in sema).
                TokenKind::DotDot if matches!(self.nth_kind(1), TokenKind::Ident) => {
                    self.bump();
                    let name = self.bump().span;
                    e = Expr::Member {
                        span: self.finish(lo),
                        base: Box::new(e),
                        name,
                        conditional: false,
                    };
                }
                TokenKind::LParen => {
                    // An inferred-ctor `.()` may carry an object
                    // initializer (`.() { mA = 1 }`); a plain call never
                    // does, so we only look for `{` in the dot-ctor case.
                    let is_dot_ctor = matches!(&e, Expr::DotIdent { .. });
                    self.bump();
                    let args = self.arg_list(TokenKind::RParen);
                    self.expect(TokenKind::RParen, "`)` to close call");
                    e = Expr::Call {
                        span: self.finish(lo),
                        callee: Box::new(e),
                        args,
                    };
                    if is_dot_ctor && self.at(TokenKind::LBrace) {
                        self.consume_initializer();
                    }
                }
                TokenKind::LBracket => {
                    self.bump();
                    let args = self.arg_list(TokenKind::RBracket);
                    self.expect(TokenKind::RBracket, "`]` to close index");
                    e = Expr::Index {
                        span: self.finish(lo),
                        base: Box::new(e),
                        args,
                    };
                }
                TokenKind::PlusPlus => {
                    self.bump();
                    e = Expr::PostInc {
                        span: self.finish(lo),
                        operand: Box::new(e),
                    };
                }
                // `name!(args)` — Beef mixin/macro invocation. Modeled as
                // a Call for now (the `!` is lost at this phase).
                TokenKind::Bang if matches!(self.nth_kind(1), TokenKind::LParen) => {
                    self.bump(); // !
                    self.bump(); // (
                    let args = self.arg_list(TokenKind::RParen);
                    self.expect(TokenKind::RParen, "`)` to close mixin call");
                    e = Expr::Call {
                        span: self.finish(lo),
                        callee: Box::new(e),
                        args,
                    };
                }
                // `name!<T>(args)` — forced generic mixin call (`Get!<int>()`).
                // The `!` disambiguates, so the `<…>` is unambiguously a
                // generic-arg list; the following `(` becomes the call.
                TokenKind::Bang if matches!(self.nth_kind(1), TokenKind::Lt) => {
                    self.bump(); // !
                    let args = self.type_args();
                    e = Expr::Generic {
                        span: self.finish(lo),
                        base: Box::new(e),
                        args,
                    };
                }
                TokenKind::MinusMinus => {
                    self.bump();
                    e = Expr::PostDec {
                        span: self.finish(lo),
                        operand: Box::new(e),
                    };
                }
                TokenKind::Lt if Self::can_be_generic_base(&e) => {
                    if let Some(generic) = self.try_generic(&e, lo) {
                        e = generic;
                    } else {
                        break; // not a generic — let the binary loop handle `<`
                    }
                }
                // Object / collection initializer: `StructB { mA = 2 }`,
                // `new T() { 1, 2, 3 }`. Only after a constructible base; the
                // initializer is consumed and dropped for now (a later sprint
                // records it). Control-flow bodies never reach here — their
                // conditions are parenthesised, so the `{` follows the `)`.
                TokenKind::LBrace if !self.suppress_init && self.initializer_follows(&e) => {
                    self.consume_initializer();
                }
                _ => break,
            }
        }
        e
    }

    /// Whether a `{ … }` immediately after expr `e` (current token is `{`)
    /// is an object/collection initializer rather than an unrelated block.
    ///
    /// Allocations and call/generic/dot-ctor results take an initializer
    /// unconditionally (`new T() { … }`, `T<G> { … }`, `.() { … }`). A bare
    /// `Ident`/`Member` is the ambiguous case (`StructB { mA = 2 }` vs. a
    /// following block), so it only counts when the brace *looks* like an
    /// initializer: empty `{}`, or it opens with `ident =`.
    fn initializer_follows(&self, e: &Expr) -> bool {
        match e {
            Expr::Generic { .. }
            | Expr::Call { .. }
            | Expr::Index { .. }
            | Expr::DotIdent { .. }
            | Expr::Prefix { .. } => true,
            Expr::Ident(_) | Expr::Member { .. } => {
                self.nth_kind(1) == TokenKind::RBrace
                    || (self.nth_kind(1) == TokenKind::Ident
                        && self.nth_kind(2) == TokenKind::Assign)
            }
            _ => false,
        }
    }

    /// Only `Ident`, `Member`, and `Generic` (chained) can sensibly be
    /// the base of a generic-arg instantiation.
    fn can_be_generic_base(e: &Expr) -> bool {
        matches!(
            e,
            Expr::Ident(_) | Expr::Member { .. } | Expr::Generic { .. }
        )
    }

    /// Speculatively parse `<typelist>`; commit only if the token after
    /// `>` is in the generic-follow set. On failure, restore parser
    /// state (including any `>>`-splits).
    fn try_generic(&mut self, base: &Expr, lo: u32) -> Option<Expr> {
        let save = self.save();
        let args = self.type_args();
        if self.diagnostics.len() > save.diag_len || !Self::is_generic_follow(self.kind()) {
            self.restore(save);
            return None;
        }
        Some(Expr::Generic {
            span: self.finish(lo),
            base: Box::new(base.clone()),
            args,
        })
    }

    /// The token kinds that can legitimately follow a generic-arg list in
    /// expression position. Anything else means the `<…>` was actually a
    /// pair of comparisons. (Standard Roslyn-style heuristic, trimmed.)
    fn is_generic_follow(k: TokenKind) -> bool {
        matches!(
            k,
            TokenKind::LParen
                | TokenKind::RParen
                | TokenKind::LBracket
                | TokenKind::RBracket
                | TokenKind::LBrace
                | TokenKind::RBrace
                | TokenKind::Dot
                | TokenKind::QuestionDot
                | TokenKind::Comma
                | TokenKind::Semicolon
                | TokenKind::Colon
                | TokenKind::Question
                | TokenKind::Assign
                | TokenKind::EqEq
                | TokenKind::NotEq
                | TokenKind::FatArrow
                | TokenKind::Eof
        )
    }

    /// Parse a lambda body after `=>` (current token is `=>`). `lo` is the
    /// lambda's start offset (covering any params already consumed).
    fn lambda_body(&mut self, lo: u32) -> Expr {
        self.bump(); // =>
        let body = if self.at(TokenKind::LBrace) {
            self.block()
        } else {
            let blo = self.start();
            let e = self.expr();
            Stmt::Expr {
                span: self.finish(blo),
                expr: e,
            }
        };
        Expr::Lambda {
            span: self.finish(lo),
            body: Box::new(body),
        }
    }

    /// Consume an object/collection initializer `{ a = 1, b, … }`
    /// (contents discarded for now — recovered in sema). Assumes the
    /// current token is `{`.
    fn consume_initializer(&mut self) {
        self.bump(); // {
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let before = self.pos;
            // Indexed collection-initializer entry: `[key] = value`.
            if self.at(TokenKind::LBracket) {
                self.skip_balanced(TokenKind::LBracket, TokenKind::RBracket);
                if self.eat(TokenKind::Assign) {
                    let _ = self.expr();
                }
            } else {
                let _ = self.expr();
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
            if self.pos == before {
                break;
            }
        }
        self.expect(TokenKind::RBrace, "`}` to close initializer");
    }

    fn arg_list(&mut self, close: TokenKind) -> Vec<Expr> {
        // Inside `()`/`[]`, a trailing `{` is an initializer again, even if
        // we entered while suppressing one for an enclosing construct.
        let prev_suppress = self.suppress_init;
        self.suppress_init = false;
        let mut args = Vec::new();
        while !self.at(close) && !self.at(TokenKind::Eof) {
            let before = self.pos;
            args.push(self.arg());
            if !self.eat(TokenKind::Comma) {
                break;
            }
            if self.pos == before {
                break; // safety: guarantee progress
            }
        }
        self.suppress_init = prev_suppress;
        args
    }

    /// One argument: a plain expression, or a named argument `name: value`
    /// (Beef call syntax). A leading `Ident :` at argument start is always a
    /// named argument — a ternary would carry a `?`, and `::` is one token.
    fn arg(&mut self) -> Expr {
        if self.at(TokenKind::Ident) && self.nth_kind(1) == TokenKind::Colon {
            let name = self.bump().span; // name
            self.bump(); // :
            let value = self.expr();
            let span = Span::new(self.file, name.lo, value.span().hi);
            return Expr::Named {
                span,
                name,
                value: Box::new(value),
            };
        }
        let value = self.expr();
        // Typed binding pattern in argument position: `Type name`, used in
        // `case` patterns (`case .Range(let lo, int hi):`). After an argument
        // expression a bare identifier can only be such a binding name (args
        // are comma-separated, so a valid arg is otherwise followed by `,`
        // or `)`); consume it (dropped for now).
        if self.at(TokenKind::Ident) {
            self.bump();
        }
        value
    }

    fn primary(&mut self) -> Expr {
        let span = self.cur().span;
        match self.kind() {
            TokenKind::Int => {
                self.bump();
                Expr::Int(span)
            }
            TokenKind::Float => {
                self.bump();
                Expr::Float(span)
            }
            TokenKind::Char => {
                self.bump();
                Expr::Char(span)
            }
            TokenKind::Str | TokenKind::VerbatimStr | TokenKind::InterpStr => {
                self.bump();
                Expr::Str(span)
            }
            TokenKind::Ident => {
                self.bump();
                // Single-parameter lambda: `x => body`.
                if self.at(TokenKind::FatArrow) {
                    return self.lambda_body(span.lo);
                }
                Expr::Ident(span)
            }
            TokenKind::Keyword(k) => self.primary_keyword(k, span),
            TokenKind::LParen => self.paren_or_cast(),
            // Lambda capture clause: `[&] (a,b) => …`, `[=] => …`,
            // `[x, &y] z => …`; or an attributed block `[IgnoreErrors] { … }`.
            // The bracket clause is consumed and dropped; what follows is a
            // block expression or a lambda.
            TokenKind::LBracket => {
                self.skip_capture_clause();
                if self.at(TokenKind::LBrace) {
                    self.block_expr(span.lo)
                } else {
                    self.unary()
                }
            }
            // Block expression: `{ stmts; result }` used in expression
            // position (an `if` condition, an argument, an assignment RHS).
            // Consumed and dropped for now; placeholder primary stands in.
            TokenKind::LBrace => self.block_expr(span.lo),
            // `?` — Beef "uninitialized" placeholder (e.g. `int x = ?;`).
            TokenKind::Question => {
                self.bump();
                Expr::Ident(span)
            }
            // Leading-`.` shorthand: `.Variant` (enum case), `.(args)`
            // (inferred-type ctor), `.[i]` (inferred-type indexer). The
            // postfix loop handles the call/index after we emit the
            // leading-dot primary.
            TokenKind::Dot => {
                let lo = span.lo;
                self.bump(); // .
                if self.at(TokenKind::Ident) {
                    let name = self.bump().span;
                    Expr::DotIdent {
                        span: self.finish(lo),
                        name,
                    }
                } else {
                    Expr::DotIdent {
                        span: self.finish(lo),
                        name: span,
                    }
                }
            }
            _ => {
                self.error("expected an expression");
                // Recovery: consume one token so callers make progress.
                self.bump();
                Expr::Error(span)
            }
        }
    }

    /// Consume a balanced `[ … ]` lambda capture clause (current token is
    /// `[`). Nested brackets are tracked so a capture like `[obj[0]]`
    /// closes correctly.
    fn skip_capture_clause(&mut self) {
        self.skip_balanced(TokenKind::LBracket, TokenKind::RBracket);
    }

    /// A block used in expression position: `{ stmts; result }` (an `if`
    /// condition, argument, or RHS). The statements are parsed (so inner
    /// errors are still reported); the block's value is a placeholder.
    fn block_expr(&mut self, lo: u32) -> Expr {
        let _ = self.block();
        Expr::Ident(self.finish(lo))
    }

    /// Consume a balanced `open … close` run (current token is `open`).
    fn skip_balanced(&mut self, open: TokenKind, close: TokenKind) {
        let mut depth = 0u32;
        loop {
            let k = self.kind();
            if k == open {
                depth += 1;
                self.bump();
            } else if k == close {
                self.bump();
                depth -= 1;
                if depth == 0 {
                    break;
                }
            } else if k == TokenKind::Eof {
                break;
            } else {
                self.bump();
            }
        }
    }

    /// `(Type)expr` C-style cast vs. `(expr)` parenthesised. We
    /// speculatively try to parse a type followed by `)` and a
    /// unary-startable token; if that fails we backtrack to a paren.
    fn paren_or_cast(&mut self) -> Expr {
        let save = self.save();
        let lo = self.start();
        self.bump(); // (
        let ty = self.ty();
        let could_be_cast = self.diagnostics.len() == save.diag_len && self.at(TokenKind::RParen);
        if could_be_cast {
            self.bump(); // )
            // A bare-identifier "type" only commits to a cast before an
            // unambiguous unary start. A *definite* type (pointer / generic
            // / array / tuple) also commits before `& * - + ~ ! ++ --`
            // (e.g. `(char8*)&mVal`, `(int)-1`).
            let definite = Self::is_definite_type(&ty);
            let commit = Self::can_start_unary(self.kind())
                || (definite
                    && matches!(
                        self.kind(),
                        TokenKind::Star
                            | TokenKind::Amp
                            | TokenKind::Minus
                            | TokenKind::Plus
                            | TokenKind::PlusPlus
                            | TokenKind::MinusMinus
                    ));
            if commit {
                let operand = self.unary();
                return Expr::Cast {
                    span: self.finish(lo),
                    ty,
                    operand: Box::new(operand),
                };
            }
        }
        self.restore(save);
        self.bump(); // (
        // Empty `()` — unit, or a zero-param lambda `() => …`.
        if self.at(TokenKind::RParen) {
            self.bump();
            if self.at(TokenKind::FatArrow) {
                return self.lambda_body(lo);
            }
            return Expr::Tuple {
                span: self.finish(lo),
                elems: Vec::new(),
            };
        }
        // Use `arg` so tuple elements can be named (`(x: 3, y: 4)`).
        let inner = self.arg();
        // Tuple literal `(a, b, …)` or lambda params `(a, b) => …`.
        if self.at(TokenKind::Comma) {
            let mut elems = vec![inner];
            while self.eat(TokenKind::Comma) {
                if self.at(TokenKind::RParen) {
                    break;
                }
                let before = self.pos;
                elems.push(self.arg());
                if self.pos == before {
                    break;
                }
            }
            self.expect(TokenKind::RParen, "`)` to close tuple literal");
            if self.at(TokenKind::FatArrow) {
                return self.lambda_body(lo);
            }
            return Expr::Tuple {
                span: self.finish(lo),
                elems,
            };
        }
        self.expect(TokenKind::RParen, "`)` to close parenthesized expression");
        // `(x) => …` — single-param lambda.
        if self.at(TokenKind::FatArrow) {
            return self.lambda_body(lo);
        }
        Expr::Paren {
            span: self.finish(lo),
            inner: Box::new(inner),
        }
    }

    /// A "definite" type — one whose syntax can't be a plain expression
    /// (pointer/nullable/array/sized/tuple, or a generic-instantiated
    /// path). Used to widen the C-style-cast follow set.
    fn is_definite_type(t: &Type) -> bool {
        match t {
            Type::Pointer { .. }
            | Type::Nullable { .. }
            | Type::Array { .. }
            | Type::Sized { .. }
            | Type::Tuple { .. }
            | Type::Function { .. }
            | Type::Var(_) => true,
            Type::Path { segments, .. } => segments.iter().any(|s| !s.args.is_empty()),
            Type::Error(_) => false,
        }
    }

    /// Token kinds that commit `(X) …` to a C-style cast. Deliberately
    /// excludes tokens that are also binary/postfix operators (dot, star,
    /// amp, plus, minus, increment, decrement): `(a).b`, `(a)*b`, `(a)-b`
    /// are member-access / arithmetic, not casts. Only unambiguous unary
    /// starts qualify.
    fn can_start_unary(k: TokenKind) -> bool {
        matches!(
            k,
            TokenKind::Int
                | TokenKind::Float
                | TokenKind::Char
                | TokenKind::Str
                | TokenKind::VerbatimStr
                | TokenKind::InterpStr
                | TokenKind::Ident
                | TokenKind::LParen
                | TokenKind::Bang
                | TokenKind::Tilde
                | TokenKind::Keyword(_)
        )
    }

    fn primary_keyword(&mut self, k: Keyword, span: Span) -> Expr {
        match k {
            Keyword::True | Keyword::False => {
                self.bump();
                Expr::Bool(span)
            }
            Keyword::Null => {
                self.bump();
                Expr::Null(span)
            }
            Keyword::This => {
                self.bump();
                Expr::This(span)
            }
            Keyword::Base => {
                self.bump();
                Expr::Base(span)
            }
            // `sizeof`/`typeof`/`alignof`/`strideof` take a *type* argument,
            // which can use type-only syntax an expression can't (`char8*`,
            // `int[]`, `List<T>`). Parse `( type )` and drop it for now (the
            // result is a placeholder primary; the type is recovered later).
            Keyword::SizeOf | Keyword::AlignOf | Keyword::StrideOf | Keyword::TypeOf => {
                self.bump();
                if self.eat(TokenKind::LParen) {
                    let _ty = self.ty();
                    self.expect(TokenKind::RParen, "`)` after type argument");
                }
                Expr::Ident(span)
            }
            // These take an *expression* (or optional) argument, so the
            // postfix `(…)` Call handles them: `nameof(x)`, `default(T)`,
            // `comptype(e)`. Treated as a primary. `var`/`let` below cover
            // pattern bindings (e.g. `case .Ok(var val):`). `fallthrough` is
            // a bare control-flow primary used as a statement in `switch`.
            Keyword::NameOf
            | Keyword::Comptype
            | Keyword::Decltype
            | Keyword::RetType
            | Keyword::Default
            | Keyword::Fallthrough => {
                self.bump();
                Expr::Ident(span)
            }
            // `function`/`delegate` type in expression position — the RHS of
            // `is`/`as` (`obj is delegate int(int a, int b)`). Parse the
            // function type and drop it; placeholder primary stands in.
            Keyword::Function | Keyword::Delegate => {
                let _ty = self.ty();
                Expr::Ident(span)
            }
            // `var x` / `let val` — binding patterns (used in `case`
            // patterns and `if (var x = …)`); also `var ref val` / `let mut x`
            // with a binding modifier. Consume the modifier and bound name.
            Keyword::Var | Keyword::Let => {
                self.bump();
                let _ = self.eat_kw(Keyword::Ref)
                    || self.eat_kw(Keyword::Mut)
                    || self.eat_kw(Keyword::Out);
                if self.at(TokenKind::Ident) {
                    self.bump();
                }
                Expr::Ident(span)
            }
            _ => {
                self.error("expected an expression");
                self.bump();
                Expr::Error(span)
            }
        }
    }

    fn expect_ident_span(&mut self, what: &str) -> Span {
        let span = self.cur().span;
        // Accept a verbatim/ordinary identifier; also tolerate a keyword
        // used as a member name in recovery.
        if matches!(self.kind(), TokenKind::Ident) {
            self.bump();
            span
        } else {
            self.error(&format!("expected {what}"));
            span
        }
    }

    // ── operator tables ─────────────────────────────────────────────────

    fn peek_unary_op(&self) -> Option<UnOp> {
        Some(match self.kind() {
            TokenKind::Minus => UnOp::Neg,
            TokenKind::Plus => UnOp::Pos,
            TokenKind::Bang => UnOp::Not,
            TokenKind::Tilde => UnOp::BitNot,
            TokenKind::PlusPlus => UnOp::PreInc,
            TokenKind::MinusMinus => UnOp::PreDec,
            TokenKind::Star => UnOp::Deref,
            TokenKind::Amp => UnOp::AddrOf,
            _ => return None,
        })
    }

    fn peek_prefix_kw(&self) -> Option<PrefixKw> {
        let TokenKind::Keyword(k) = self.kind() else {
            return None;
        };
        Some(match k {
            Keyword::New => PrefixKw::New,
            Keyword::Scope => PrefixKw::Scope,
            Keyword::Append => PrefixKw::Append,
            Keyword::Delete => PrefixKw::Delete,
            Keyword::Box => PrefixKw::Box,
            Keyword::Ref => PrefixKw::Ref,
            Keyword::Out => PrefixKw::Out,
            Keyword::Mut => PrefixKw::Mut,
            Keyword::In => PrefixKw::In,
            Keyword::Params => PrefixKw::Params,
            _ => return None,
        })
    }

    fn peek_binop(&self) -> Option<BinOp> {
        Some(match self.kind() {
            TokenKind::Star | TokenKind::AmpStar => BinOp::Mul,
            TokenKind::Slash => BinOp::Div,
            TokenKind::Percent => BinOp::Mod,
            TokenKind::Plus | TokenKind::AmpPlus => BinOp::Add,
            TokenKind::Minus | TokenKind::AmpMinus => BinOp::Sub,
            TokenKind::Shl => BinOp::Shl,
            TokenKind::Shr => BinOp::Shr,
            TokenKind::Amp => BinOp::BitAnd,
            TokenKind::Caret => BinOp::BitXor,
            TokenKind::Pipe => BinOp::BitOr,
            TokenKind::DotDotLess => BinOp::Range,
            TokenKind::DotDotDot => BinOp::ClosedRange,
            TokenKind::Spaceship => BinOp::Compare,
            TokenKind::Lt => BinOp::Lt,
            TokenKind::Gt => BinOp::Gt,
            TokenKind::Le => BinOp::Le,
            TokenKind::Ge => BinOp::Ge,
            TokenKind::EqEq | TokenKind::StrictEq => BinOp::Eq,
            TokenKind::NotEq | TokenKind::StrictNeq => BinOp::Ne,
            TokenKind::AmpAmp => BinOp::And,
            TokenKind::PipePipe => BinOp::Or,
            TokenKind::QuestionQuestion => BinOp::NullCoalesce,
            TokenKind::Keyword(Keyword::Is) => BinOp::Is,
            TokenKind::Keyword(Keyword::As) => BinOp::As,
            TokenKind::Keyword(Keyword::Case) => BinOp::Case,
            _ => return None,
        })
    }

    fn peek_assign_op(&self) -> Option<AssignOp> {
        Some(match self.kind() {
            TokenKind::Assign => AssignOp::Assign,
            TokenKind::PlusEq | TokenKind::AmpPlusEq => AssignOp::Add,
            TokenKind::MinusEq | TokenKind::AmpMinusEq => AssignOp::Sub,
            TokenKind::StarEq | TokenKind::AmpStarEq => AssignOp::Mul,
            TokenKind::SlashEq => AssignOp::Div,
            TokenKind::PercentEq => AssignOp::Mod,
            TokenKind::AmpEq => AssignOp::And,
            TokenKind::PipeEq => AssignOp::Or,
            TokenKind::CaretEq => AssignOp::Xor,
            TokenKind::ShlEq => AssignOp::Shl,
            TokenKind::ShrEq => AssignOp::Shr,
            TokenKind::QuestionQuestionEq => AssignOp::NullCoalesce,
            _ => return None,
        })
    }

    // ── statements ──────────────────────────────────────────────────────

    fn stmt(&mut self) -> Stmt {
        // Attributes on a statement: `[Inline] { … }`, `[IgnoreErrors] …`.
        if self.at(TokenKind::LBracket) {
            let _ = self.attributes();
            return self.stmt();
        }
        // Labeled statement: `label: stmt` (e.g. `outer: for (…)`).
        if self.at(TokenKind::Ident) && self.nth_kind(1) == TokenKind::Colon {
            self.bump(); // label
            self.bump(); // :
            return self.stmt();
        }
        match self.kind() {
            TokenKind::LBrace => self.block(),
            TokenKind::Semicolon => {
                let s = self.cur().span;
                self.bump();
                Stmt::Empty(s)
            }
            TokenKind::Keyword(Keyword::If) => self.if_stmt(),
            TokenKind::Keyword(Keyword::While) => self.while_stmt(),
            TokenKind::Keyword(Keyword::Do) | TokenKind::Keyword(Keyword::Repeat) => {
                self.do_while_stmt()
            }
            TokenKind::Keyword(Keyword::For) => self.for_stmt(),
            TokenKind::Keyword(Keyword::Return) => self.return_stmt(),
            TokenKind::Keyword(Keyword::Break) => self.break_continue(true),
            TokenKind::Keyword(Keyword::Continue) => self.break_continue(false),
            TokenKind::Keyword(Keyword::Defer) => self.defer_stmt(),
            TokenKind::Keyword(Keyword::Var) | TokenKind::Keyword(Keyword::Let) => self.local(true),
            TokenKind::Keyword(Keyword::Switch) => self.switch_stmt(),
            // `using (resource) stmt` — RAII scope (modeled as a block).
            TokenKind::Keyword(Keyword::Using) if self.nth_kind(1) == TokenKind::LParen => {
                let lo = self.start();
                self.bump(); // using
                self.bump(); // (
                if self.at_kw(Keyword::Var) || self.at_kw(Keyword::Let) {
                    let _ = self.local(false);
                } else {
                    let _ = self.expr();
                }
                self.expect(TokenKind::RParen, "`)` after using-resource");
                let body = self.stmt();
                Stmt::Block {
                    span: self.finish(lo),
                    stmts: vec![body],
                }
            }
            // Local `mixin Name(params) body` inside a method.
            TokenKind::Keyword(Keyword::Mixin) => {
                let lo = self.start();
                self.bump(); // mixin
                let name = if self.at(TokenKind::Ident) {
                    self.bump().span
                } else {
                    self.error("expected mixin name");
                    self.cur().span
                };
                let generic_params = if self.at(TokenKind::Lt) {
                    self.generic_params()
                } else {
                    Vec::new()
                };
                let params = if self.at(TokenKind::LParen) {
                    self.params()
                } else {
                    Vec::new()
                };
                let body = if self.at(TokenKind::LBrace) {
                    self.block()
                } else if self.eat(TokenKind::FatArrow) {
                    let blo = self.start();
                    let e = self.expr();
                    self.expect(TokenKind::Semicolon, "`;` after `=> expr` body");
                    Stmt::Expr {
                        span: self.finish(blo),
                        expr: e,
                    }
                } else {
                    self.expect(TokenKind::Semicolon, "mixin body");
                    Stmt::Empty(self.cur().span)
                };
                Stmt::LocalFunction {
                    span: self.finish(lo),
                    return_ty: Type::Var(name),
                    name,
                    generic_params,
                    params,
                    body: Box::new(body),
                }
            }
            // `const`/`readonly`/`static` local: consume the modifier(s)
            // then parse `Type name = init;` (or `var`/`let`).
            TokenKind::Keyword(Keyword::Const)
            | TokenKind::Keyword(Keyword::ReadOnly)
            | TokenKind::Keyword(Keyword::Static)
                if matches!(
                    self.nth_kind(1),
                    TokenKind::Ident
                        | TokenKind::Keyword(Keyword::Var)
                        | TokenKind::Keyword(Keyword::Let)
                ) =>
            {
                let lo = self.start();
                while matches!(
                    self.kind(),
                    TokenKind::Keyword(Keyword::Const)
                        | TokenKind::Keyword(Keyword::ReadOnly)
                        | TokenKind::Keyword(Keyword::Static)
                ) {
                    self.bump();
                }
                if self.at_kw(Keyword::Var) || self.at_kw(Keyword::Let) {
                    self.local(true)
                } else {
                    let ty = self.ty();
                    let name = self.expect_ident_span("variable name");
                    let init = if self.eat(TokenKind::Assign) {
                        Some(self.expr())
                    } else {
                        None
                    };
                    self.expect(TokenKind::Semicolon, "`;` after local variable");
                    Stmt::Local {
                        span: self.finish(lo),
                        is_let: false,
                        ty: Some(ty),
                        name,
                        init,
                    }
                }
            }
            _ => {
                // `ref Type name = …` — a ref-typed local. Speculatively
                // consume `ref` and try a typed local; otherwise it's a
                // `ref expr;` statement.
                if self.at_kw(Keyword::Ref) {
                    let save = self.save();
                    self.bump(); // ref
                    if let Some(s) = self.try_typed_local() {
                        return s;
                    }
                    self.restore(save);
                }
                if let Some(s) = self.try_local_function() {
                    s
                } else if let Some(s) = self.try_typed_local() {
                    s
                } else {
                    self.expr_stmt()
                }
            }
        }
    }

    /// Speculatively parse a local function declaration nested in a
    /// method body: `Type Name [<G…>] (params) { body }` (or `=> expr;` /
    /// `;`).
    fn try_local_function(&mut self) -> Option<Stmt> {
        if !matches!(
            self.kind(),
            TokenKind::Ident
                | TokenKind::LParen
                | TokenKind::Keyword(
                    Keyword::Var
                        | Keyword::Delegate
                        | Keyword::Function
                        | Keyword::Comptype
                        | Keyword::Decltype
                        | Keyword::RetType
                        | Keyword::AllocType
                )
        ) {
            return None;
        }
        let lo = self.start();
        let save = self.save();
        let return_ty = self.ty();
        if self.diagnostics.len() > save.diag_len || !self.at(TokenKind::Ident) {
            self.restore(save);
            return None;
        }
        // Local-fn signature: `Ident [<G…>] (` — we require `(` after the
        // name (with optional generic params in between).
        let after_name = self.nth_kind(1);
        let looks_like_fn = match after_name {
            TokenKind::LParen => true,
            TokenKind::Lt => {
                // Could be a generic-param list, but we don't speculate
                // that hard — accept it as a local-fn candidate.
                true
            }
            _ => false,
        };
        if !looks_like_fn {
            self.restore(save);
            return None;
        }
        let name = self.bump().span;
        let generic_params = if self.at(TokenKind::Lt) {
            self.generic_params()
        } else {
            Vec::new()
        };
        if !self.at(TokenKind::LParen) {
            self.restore(save);
            return None;
        }
        let params = self.params();
        // Optional `where` clauses on local fns.
        let _ = self.where_clauses();
        let body: Stmt = if self.at(TokenKind::LBrace) {
            self.block()
        } else if self.eat(TokenKind::FatArrow) {
            let body_lo = self.start();
            let e = self.expr();
            self.expect(TokenKind::Semicolon, "`;` after `=> expr` body");
            Stmt::Expr {
                span: self.finish(body_lo),
                expr: e,
            }
        } else {
            self.expect(
                TokenKind::Semicolon,
                "`;`, `{ … }`, or `=> expr;` for function body",
            );
            Stmt::Empty(self.cur().span)
        };
        Some(Stmt::LocalFunction {
            span: self.finish(lo),
            return_ty,
            name,
            generic_params,
            params,
            body: Box::new(body),
        })
    }

    /// Speculatively parse a typed local declaration `Type name [= init];`.
    /// Returns `None` (restoring state) if the lookahead doesn't fit.
    fn try_typed_local(&mut self) -> Option<Stmt> {
        self.try_typed_local_init(true)
    }

    fn try_typed_local_init(&mut self, consume_semi: bool) -> Option<Stmt> {
        // Only attempt if the current token could plausibly start a type.
        if !matches!(
            self.kind(),
            TokenKind::Ident
                | TokenKind::LParen
                | TokenKind::Keyword(
                    Keyword::Var
                        | Keyword::Delegate
                        | Keyword::Function
                        | Keyword::Comptype
                        | Keyword::Decltype
                        | Keyword::RetType
                        | Keyword::AllocType
                )
        ) {
            return None;
        }
        let lo = self.start();
        let save = self.save();
        let ty = self.ty();
        // Demand: no errors, current is Ident, next is `=`/`;`/`,`/`)`.
        if self.diagnostics.len() > save.diag_len || !self.at(TokenKind::Ident) {
            self.restore(save);
            return None;
        }
        let next = self.nth_kind(1);
        if !matches!(
            next,
            TokenKind::Assign | TokenKind::Semicolon | TokenKind::Comma | TokenKind::RParen
        ) {
            self.restore(save);
            return None;
        }
        let name = self.bump().span;
        let init = if self.eat(TokenKind::Assign) {
            Some(self.expr())
        } else {
            None
        };
        // Multiple declarators: `int a, b = 2, c;` — keep first, consume rest.
        if consume_semi {
            while self.eat(TokenKind::Comma) {
                if !self.at(TokenKind::Ident) {
                    break;
                }
                self.bump(); // name
                if self.eat(TokenKind::Assign) {
                    let _ = self.expr();
                }
            }
            self.expect(TokenKind::Semicolon, "`;` after local variable");
        }
        Some(Stmt::Local {
            span: self.finish(lo),
            is_let: false,
            ty: Some(ty),
            name,
            init,
        })
    }

    fn switch_stmt(&mut self) -> Stmt {
        let lo = self.start();
        self.bump(); // switch
        self.expect(TokenKind::LParen, "`(` after `switch`");
        let scrutinee = self.expr();
        self.expect(TokenKind::RParen, "`)` after switch scrutinee");
        self.expect(TokenKind::LBrace, "`{` to open switch body");
        let mut arms = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let arm_lo = self.start();
            let pattern = if self.eat_kw(Keyword::Case) {
                let first = self.expr();
                // Multiple values per case: `case a, b, c:` — keep the
                // first, consume the rest.
                while self.eat(TokenKind::Comma) {
                    if self.at(TokenKind::Colon) {
                        break;
                    }
                    let before = self.pos;
                    let _ = self.expr();
                    if self.pos == before {
                        break;
                    }
                }
                // Optional `when <guard>` clause (consumed/dropped for now):
                // `case .Circle(let x, let y) when x == 10:`.
                if self.eat_kw(Keyword::When) {
                    let _ = self.expr();
                }
                Some(first)
            } else if self.eat_kw(Keyword::Default) {
                None
            } else {
                self.error("expected `case` or `default`");
                self.bump(); // safety: guarantee progress
                continue;
            };
            self.expect(TokenKind::Colon, "`:` after case/default label");
            let mut body = Vec::new();
            while !self.at(TokenKind::RBrace)
                && !self.at_kw(Keyword::Case)
                && !self.at_kw(Keyword::Default)
                && !self.at(TokenKind::Eof)
            {
                let before = self.pos;
                body.push(self.stmt());
                if self.pos == before {
                    self.bump();
                }
            }
            arms.push(SwitchArm {
                span: self.finish(arm_lo),
                pattern,
                body,
            });
        }
        self.expect(TokenKind::RBrace, "`}` to close switch");
        Stmt::Switch {
            span: self.finish(lo),
            scrutinee,
            arms,
        }
    }

    fn block(&mut self) -> Stmt {
        let lo = self.start();
        self.expect(TokenKind::LBrace, "`{`");
        let mut stmts = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let before = self.pos;
            stmts.push(self.stmt());
            if self.pos == before {
                self.bump(); // safety: guarantee progress
            }
        }
        self.expect(TokenKind::RBrace, "`}` to close block");
        Stmt::Block {
            span: self.finish(lo),
            stmts,
        }
    }

    /// A condition that may be a declaration: `if (Type name = expr)`,
    /// `while (Type name = expr)` (Beef binds `name` for the body). The
    /// `Type name =` prefix is consumed and the value becomes the condition
    /// (the binding is dropped for now). `var x = …` already parses via the
    /// `var`/`let` binding primary, so this targets the typed form.
    fn cond_expr(&mut self) -> Expr {
        let save = self.save();
        if self.at(TokenKind::Ident) {
            let _ty = self.ty();
            if self.diagnostics.len() == save.diag_len
                && self.at(TokenKind::Ident)
                && self.nth_kind(1) == TokenKind::Assign
            {
                self.bump(); // name
                self.bump(); // =
                return self.expr();
            }
            self.restore(save);
        }
        self.expr()
    }

    fn if_stmt(&mut self) -> Stmt {
        let lo = self.start();
        self.bump(); // if
        self.expect(TokenKind::LParen, "`(` after `if`");
        let cond = self.cond_expr();
        self.expect(TokenKind::RParen, "`)` after if-condition");
        let then = Box::new(self.stmt());
        let els = if self.eat_kw(Keyword::Else) {
            Some(Box::new(self.stmt()))
        } else {
            None
        };
        Stmt::If {
            span: self.finish(lo),
            cond,
            then,
            els,
        }
    }

    fn while_stmt(&mut self) -> Stmt {
        let lo = self.start();
        self.bump(); // while
        self.expect(TokenKind::LParen, "`(` after `while`");
        let cond = self.cond_expr();
        self.expect(TokenKind::RParen, "`)` after while-condition");
        let body = Box::new(self.stmt());
        Stmt::While {
            span: self.finish(lo),
            cond,
            body,
        }
    }

    fn do_while_stmt(&mut self) -> Stmt {
        let lo = self.start();
        self.bump(); // do / repeat
        let body = Box::new(self.stmt());
        self.expect(
            TokenKind::Keyword(Keyword::While),
            "`while` after loop body",
        );
        self.expect(TokenKind::LParen, "`(` after `while`");
        let cond = self.expr();
        self.expect(TokenKind::RParen, "`)` after while-condition");
        self.eat(TokenKind::Semicolon);
        Stmt::DoWhile {
            span: self.finish(lo),
            body,
            cond,
        }
    }

    fn for_stmt(&mut self) -> Stmt {
        let lo = self.start();
        self.bump(); // for
        self.expect(TokenKind::LParen, "`(` after `for`");

        // for-each: `(var? IDENT in EXPR)`
        let checkpoint = self.pos;
        let _ = self.eat_kw(Keyword::Var) || self.eat_kw(Keyword::Let);
        if self.at(TokenKind::Ident) && self.nth_kind(1) == TokenKind::Keyword(Keyword::In) {
            let name = self.bump().span;
            self.bump(); // in
            let iter = self.expr();
            self.expect(TokenKind::RParen, "`)` after for-each");
            let body = Box::new(self.stmt());
            return Stmt::ForEach {
                span: self.finish(lo),
                name,
                iter,
                body,
            };
        }
        self.pos = checkpoint; // not a for-each; rewind

        // Tuple-destructuring for-each: `for (var (a, b) in EXPR)`. The
        // binding pattern's span stands in for `name` (no pattern field yet).
        {
            let save = self.save();
            let _ = self.eat_kw(Keyword::Var) || self.eat_kw(Keyword::Let);
            if self.at(TokenKind::LParen) {
                let name_lo = self.cur().span.lo;
                self.skip_balanced(TokenKind::LParen, TokenKind::RParen);
                let name_hi = self.toks[self.pos - 1].span.hi;
                if self.eat_kw(Keyword::In) {
                    let name = Span::new(self.file, name_lo, name_hi);
                    let iter = self.expr();
                    self.expect(TokenKind::RParen, "`)` after for-each");
                    let body = Box::new(self.stmt());
                    return Stmt::ForEach {
                        span: self.finish(lo),
                        name,
                        iter,
                        body,
                    };
                }
            }
            self.restore(save);
        }

        // Typed for-each: `(Type IDENT in EXPR)` — e.g. `for (int i in 0..<10)`.
        // (The binding type is dropped for now, like the count-loop below;
        // a later sprint records it on `ForEach`.)
        {
            let save = self.save();
            let _ = self.eat_kw(Keyword::Var) || self.eat_kw(Keyword::Let);
            let _ty = self.ty();
            if self.diagnostics.len() == save.diag_len
                && self.at(TokenKind::Ident)
                && self.nth_kind(1) == TokenKind::Keyword(Keyword::In)
            {
                let name = self.bump().span;
                self.bump(); // in
                let iter = self.expr();
                self.expect(TokenKind::RParen, "`)` after for-each");
                let body = Box::new(self.stmt());
                return Stmt::ForEach {
                    span: self.finish(lo),
                    name,
                    iter,
                    body,
                };
            }
            self.restore(save);
        }

        // Beef count-loop without a type: `for (var? name < EXPR)` — e.g.
        // `for (let i < 2)`. The name directly precedes `<`.
        {
            let save = self.save();
            let _ = self.eat_kw(Keyword::Var) || self.eat_kw(Keyword::Let);
            if self.at(TokenKind::Ident) && self.nth_kind(1) == TokenKind::Lt {
                let name = self.bump().span;
                self.bump(); // <
                let iter = self.expr();
                self.expect(TokenKind::RParen, "`)` after for count-loop");
                let body = Box::new(self.stmt());
                return Stmt::ForEach {
                    span: self.finish(lo),
                    name,
                    iter,
                    body,
                };
            }
            self.restore(save);
        }

        // Beef count-loop: `for (var? Type name < EXPR)` — shorthand for
        // `for (Type name = 0; name < EXPR; name++)`. Modeled as ForEach
        // (the bound goes in `iter`; sema reinterprets).
        {
            let save = self.save();
            let _ = self.eat_kw(Keyword::Var) || self.eat_kw(Keyword::Let);
            let _ty = self.ty();
            if self.diagnostics.len() == save.diag_len
                && self.at(TokenKind::Ident)
                && self.nth_kind(1) == TokenKind::Lt
            {
                let name = self.bump().span;
                self.bump(); // <
                let iter = self.expr();
                self.expect(TokenKind::RParen, "`)` after for count-loop");
                let body = Box::new(self.stmt());
                return Stmt::ForEach {
                    span: self.finish(lo),
                    name,
                    iter,
                    body,
                };
            }
            self.restore(save);
        }

        // C-style: `(init; cond; update)`
        let init = if self.at(TokenKind::Semicolon) {
            None
        } else {
            Some(Box::new(self.for_init()))
        };
        self.expect(TokenKind::Semicolon, "`;` after for-init");
        let cond = if self.at(TokenKind::Semicolon) {
            None
        } else {
            Some(self.expr())
        };
        self.expect(TokenKind::Semicolon, "`;` after for-condition");
        let update = if self.at(TokenKind::RParen) {
            None
        } else {
            // Comma-separated update expressions: `for (…;…; i++, j--)`.
            // Keep the first; consume the rest.
            let first = self.expr();
            while self.eat(TokenKind::Comma) {
                if self.at(TokenKind::RParen) {
                    break;
                }
                let before = self.pos;
                let _ = self.expr();
                if self.pos == before {
                    break;
                }
            }
            Some(first)
        };
        self.expect(TokenKind::RParen, "`)` after for-clauses");
        let body = Box::new(self.stmt());
        Stmt::For {
            span: self.finish(lo),
            init,
            cond,
            update,
            body,
        }
    }

    /// A for-init: a `var`/`let` local without trailing `;`, a typed
    /// local (`int32 i = 0`) without trailing `;`, or an expression.
    fn for_init(&mut self) -> Stmt {
        if self.at_kw(Keyword::Var) || self.at_kw(Keyword::Let) {
            return self.local(false);
        }
        if let Some(s) = self.try_typed_local_init(false) {
            return s;
        }
        let lo = self.start();
        let e = self.expr();
        Stmt::Expr {
            span: self.finish(lo),
            expr: e,
        }
    }

    fn return_stmt(&mut self) -> Stmt {
        let lo = self.start();
        self.bump(); // return
        let value = if self.at(TokenKind::Semicolon) {
            None
        } else {
            Some(self.expr())
        };
        self.expect(TokenKind::Semicolon, "`;` after return");
        Stmt::Return {
            span: self.finish(lo),
            value,
        }
    }

    fn break_continue(&mut self, is_break: bool) -> Stmt {
        let lo = self.start();
        self.bump(); // break / continue
        let label = if self.at(TokenKind::Ident) {
            Some(self.bump().span)
        } else {
            None
        };
        self.expect(TokenKind::Semicolon, "`;`");
        let span = self.finish(lo);
        if is_break {
            Stmt::Break { span, label }
        } else {
            Stmt::Continue { span, label }
        }
    }

    fn defer_stmt(&mut self) -> Stmt {
        let lo = self.start();
        self.bump(); // defer
        // Beef allows `defer::`/`defer:scope` qualifiers; tolerate them.
        if self.eat(TokenKind::Colon) {
            let _ = self.bump();
        } else if self.at(TokenKind::ColonColon) {
            self.bump();
        }
        let body = Box::new(self.stmt());
        Stmt::Defer {
            span: self.finish(lo),
            body,
        }
    }

    fn local(&mut self, consume_semi: bool) -> Stmt {
        let lo = self.start();
        let is_let = self.at_kw(Keyword::Let);
        self.bump(); // var / let
        // Tuple destructuring: `var (a, b) = …`. Consume the `(names)`
        // pattern; keep the opening-paren span as the binding name.
        let name = if self.at(TokenKind::LParen) {
            let pat = self.cur().span;
            self.bump(); // (
            while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                self.bump();
            }
            self.expect(TokenKind::RParen, "`)` to close destructuring pattern");
            pat
        } else {
            self.expect_ident_span("variable name")
        };
        let init = if self.eat(TokenKind::Assign) {
            Some(self.expr())
        } else {
            None
        };
        if consume_semi {
            self.expect(TokenKind::Semicolon, "`;` after local variable");
        }
        Stmt::Local {
            span: self.finish(lo),
            is_let,
            ty: None,
            name,
            init,
        }
    }

    fn expr_stmt(&mut self) -> Stmt {
        let lo = self.start();
        let expr = self.expr();
        // A trailing expression with no `;` right before `}` is a block's
        // yielded value (`{ …; result }`) — don't require the semicolon.
        if !self.eat(TokenKind::Semicolon) && !self.at(TokenKind::RBrace) {
            self.error("expected `;` after expression statement");
        }
        Stmt::Expr {
            span: self.finish(lo),
            expr,
        }
    }

    // ── types ───────────────────────────────────────────────────────────

    /// Parse a type reference.
    fn ty(&mut self) -> Type {
        let lo = self.start();
        let base = match self.kind() {
            TokenKind::LParen => self.tuple_type(lo),
            TokenKind::Keyword(Keyword::Var) => {
                let s = self.cur().span;
                self.bump();
                Type::Var(s)
            }
            // `comptype(expr)` / `decltype(expr)` — type computed at
            // compile-time from an expression. Modeled as a Var
            // placeholder for now.
            TokenKind::Keyword(Keyword::Comptype)
            | TokenKind::Keyword(Keyword::Decltype)
            | TokenKind::Keyword(Keyword::RetType)
            | TokenKind::Keyword(Keyword::AllocType) => {
                self.bump();
                self.expect(
                    TokenKind::LParen,
                    "`(` after comptype/decltype/rettype/alloctype",
                );
                let _e = self.expr();
                self.expect(
                    TokenKind::RParen,
                    "`)` after comptype/decltype/rettype/alloctype argument",
                );
                Type::Var(self.finish(lo))
            }
            // Function/delegate types: `delegate Ret(params)`,
            // `function Ret(params)`, optionally with attributes between the
            // keyword and the return type (`function [CallingConvention(.Cdecl)]
            // void(StructC)`).
            TokenKind::Keyword(Keyword::Delegate) | TokenKind::Keyword(Keyword::Function) => {
                let is_delegate = self.at_kw(Keyword::Delegate);
                self.bump();
                let _attrs = self.attributes();
                let return_ty = Box::new(self.ty());
                let params = if self.at(TokenKind::LParen) {
                    self.params().into_iter().map(|p| p.ty).collect()
                } else {
                    Vec::new()
                };
                Type::Function {
                    span: self.finish(lo),
                    is_delegate,
                    return_ty,
                    params,
                }
            }
            // Anonymous type in type position: `struct { … }`, `enum { … }`,
            // `enum : int { … }` (used as a field/return/property type). The
            // body is parsed and dropped for now; a placeholder type stands
            // in. (Named `struct Foo { … }` is a nested type decl, handled in
            // `member`.)
            TokenKind::Keyword(Keyword::Struct | Keyword::Class | Keyword::Enum)
                if matches!(self.nth_kind(1), TokenKind::LBrace | TokenKind::Colon) =>
            {
                self.anonymous_type(lo)
            }
            // Bare `.` in type position = inferred type — the `(.)`
            // cast-to-inferred-type, `StructA v = .();` etc.
            TokenKind::Dot if self.nth_kind(1) != TokenKind::Ident => {
                let s = self.cur().span;
                self.bump();
                Type::Var(s)
            }
            TokenKind::Ident => self.path_type(lo),
            _ => {
                self.error("expected a type");
                self.bump();
                Type::Error(self.finish(lo))
            }
        };
        self.type_suffixes(base)
    }

    /// Parse an anonymous type used in type position: `struct { members }`,
    /// `enum { cases }`, `enum : Base { cases }`. The body's members are
    /// parsed (to balance braces) and dropped; a `Var` placeholder stands in
    /// for the anonymous type for now.
    fn anonymous_type(&mut self, lo: u32) -> Type {
        let kind = self.type_kind().unwrap_or(TypeKind::Struct);
        // Optional underlying/base type: `enum : int`.
        if self.eat(TokenKind::Colon) {
            let _ = self.ty();
        }
        if self.eat(TokenKind::LBrace) {
            let _ = self.members(kind);
            self.expect(TokenKind::RBrace, "`}` to close anonymous type");
        }
        Type::Var(self.finish(lo))
    }

    fn path_type(&mut self, lo: u32) -> Type {
        let mut segments = Vec::new();
        loop {
            if !self.at(TokenKind::Ident) {
                self.error("expected identifier in type path");
                break;
            }
            let name = self.bump().span;
            let args = if self.at(TokenKind::Lt) {
                self.type_args()
            } else {
                Vec::new()
            };
            segments.push(TypeSeg { name, args });
            // Descend on `.ident` or `::ident` (`global::System.String`).
            if (self.at(TokenKind::Dot) || self.at(TokenKind::ColonColon))
                && self.nth_kind(1) == TokenKind::Ident
            {
                self.bump();
            } else {
                break;
            }
        }
        if segments.is_empty() {
            return Type::Error(self.finish(lo));
        }
        Type::Path {
            span: self.finish(lo),
            segments,
        }
    }

    /// Parse `<T1, T2, …>` — consumes the `<` and the closing `>`,
    /// splitting a `>>` token when it closes nested generics.
    fn type_args(&mut self) -> Vec<Type> {
        debug_assert!(self.at(TokenKind::Lt));
        self.bump(); // <
        let mut args = Vec::new();
        if !self.at(TokenKind::Gt) && !self.at(TokenKind::Shr) {
            loop {
                let before = self.pos;
                // Const generic argument: `Foo<const N>`, or a bare literal
                // value (`Foo<16>`, `Foo<true>`). A type never starts with a
                // numeric/char/bool literal, so this is unambiguous. Parse the
                // value expression and stand a placeholder type in for it.
                let arg = if self.at_kw(Keyword::Const)
                    || matches!(
                        self.kind(),
                        TokenKind::Int
                            | TokenKind::Float
                            | TokenKind::Char
                            | TokenKind::Keyword(Keyword::True)
                            | TokenKind::Keyword(Keyword::False)
                            | TokenKind::Minus
                    ) {
                    let s = self.cur().span;
                    let _ = self.eat_kw(Keyword::Const);
                    // Parse the const value at a precedence above comparison
                    // (binding power 6) so `>` closes the generic list rather
                    // than being read as greater-than: `<const TSize + 100>`.
                    let _ = self.binary(6);
                    Type::Var(s)
                } else {
                    self.ty()
                };
                args.push(arg);
                if !self.eat(TokenKind::Comma) {
                    break;
                }
                if self.pos == before {
                    break;
                }
            }
        }
        self.close_gt();
        args
    }

    /// Consume a single `>` close, splitting an adjacent `>>` (Shr) into
    /// two halves so `List<List<int>>` closes both generics correctly.
    fn close_gt(&mut self) -> bool {
        if self.at(TokenKind::Gt) {
            self.bump();
            return true;
        }
        if self.at(TokenKind::Shr) {
            let tok = self.toks[self.pos];
            let half = Span::new(tok.span.file, tok.span.lo + 1, tok.span.hi);
            // Replace `>>` with the remaining `>`; we've "consumed" the
            // left half. Record the mutation so `restore` can undo it.
            self.splits.push((self.pos, tok));
            self.toks[self.pos] = Token {
                kind: TokenKind::Gt,
                span: half,
            };
            return true;
        }
        self.error("expected `>` to close generic arguments");
        false
    }

    /// Parse trailing type modifiers: `*`, `?`, `[]`/`[,]`, `[N]`.
    fn type_suffixes(&mut self, mut t: Type) -> Type {
        let lo = t.span().lo;
        loop {
            t = match self.kind() {
                TokenKind::Star => {
                    self.bump();
                    Type::Pointer {
                        span: self.finish(lo),
                        inner: Box::new(t),
                    }
                }
                TokenKind::Question => {
                    self.bump();
                    Type::Nullable {
                        span: self.finish(lo),
                        inner: Box::new(t),
                    }
                }
                TokenKind::LBracket => {
                    self.bump();
                    if self.at(TokenKind::RBracket) {
                        self.bump();
                        Type::Array {
                            span: self.finish(lo),
                            inner: Box::new(t),
                            rank: 1,
                        }
                    } else if self.at(TokenKind::Comma) {
                        let mut rank = 1u32;
                        while self.eat(TokenKind::Comma) {
                            rank += 1;
                        }
                        self.expect(TokenKind::RBracket, "`]` to close array type");
                        Type::Array {
                            span: self.finish(lo),
                            inner: Box::new(t),
                            rank,
                        }
                    } else {
                        let size = Box::new(self.expr());
                        self.expect(TokenKind::RBracket, "`]` to close sized-array");
                        Type::Sized {
                            span: self.finish(lo),
                            inner: Box::new(t),
                            size,
                        }
                    }
                }
                _ => break,
            };
        }
        t
    }

    fn tuple_type(&mut self, lo: u32) -> Type {
        self.bump(); // (
        let mut elems = Vec::new();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            let before = self.pos;
            elems.push(self.ty());
            // Named tuple element: `(K key, V value)` — consume the
            // optional element name.
            if self.at(TokenKind::Ident) {
                self.bump();
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
            if self.pos == before {
                break;
            }
        }
        self.expect(TokenKind::RParen, "`)` to close tuple type");
        Type::Tuple {
            span: self.finish(lo),
            elems,
        }
    }

    // ── declarations (compilation unit / items / members) ──────────────

    fn comp_unit(&mut self) -> CompUnit {
        let lo = self.start();
        let mut items = Vec::new();
        while !self.at(TokenKind::Eof) {
            let before = self.pos;
            items.push(self.item());
            if self.pos == before {
                self.bump(); // safety
            }
        }
        CompUnit {
            span: self.finish(lo),
            items,
        }
    }

    fn item(&mut self) -> Item {
        let lo = self.start();
        let attributes = self.attributes();
        if self.at_kw(Keyword::Using) {
            return self.using_item(lo, attributes);
        }
        if self.at_kw(Keyword::Namespace) {
            return self.namespace_item(lo, attributes);
        }
        let modifiers = self.modifiers();
        if self.at_kw(Keyword::Delegate) {
            return self.delegate_item(lo, attributes, modifiers);
        }
        if self.at_kw(Keyword::TypeAlias) {
            return self.type_alias_item(lo, attributes, modifiers);
        }
        if self.at_type_kind_kw() {
            return Item::Type(self.type_decl(lo, attributes, modifiers));
        }
        // Namespace-level `static { members }` grouping block.
        if self.at(TokenKind::LBrace) {
            let name = self.cur().span;
            self.bump(); // {
            let members = self.members(TypeKind::Class);
            self.expect(TokenKind::RBrace, "`}` to close static block");
            return Item::Type(TypeDecl {
                span: self.finish(lo),
                attributes,
                modifiers,
                kind: TypeKind::Class,
                name,
                generic_params: Vec::new(),
                bases: Vec::new(),
                constraints: Vec::new(),
                members,
            });
        }
        self.error("expected an item declaration (using/namespace/class/struct/interface/enum)");
        // Recovery: skip to next item-ish boundary.
        self.skip_to_item_boundary();
        Item::Error(self.finish(lo))
    }

    fn type_alias_item(
        &mut self,
        lo: u32,
        attributes: Vec<Attribute>,
        modifiers: Vec<(Modifier, Span)>,
    ) -> Item {
        self.bump(); // typealias
        let name = if self.at(TokenKind::Ident) {
            self.bump().span
        } else {
            self.error("expected typealias name");
            self.cur().span
        };
        let generic_params = if self.at(TokenKind::Lt) {
            self.generic_params()
        } else {
            Vec::new()
        };
        self.expect(TokenKind::Assign, "`=` after typealias name");
        let target = self.ty();
        self.expect(TokenKind::Semicolon, "`;` after typealias");
        Item::TypeAlias {
            span: self.finish(lo),
            attributes,
            modifiers,
            name,
            generic_params,
            target,
        }
    }

    fn delegate_item(
        &mut self,
        lo: u32,
        attributes: Vec<Attribute>,
        modifiers: Vec<(Modifier, Span)>,
    ) -> Item {
        self.bump(); // delegate
        let return_ty = self.ty();
        let name = if self.at(TokenKind::Ident) {
            self.bump().span
        } else {
            self.error("expected delegate name");
            self.cur().span
        };
        let generic_params = if self.at(TokenKind::Lt) {
            self.generic_params()
        } else {
            Vec::new()
        };
        let params = if self.at(TokenKind::LParen) {
            self.params()
        } else {
            Vec::new()
        };
        let _ = self.where_clauses();
        self.expect(TokenKind::Semicolon, "`;` after delegate declaration");
        Item::Delegate {
            span: self.finish(lo),
            attributes,
            modifiers,
            return_ty,
            name,
            generic_params,
            params,
        }
    }

    fn using_item(&mut self, lo: u32, attributes: Vec<Attribute>) -> Item {
        self.bump(); // using
        let is_static = self.eat_kw(Keyword::Static);
        let first_lo = self.start();
        let first = self.path_type(first_lo);
        // `using A = B;` — the first parsed name was an alias.
        let (alias, target) = if self.eat(TokenKind::Assign) {
            let alias_span = match &first {
                Type::Path { segments, .. }
                    if segments.len() == 1 && segments[0].args.is_empty() =>
                {
                    Some(segments[0].name)
                }
                _ => {
                    self.error("alias before `=` must be a single identifier");
                    None
                }
            };
            let target_lo = self.start();
            (alias_span, self.path_type(target_lo))
        } else {
            (None, first)
        };
        self.expect(TokenKind::Semicolon, "`;` after `using` directive");
        Item::Using {
            span: self.finish(lo),
            attributes,
            is_static,
            alias,
            target,
        }
    }

    fn namespace_item(&mut self, lo: u32, attributes: Vec<Attribute>) -> Item {
        self.bump(); // namespace
        let path_lo = self.start();
        let path = self.path_type(path_lo);
        let body = if self.eat(TokenKind::Semicolon) {
            None // file-scoped
        } else {
            self.expect(TokenKind::LBrace, "`{` or `;` after namespace path");
            let mut items = Vec::new();
            while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                let before = self.pos;
                items.push(self.item());
                if self.pos == before {
                    self.bump();
                }
            }
            self.expect(TokenKind::RBrace, "`}` to close namespace");
            Some(items)
        };
        Item::Namespace {
            span: self.finish(lo),
            attributes,
            path,
            body,
        }
    }

    fn type_decl(
        &mut self,
        lo: u32,
        attributes: Vec<Attribute>,
        modifiers: Vec<(Modifier, Span)>,
    ) -> TypeDecl {
        let kind = self
            .type_kind()
            .expect("at_type_kind_kw guaranteed a type-kind keyword");
        let name = if self.at(TokenKind::Ident) {
            self.bump().span
        } else {
            self.error("expected type name");
            self.cur().span
        };
        let generic_params = if self.at(TokenKind::Lt) {
            self.generic_params()
        } else {
            Vec::new()
        };
        let bases = if self.eat(TokenKind::Colon) {
            self.base_list()
        } else {
            Vec::new()
        };
        let constraints = self.where_clauses();
        // Bodyless / forward declaration: `struct StructB;`.
        if self.eat(TokenKind::Semicolon) {
            return TypeDecl {
                span: self.finish(lo),
                attributes,
                modifiers,
                kind,
                name,
                generic_params,
                bases,
                constraints,
                members: Vec::new(),
            };
        }
        let members = if self.eat(TokenKind::LBrace) {
            self.members(kind)
        } else {
            self.expect(TokenKind::LBrace, "`{` to open type body");
            Vec::new()
        };
        // Consume `}` (members() stops at `}` but does not consume it).
        self.expect(TokenKind::RBrace, "`}` to close type body");
        TypeDecl {
            span: self.finish(lo),
            attributes,
            modifiers,
            kind,
            name,
            generic_params,
            bases,
            constraints,
            members,
        }
    }

    /// Parse attributes and modifiers in any order / interleaved (Beef
    /// allows `public [Inline] static`, `[Union] public`, etc.).
    fn attrs_and_modifiers(&mut self) -> (Vec<Attribute>, Vec<(Modifier, Span)>) {
        let mut attributes = Vec::new();
        let mut modifiers = Vec::new();
        loop {
            let before = self.pos;
            attributes.extend(self.attributes());
            modifiers.extend(self.modifiers());
            // `using` field qualifier (member forwarding):
            // `using public ClassA mInst;`. Consumed; not modeled yet.
            let _ = self.eat_kw(Keyword::Using);
            if self.pos == before {
                break;
            }
        }
        (attributes, modifiers)
    }

    fn at_type_kind_kw(&self) -> bool {
        matches!(
            self.kind(),
            TokenKind::Keyword(Keyword::Class)
                | TokenKind::Keyword(Keyword::Struct)
                | TokenKind::Keyword(Keyword::Interface)
                | TokenKind::Keyword(Keyword::Enum)
                | TokenKind::Keyword(Keyword::Extension)
        )
    }

    fn type_kind(&mut self) -> Option<TypeKind> {
        let TokenKind::Keyword(k) = self.kind() else {
            return None;
        };
        let kind = match k {
            Keyword::Class => TypeKind::Class,
            Keyword::Struct => TypeKind::Struct,
            Keyword::Interface => TypeKind::Interface,
            Keyword::Enum => TypeKind::Enum,
            Keyword::Extension => TypeKind::Extension,
            _ => return None,
        };
        self.bump();
        Some(kind)
    }

    fn attributes(&mut self) -> Vec<Attribute> {
        let mut attrs = Vec::new();
        while self.at(TokenKind::LBracket) {
            self.bump(); // [
            // Optional target specifier: `[return: X]`, `[field: X]`, etc.
            if self.nth_kind(1) == TokenKind::Colon
                && matches!(self.kind(), TokenKind::Ident | TokenKind::Keyword(_))
            {
                self.bump(); // target
                self.bump(); // :
            }
            loop {
                let lo = self.start();
                let name = self.path_type(lo);
                let args = if self.eat(TokenKind::LParen) {
                    let a = self.arg_list(TokenKind::RParen);
                    self.expect(TokenKind::RParen, "`)` after attribute args");
                    a
                } else {
                    Vec::new()
                };
                attrs.push(Attribute {
                    span: self.finish(lo),
                    name,
                    args,
                });
                if !self.eat(TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::RBracket, "`]` to close attribute");
        }
        attrs
    }

    fn modifiers(&mut self) -> Vec<(Modifier, Span)> {
        let mut mods = Vec::new();
        while let Some(m) = self.peek_modifier() {
            let span = self.cur().span;
            self.bump();
            mods.push((m, span));
        }
        mods
    }

    fn peek_modifier(&self) -> Option<Modifier> {
        let TokenKind::Keyword(k) = self.kind() else {
            return None;
        };
        Some(match k {
            Keyword::Public => Modifier::Public,
            Keyword::Private => Modifier::Private,
            Keyword::Protected => Modifier::Protected,
            Keyword::Internal => Modifier::Internal,
            Keyword::Static => Modifier::Static,
            Keyword::Abstract => Modifier::Abstract,
            Keyword::Sealed => Modifier::Sealed,
            Keyword::Virtual => Modifier::Virtual,
            Keyword::Override => Modifier::Override,
            Keyword::Extern => Modifier::Extern,
            Keyword::ReadOnly => Modifier::ReadOnly,
            Keyword::Const => Modifier::Const,
            Keyword::Mut => Modifier::Mut,
            Keyword::Ref => Modifier::Ref,
            Keyword::New => Modifier::New,
            Keyword::Inline => Modifier::Inline,
            // NB: `mixin` is NOT a modifier — it's a member kind
            // (`mixin Name(…)`), handled in `member`.
            Keyword::Append => Modifier::Append,
            Keyword::Concrete => Modifier::Concrete,
            Keyword::Implicit => Modifier::Implicit,
            Keyword::Explicit => Modifier::Explicit,
            Keyword::Volatile => Modifier::Volatile,
            _ => return None,
        })
    }

    fn generic_params(&mut self) -> Vec<GenericParam> {
        let mut params = Vec::new();
        if !self.eat(TokenKind::Lt) {
            return params;
        }
        loop {
            let lo = self.start();
            // skip any attributes on the type param
            let _ = self.attributes();
            if !self.at(TokenKind::Ident) {
                break;
            }
            let name = self.bump().span;
            params.push(GenericParam {
                span: self.finish(lo),
                name,
            });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.close_gt();
        params
    }

    fn base_list(&mut self) -> Vec<Type> {
        let mut bases = Vec::new();
        loop {
            let before = self.pos;
            // Beef tuple-struct/primary-ctor base clause: `: this(int a)`.
            // Consume it (params become the type's fields) and continue.
            if self.at_kw(Keyword::This) && self.nth_kind(1) == TokenKind::LParen {
                self.bump(); // this
                let _ = self.params();
            } else {
                bases.push(self.ty());
            }
            if !self.eat(TokenKind::Comma) {
                break;
            }
            if self.pos == before {
                break;
            }
        }
        bases
    }

    fn where_clauses(&mut self) -> Vec<WhereClause> {
        let mut clauses = Vec::new();
        while self.at_kw(Keyword::Where) {
            let lo = self.start();
            self.bump(); // where
            // The constrained entity is usually a type-parameter name, but
            // can be a type expression: `where alloctype(T) : delete`.
            // Consume everything up to the `:` as the name span.
            let name = if self.at(TokenKind::Ident) && matches!(self.nth_kind(1), TokenKind::Colon)
            {
                self.bump().span
            } else {
                let name_lo = self.cur().span.lo;
                let mut depth = 0i32;
                loop {
                    if self.at(TokenKind::Eof) || self.at(TokenKind::LBrace) {
                        break;
                    }
                    if depth == 0 && self.at(TokenKind::Colon) {
                        break;
                    }
                    match self.kind() {
                        TokenKind::LParen | TokenKind::LBracket => depth += 1,
                        TokenKind::RParen | TokenKind::RBracket => depth -= 1,
                        _ => {}
                    }
                    self.bump();
                }
                Span::new(self.file, name_lo, self.toks[self.pos - 1].span.hi)
            };
            self.expect(TokenKind::Colon, "`:` after `where T`");
            let mut constraints = Vec::new();
            loop {
                let before = self.pos;
                constraints.push(self.constraint_atom());
                if !self.eat(TokenKind::Comma) {
                    break;
                }
                if self.pos == before {
                    break;
                }
            }
            clauses.push(WhereClause {
                span: self.finish(lo),
                name,
                constraints,
            });
        }
        clauses
    }

    /// One constraint atom: a type, or a keyword like `class`/`struct`/
    /// `new`/`delete` (synthesised as a single-ident Path).
    fn constraint_atom(&mut self) -> Type {
        if let TokenKind::Keyword(k) = self.kind() {
            // `where T : const Type` — const generic constraint takes a
            // type after the keyword.
            if k == Keyword::Const {
                self.bump();
                return self.ty();
            }
            // `where T : operator T <=> T` / `where bool : operator T == T`
            // — operator constraint; consume to the next clause boundary.
            if k == Keyword::Operator {
                let op_lo = self.start();
                self.bump(); // operator
                while !matches!(
                    self.kind(),
                    TokenKind::Comma | TokenKind::LBrace | TokenKind::Semicolon | TokenKind::Eof
                ) && !self.at_kw(Keyword::Where)
                {
                    self.bump();
                }
                return Type::Var(self.finish(op_lo));
            }
            if matches!(
                k,
                Keyword::Class
                    | Keyword::Struct
                    | Keyword::New
                    | Keyword::Delete
                    | Keyword::Var
                    | Keyword::Concrete
                    | Keyword::Interface
                    | Keyword::Enum
            ) {
                let span = self.bump().span;
                let base = Type::Path {
                    span,
                    segments: vec![TypeSeg {
                        name: span,
                        args: Vec::new(),
                    }],
                };
                // `where T : struct*` etc. — allow trailing type suffixes.
                return self.type_suffixes(base);
            }
        }
        self.ty()
    }

    // ── members ─────────────────────────────────────────────────────────

    fn members(&mut self, kind: TypeKind) -> Vec<Member> {
        let mut out = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            // Stray `;` between members — e.g. a trailing semicolon after a
            // nested type body (`enum { … };`). Skip as an empty member.
            if self.eat(TokenKind::Semicolon) {
                continue;
            }
            let before = self.pos;
            out.push(self.member(kind));
            if self.pos == before {
                self.bump();
            }
        }
        out
    }

    /// Read-only lookahead: does the upcoming sequence look like the
    /// `IFace<Args>.Member` head of an explicit interface implementation —
    /// an identifier, an optional balanced `<…>`, then `.Ident`? The
    /// `.this` explicit-indexer form (ending in a keyword) is excluded; it's
    /// handled by the `.Ident`/`.this` walk in `member`.
    fn looks_like_explicit_iface(&self) -> bool {
        let mut i = self.pos;
        if self.toks.get(i).map(|t| t.kind) != Some(TokenKind::Ident) {
            return false;
        }
        i += 1;
        if self.toks.get(i).map(|t| t.kind) == Some(TokenKind::Lt) {
            let mut depth: i32 = 0;
            loop {
                match self.toks.get(i).map(|t| t.kind) {
                    Some(TokenKind::Lt) => depth += 1,
                    Some(TokenKind::Shl) => depth += 2,
                    Some(TokenKind::Gt) => depth -= 1,
                    Some(TokenKind::Shr) => depth -= 2,
                    Some(TokenKind::Eof) | None => return false,
                    _ => {}
                }
                i += 1;
                if depth <= 0 {
                    break;
                }
            }
        }
        self.toks.get(i).map(|t| t.kind) == Some(TokenKind::Dot)
            && self.toks.get(i + 1).map(|t| t.kind) == Some(TokenKind::Ident)
    }

    fn member(&mut self, kind: TypeKind) -> Member {
        let lo = self.start();
        let (attributes, modifiers) = self.attrs_and_modifiers();

        // Enum case: `case Name [(payload)] [= value];`
        if kind == TypeKind::Enum && self.at_kw(Keyword::Case) {
            return self.enum_case(lo, attributes);
        }

        // C-style enum value: `Name [= value],` / `Name [= value];`
        // (no `case` keyword — common in real-world Beef enums).
        if kind == TypeKind::Enum && self.at(TokenKind::Ident) {
            let next = self.nth_kind(1);
            if matches!(
                next,
                TokenKind::Comma | TokenKind::Semicolon | TokenKind::Assign | TokenKind::RBrace
            ) {
                let name = self.bump().span;
                let value = if self.eat(TokenKind::Assign) {
                    Some(self.expr())
                } else {
                    None
                };
                let _ = self.eat(TokenKind::Comma) || self.eat(TokenKind::Semicolon);
                return Member::EnumCase {
                    span: self.finish(lo),
                    attributes,
                    name,
                    payload: Vec::new(),
                    value,
                };
            }
        }

        // `typealias Name [<G…>] = Type;`
        if self.at_kw(Keyword::TypeAlias) {
            return self.type_alias_member(lo, attributes, modifiers);
        }

        // Nested *named* type. An anonymous `struct {…}` / `enum {…}` /
        // `enum : int {…}` (no name after the keyword) is instead a member
        // whose *type* is anonymous — it falls through to `self.ty()` below.
        if self.at_type_kind_kw() && self.nth_kind(1) == TokenKind::Ident {
            return Member::Nested(self.type_decl(lo, attributes, modifiers));
        }

        // `static { members }` — a static member-grouping block (Beef
        // groups statics this way). Parse its contents as MEMBERS and
        // model as a nested anonymous type.
        if self.at(TokenKind::LBrace) {
            let name = self.cur().span;
            self.bump(); // {
            let members = self.members(kind);
            self.expect(TokenKind::RBrace, "`}` to close static block");
            return Member::Nested(TypeDecl {
                span: self.finish(lo),
                attributes,
                modifiers,
                kind,
                name,
                generic_params: Vec::new(),
                bases: Vec::new(),
                constraints: Vec::new(),
                members,
            });
        }

        // Mixin member: `[mods] mixin Name(params) body` (no return type).
        if self.at_kw(Keyword::Mixin) {
            self.bump(); // mixin
            let name = if self.at(TokenKind::Ident) {
                self.bump().span
            } else {
                self.error("expected mixin name");
                self.cur().span
            };
            let generic_params = if self.at(TokenKind::Lt) {
                self.generic_params()
            } else {
                Vec::new()
            };
            let params = if self.at(TokenKind::LParen) {
                self.params()
            } else {
                Vec::new()
            };
            let _ = self.where_clauses();
            let body = self.method_body();
            return Member::Method {
                span: self.finish(lo),
                attributes,
                modifiers,
                return_ty: Type::Error(name),
                name,
                generic_params,
                params,
                constraints: Vec::new(),
                body,
                explicit_iface: None,
            };
        }

        // Constructor: `this(...)`, generic `this<T>(...)`, or paren-less
        // `this { … }`.
        if self.at_kw(Keyword::This)
            && matches!(
                self.nth_kind(1),
                TokenKind::LParen | TokenKind::LBrace | TokenKind::Lt
            )
        {
            self.bump(); // this
            // Optional generic parameters: `this<T3>(…)`.
            if self.at(TokenKind::Lt) {
                let _ = self.generic_params();
            }
            let params = if self.at(TokenKind::LParen) {
                self.params()
            } else {
                Vec::new()
            };
            // Optional constructor chain: `: this(args)` / `: base(args)` /
            // `: this[Friend](args)` etc. We just consume an expression.
            // Suppress trailing-`{` initializer parsing so the chain doesn't
            // swallow the constructor body `{ … }` (it's still allowed inside
            // the chain's argument parens).
            if self.eat(TokenKind::Colon) {
                let prev = self.suppress_init;
                self.suppress_init = true;
                let _ = self.expr();
                self.suppress_init = prev;
            }
            // Optional `where` constraints on a generic constructor.
            let _ = self.where_clauses();
            let body = self.method_body();
            return Member::Constructor {
                span: self.finish(lo),
                attributes,
                modifiers,
                params,
                body,
            };
        }

        // Destructor: `~this()`
        if self.at(TokenKind::Tilde) && self.nth_kind(1) == TokenKind::Keyword(Keyword::This) {
            self.bump(); // ~
            self.bump(); // this
            self.expect(TokenKind::LParen, "`(` after `~this`");
            self.expect(TokenKind::RParen, "`)` after `~this(`");
            let body = self.method_body();
            return Member::Destructor {
                span: self.finish(lo),
                attributes,
                modifiers,
                body,
            };
        }

        // Conversion-shaped operator: `operator T(...)` (no explicit return
        // type — the operator IS the target type or symbol).
        if self.at_kw(Keyword::Operator) {
            let op_lo = self.start();
            self.bump(); // operator
            while !self.at(TokenKind::LParen) && !self.at(TokenKind::Eof) {
                self.bump();
            }
            let name = self.finish(op_lo);
            let params = self.params();
            let _ = self.where_clauses();
            let body = self.method_body();
            return Member::Method {
                span: self.finish(lo),
                attributes,
                modifiers,
                return_ty: Type::Error(name),
                name,
                generic_params: Vec::new(),
                params,
                constraints: Vec::new(),
                body,
                explicit_iface: None,
            };
        }

        // Otherwise: `Type Name …` — a field / method / property /
        // operator. After the return type we may instead see `operator`
        // (operator method) — the "name" then spans the operator symbol(s).
        let ty = self.ty();

        // Anonymous field: `public struct { … };` — the (anonymous) type IS
        // the member, with no name. Emit a name-less field (empty-span name).
        if self.at(TokenKind::Semicolon) {
            let name = Span::new(self.file, lo, lo);
            self.bump(); // ;
            return Member::Field {
                span: self.finish(lo),
                attributes,
                modifiers,
                ty,
                name,
                init: None,
            };
        }

        // Indexer: `Type this[params] { accessors }` or `=> expr;`.
        if self.at_kw(Keyword::This) && self.nth_kind(1) == TokenKind::LBracket {
            let name = self.bump().span;
            let _params = self.bracketed_params();
            let accessors = if self.eat(TokenKind::LBrace) {
                let mut accs = Vec::new();
                while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                    let before = self.pos;
                    accs.push(self.accessor());
                    if self.pos == before {
                        self.bump();
                    }
                }
                self.expect(TokenKind::RBrace, "`}` to close indexer body");
                accs
            } else if self.eat(TokenKind::FatArrow) {
                let acc_span = self.finish(lo);
                let e = self.expr();
                self.expect(TokenKind::Semicolon, "`;` after expression-bodied indexer");
                vec![Accessor {
                    span: acc_span,
                    attributes: Vec::new(),
                    modifiers: Vec::new(),
                    kind: AccessorKind::Get,
                    body: MethodBody::Expr(e),
                }]
            } else {
                self.expect(
                    TokenKind::Semicolon,
                    "`{ accessors }` or `=> expr;` for indexer",
                );
                Vec::new()
            };
            return Member::Property {
                span: self.finish(lo),
                attributes,
                modifiers,
                ty,
                name,
                accessors,
                explicit_iface: None,
            };
        }
        // The qualifying interface of an explicit interface implementation,
        // if any (`Ret IFace<Args>.Member …`). Captured so the member name
        // doesn't false-collide with a same-named regular member and so sema
        // can resolve the impl.
        let mut explicit_iface: Option<Type> = None;

        let name = if self.at_kw(Keyword::Operator) {
            let op_lo = self.start();
            self.bump(); // operator
            // Consume the operator name up to `(` — symbols, or a type /
            // keyword for conversion ops (`operator implicit`, `operator T`).
            while !self.at(TokenKind::LParen) && !self.at(TokenKind::Eof) {
                self.bump();
            }
            self.finish(op_lo)
        } else if self.looks_like_explicit_iface() {
            // Parse the whole `IFace<Args>.Member` as a type path, then peel
            // the final segment off as the member name. (The `.this`
            // explicit-indexer form ends in a keyword, so it isn't matched
            // here and falls to the `.Ident`/`.this` walk below.)
            let path = self.ty();
            match split_qualified_name(path) {
                Some((iface, member)) => {
                    explicit_iface = Some(iface);
                    member
                }
                None => {
                    self.error("malformed explicit interface member");
                    self.skip_to_member_boundary();
                    return Member::Error(self.finish(lo));
                }
            }
        } else if self.at(TokenKind::Ident) {
            self.bump().span
        } else {
            self.error("expected member name");
            self.skip_to_member_boundary();
            return Member::Error(self.finish(lo));
        };

        // Optional generic params on a method.
        let mut generic_params = if self.at(TokenKind::Lt) {
            self.generic_params()
        } else {
            Vec::new()
        };

        // Fallback explicit-interface walk for forms not captured as a type
        // path above — chiefly the `.this` explicit indexer
        // (`Ret IFoo.this[…]`). The final segment becomes the member name.
        let mut name = name;
        while self.at(TokenKind::Dot)
            && matches!(
                self.nth_kind(1),
                TokenKind::Ident | TokenKind::Keyword(Keyword::This)
            )
        {
            self.bump(); // .
            name = self.bump().span; // Ident or `this` (explicit-iface indexer)
            generic_params = if self.at(TokenKind::Lt) {
                self.generic_params()
            } else {
                Vec::new()
            };
        }

        // Explicit-interface indexer: `Ret IFoo.this[params] { … }`.
        if self.at(TokenKind::LBracket) {
            let _params = self.bracketed_params();
            let accessors = if self.eat(TokenKind::LBrace) {
                let mut accs = Vec::new();
                while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                    let before = self.pos;
                    accs.push(self.accessor());
                    if self.pos == before {
                        self.bump();
                    }
                }
                self.expect(TokenKind::RBrace, "`}` to close indexer body");
                accs
            } else if self.eat(TokenKind::FatArrow) {
                let acc_span = self.finish(lo);
                let e = self.expr();
                self.expect(TokenKind::Semicolon, "`;` after expression-bodied indexer");
                vec![Accessor {
                    span: acc_span,
                    attributes: Vec::new(),
                    modifiers: Vec::new(),
                    kind: AccessorKind::Get,
                    body: MethodBody::Expr(e),
                }]
            } else {
                self.expect(TokenKind::Semicolon, "`{ … }` or `=> expr;` for indexer");
                Vec::new()
            };
            return Member::Property {
                span: self.finish(lo),
                attributes,
                modifiers,
                ty,
                name,
                accessors,
                explicit_iface: None,
            };
        }

        match self.kind() {
            TokenKind::LParen => {
                let params = self.params();
                let constraints = self.where_clauses();
                let body = self.method_body();
                Member::Method {
                    span: self.finish(lo),
                    attributes,
                    modifiers,
                    return_ty: ty,
                    name,
                    generic_params,
                    params,
                    constraints,
                    body,
                    explicit_iface,
                }
            }
            TokenKind::LBrace => {
                self.bump(); // {
                let mut accessors = Vec::new();
                while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                    let before = self.pos;
                    accessors.push(self.accessor());
                    if self.pos == before {
                        self.bump();
                    }
                }
                self.expect(TokenKind::RBrace, "`}` to close property body");
                Member::Property {
                    span: self.finish(lo),
                    attributes,
                    modifiers,
                    ty,
                    name,
                    accessors,
                    explicit_iface,
                }
            }
            // Expression-bodied property: `Type Name => expr;`
            TokenKind::FatArrow => {
                self.bump();
                let e = self.expr();
                let accessor_span = self.finish(lo);
                self.expect(TokenKind::Semicolon, "`;` after expression-bodied property");
                Member::Property {
                    span: self.finish(lo),
                    attributes,
                    modifiers,
                    ty,
                    name,
                    accessors: vec![Accessor {
                        span: accessor_span,
                        attributes: Vec::new(),
                        modifiers: Vec::new(),
                        kind: AccessorKind::Get,
                        body: MethodBody::Expr(e),
                    }],
                    explicit_iface,
                }
            }
            _ => {
                let init = if self.eat(TokenKind::Assign) {
                    Some(self.expr())
                } else {
                    None
                };
                // Beef field destructor: `= new T() ~ delete _;` — `~`
                // introduces a destructor that runs when the field is
                // reclaimed. It may be a block (`~ { … }`), carry a guard
                // (`~ if (onHeap) delete _;`), or be a bare expression. We
                // consume it for coverage, leaving the field's `;` in place.
                if self.eat(TokenKind::Tilde) {
                    if self.at(TokenKind::LBrace) {
                        self.skip_balanced(TokenKind::LBrace, TokenKind::RBrace);
                    } else {
                        if self.eat_kw(Keyword::If) {
                            self.expect(TokenKind::LParen, "`(` after `if`");
                            let _ = self.expr();
                            self.expect(TokenKind::RParen, "`)` after if-condition");
                        }
                        let _ = self.expr();
                    }
                }
                // Multiple declarators: `int a, b, c;` — keep the first,
                // consume the rest (`, name [= init]`).
                while self.eat(TokenKind::Comma) {
                    if !self.at(TokenKind::Ident) {
                        break;
                    }
                    self.bump(); // name
                    if self.eat(TokenKind::Assign) {
                        let _ = self.expr();
                    }
                }
                self.expect(TokenKind::Semicolon, "`;` after field");
                Member::Field {
                    span: self.finish(lo),
                    attributes,
                    modifiers,
                    ty,
                    name,
                    init,
                }
            }
        }
    }

    fn type_alias_member(
        &mut self,
        lo: u32,
        attributes: Vec<Attribute>,
        modifiers: Vec<(Modifier, Span)>,
    ) -> Member {
        self.bump(); // typealias
        let name = if self.at(TokenKind::Ident) {
            self.bump().span
        } else {
            self.error("expected typealias name");
            self.cur().span
        };
        let generic_params = if self.at(TokenKind::Lt) {
            self.generic_params()
        } else {
            Vec::new()
        };
        self.expect(TokenKind::Assign, "`=` after typealias name");
        let target = self.ty();
        self.expect(TokenKind::Semicolon, "`;` after typealias");
        Member::TypeAlias {
            span: self.finish(lo),
            attributes,
            modifiers,
            name,
            generic_params,
            target,
        }
    }

    fn enum_case(&mut self, lo: u32, attributes: Vec<Attribute>) -> Member {
        self.bump(); // case
        let name = if self.at(TokenKind::Ident) {
            self.bump().span
        } else {
            self.error("expected case name");
            self.cur().span
        };
        let payload = if self.at(TokenKind::LParen) {
            self.params()
        } else {
            Vec::new()
        };
        let value = if self.eat(TokenKind::Assign) {
            Some(self.expr())
        } else {
            None
        };
        // Beef enum cases are terminated by `,`, `;`, or just the next `case`.
        let _ = self.eat(TokenKind::Comma) || self.eat(TokenKind::Semicolon);
        Member::EnumCase {
            span: self.finish(lo),
            attributes,
            name,
            payload,
            value,
        }
    }

    fn method_body(&mut self) -> MethodBody {
        // Trailing method modifiers appear between the signature and the
        // body: `bool MoveNext() mut { … }`, `... mut;`.
        while matches!(
            self.kind(),
            TokenKind::Keyword(Keyword::Mut) | TokenKind::Keyword(Keyword::ReadOnly)
        ) {
            self.bump();
        }
        if self.at(TokenKind::LBrace) {
            return MethodBody::Block(self.block());
        }
        if self.eat(TokenKind::FatArrow) {
            let e = self.expr();
            self.expect(TokenKind::Semicolon, "`;` after expression-bodied member");
            return MethodBody::Expr(e);
        }
        self.expect(
            TokenKind::Semicolon,
            "`;` or `{ … }` or `=> expr;` for method body",
        );
        MethodBody::None
    }

    fn bracketed_params(&mut self) -> Vec<Param> {
        self.expect(TokenKind::LBracket, "`[`");
        let mut params = Vec::new();
        while !self.at(TokenKind::RBracket) && !self.at(TokenKind::Eof) {
            let before = self.pos;
            params.push(self.param());
            if !self.eat(TokenKind::Comma) {
                break;
            }
            if self.pos == before {
                break;
            }
        }
        self.expect(TokenKind::RBracket, "`]`");
        params
    }

    fn params(&mut self) -> Vec<Param> {
        self.expect(TokenKind::LParen, "`(`");
        let mut params = Vec::new();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            let before = self.pos;
            params.push(self.param());
            if !self.eat(TokenKind::Comma) {
                break;
            }
            if self.pos == before {
                break;
            }
        }
        self.expect(TokenKind::RParen, "`)`");
        params
    }

    fn param(&mut self) -> Param {
        let lo = self.start();
        // C-style varargs marker `...` in a parameter list.
        if self.at(TokenKind::DotDotDot) {
            let s = self.bump().span;
            return Param {
                span: s,
                attributes: Vec::new(),
                modifier: None,
                ty: Type::Var(s),
                name: None,
                default: None,
            };
        }
        let attributes = self.attributes();
        // A param can carry several modifiers, e.g. `this ref StructA`.
        // We keep the first for the AST and consume the rest.
        let modifier = self.peek_param_modifier().map(|m| {
            let span = self.cur().span;
            self.bump();
            (m, span)
        });
        while self.peek_param_modifier().is_some()
            || matches!(
                self.kind(),
                TokenKind::Keyword(Keyword::ReadOnly) | TokenKind::Keyword(Keyword::Const)
            )
        {
            self.bump();
        }
        let ty = self.ty();
        // The name may be `this` (the explicit instance parameter of a
        // function/delegate type or extension method: `StructB this`).
        let name = if self.at(TokenKind::Ident) || self.at_kw(Keyword::This) {
            Some(self.bump().span)
        } else {
            None
        };
        let default = if self.eat(TokenKind::Assign) {
            Some(self.expr())
        } else {
            None
        };
        Param {
            span: self.finish(lo),
            attributes,
            modifier,
            ty,
            name,
            default,
        }
    }

    fn peek_param_modifier(&self) -> Option<ParamModifier> {
        let TokenKind::Keyword(k) = self.kind() else {
            return None;
        };
        Some(match k {
            Keyword::Ref => ParamModifier::Ref,
            Keyword::Out => ParamModifier::Out,
            Keyword::Mut => ParamModifier::Mut,
            Keyword::Params => ParamModifier::Params,
            Keyword::In => ParamModifier::In,
            Keyword::This => ParamModifier::This,
            _ => return None,
        })
    }

    fn accessor(&mut self) -> Accessor {
        let lo = self.start();
        let attributes = self.attributes();
        let mut modifiers = self.modifiers();
        let kind = if self.eat_ident_text("get") {
            AccessorKind::Get
        } else if self.eat_ident_text("set") {
            AccessorKind::Set
        } else {
            self.error("expected `get` or `set`");
            self.bump();
            return Accessor {
                span: self.finish(lo),
                attributes,
                modifiers,
                kind: AccessorKind::Get,
                body: MethodBody::None,
            };
        };
        // Trailing modifiers after `get`/`set`, e.g. `get mut { … }`.
        modifiers.extend(self.modifiers());
        let body = self.method_body();
        Accessor {
            span: self.finish(lo),
            attributes,
            modifiers,
            kind,
            body,
        }
    }

    // ── recovery ────────────────────────────────────────────────────────

    /// Skip to the next plausible item boundary on a top-level error: a
    /// `;` (consume it) or `}` (leave it) at depth 0.
    fn skip_to_item_boundary(&mut self) {
        let mut depth = 0i32;
        while !self.at(TokenKind::Eof) {
            match self.kind() {
                TokenKind::LBrace => {
                    depth += 1;
                    self.bump();
                }
                TokenKind::RBrace => {
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                    self.bump();
                }
                TokenKind::Semicolon if depth == 0 => {
                    self.bump();
                    return;
                }
                _ => {
                    self.bump();
                }
            }
        }
    }

    /// Skip to the next plausible member boundary inside a type body:
    /// `;` (consume) or `}` (leave) at depth 0.
    fn skip_to_member_boundary(&mut self) {
        self.skip_to_item_boundary();
    }
}

/// Parse a single type reference from `src`.
pub fn parse_type(src: &str, file: FileId) -> (Type, Vec<Diagnostic>) {
    let mut p = Parser::new(src, file);
    let t = p.ty();
    if !p.at(TokenKind::Eof) {
        p.error("trailing tokens after type");
    }
    (t, p.diagnostics)
}

/// Parse a whole .bf compilation unit from `src` (the parser phase
/// report behind `newbf-driver dump-ast`).
/// Split a parsed `IFace<Args>.Member` type path into the qualifying
/// interface type and the final member-name span. Returns `None` for a
/// single-segment path (shouldn't occur given the explicit-iface lookahead).
/// The qualifier keeps the full path span — it over-covers the member name
/// slightly, which is fine for diagnostics at this phase.
fn split_qualified_name(path: Type) -> Option<(Type, Span)> {
    if let Type::Path { span, mut segments } = path {
        if segments.len() < 2 {
            return None;
        }
        let last = segments.pop().unwrap();
        Some((Type::Path { span, segments }, last.name))
    } else {
        None
    }
}

pub fn parse_file(src: &str, file: FileId) -> (CompUnit, Vec<Diagnostic>) {
    let mut p = Parser::new(src, file);
    let unit = p.comp_unit();
    (unit, p.diagnostics)
}
