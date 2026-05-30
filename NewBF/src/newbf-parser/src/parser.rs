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
        let toks: Vec<Token> = lex(src, file)
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
        while let Some(op) = self.peek_binop() {
            let bp = op.precedence();
            if bp < min_bp {
                break;
            }
            self.bump();
            // `is`/`as`/`case` take a type or pattern on the right; we
            // parse it as a unary expression (a type/pattern stand-in).
            let rhs = if matches!(op, BinOp::Is | BinOp::As | BinOp::Case) {
                self.unary()
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
            // optional `:qualifier` (allocator/scope qualifier)
            let qualifier = if self.eat(TokenKind::Colon) {
                let q = self.cur().span;
                // qualifier is an identifier or a keyword like `null`
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
                TokenKind::Dot => {
                    self.bump();
                    // Beef friend-access list `.[Friend]Name` — consume
                    // and discard the bracketed attribute set for now.
                    if self.eat(TokenKind::LBracket) {
                        while !self.at(TokenKind::RBracket) && !self.at(TokenKind::Eof) {
                            self.bump();
                        }
                        self.eat(TokenKind::RBracket);
                    }
                    let name = self.expect_ident_span("member name");
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
                TokenKind::LParen => {
                    self.bump();
                    let args = self.arg_list(TokenKind::RParen);
                    self.expect(TokenKind::RParen, "`)` to close call");
                    e = Expr::Call {
                        span: self.finish(lo),
                        callee: Box::new(e),
                        args,
                    };
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
                _ => break,
            }
        }
        e
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

    fn arg_list(&mut self, close: TokenKind) -> Vec<Expr> {
        let mut args = Vec::new();
        while !self.at(close) && !self.at(TokenKind::Eof) {
            let before = self.pos;
            args.push(self.expr());
            if !self.eat(TokenKind::Comma) {
                break;
            }
            if self.pos == before {
                break; // safety: guarantee progress
            }
        }
        args
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
                Expr::Ident(span)
            }
            TokenKind::Keyword(k) => self.primary_keyword(k, span),
            TokenKind::LParen => self.paren_or_cast(),
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
            if Self::can_start_unary(self.kind()) {
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
        let inner = self.expr();
        self.expect(TokenKind::RParen, "`)` to close parenthesized expression");
        Expr::Paren {
            span: self.finish(lo),
            inner: Box::new(inner),
        }
    }

    /// Token kinds that can start a unary expression — used to commit to
    /// a C-style cast.
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
                | TokenKind::Minus
                | TokenKind::Plus
                | TokenKind::Bang
                | TokenKind::Tilde
                | TokenKind::PlusPlus
                | TokenKind::MinusMinus
                | TokenKind::Star
                | TokenKind::Amp
                | TokenKind::Dot
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
            // Builtin "function-like" keywords are treated as primaries so
            // `sizeof(T)` / `typeof(T)` / `default(T)` parse as calls.
            // `var`/`let` here covers pattern bindings inside `case`
            // patterns (e.g. `case .Ok(var val):`).
            Keyword::SizeOf
            | Keyword::AlignOf
            | Keyword::StrideOf
            | Keyword::TypeOf
            | Keyword::NameOf
            | Keyword::Comptype
            | Keyword::Default
            | Keyword::Var
            | Keyword::Let => {
                self.bump();
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
            TokenKind::Star => BinOp::Mul,
            TokenKind::Slash => BinOp::Div,
            TokenKind::Percent => BinOp::Mod,
            TokenKind::Plus => BinOp::Add,
            TokenKind::Minus => BinOp::Sub,
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
            TokenKind::EqEq => BinOp::Eq,
            TokenKind::NotEq => BinOp::Ne,
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
            TokenKind::PlusEq => AssignOp::Add,
            TokenKind::MinusEq => AssignOp::Sub,
            TokenKind::StarEq => AssignOp::Mul,
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
            _ => {
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
            TokenKind::Ident | TokenKind::LParen | TokenKind::Keyword(Keyword::Var)
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
            TokenKind::Ident | TokenKind::LParen | TokenKind::Keyword(Keyword::Var)
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
        if consume_semi {
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
                Some(self.expr())
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

    fn if_stmt(&mut self) -> Stmt {
        let lo = self.start();
        self.bump(); // if
        self.expect(TokenKind::LParen, "`(` after `if`");
        let cond = self.expr();
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
        let cond = self.expr();
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
            Some(self.expr())
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
        let name = self.expect_ident_span("variable name");
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
        self.expect(TokenKind::Semicolon, "`;` after expression statement");
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
            TokenKind::Keyword(Keyword::Comptype) | TokenKind::Keyword(Keyword::Decltype) => {
                self.bump();
                self.expect(TokenKind::LParen, "`(` after comptype/decltype");
                let _e = self.expr();
                self.expect(TokenKind::RParen, "`)` after comptype/decltype argument");
                Type::Var(self.finish(lo))
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
            // Only descend on `.ident` (avoid swallowing trailing dots).
            if self.at(TokenKind::Dot) && self.nth_kind(1) == TokenKind::Ident {
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
                args.push(self.ty());
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
            Keyword::Mixin => Modifier::Mixin,
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
            bases.push(self.ty());
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
            let name = if self.at(TokenKind::Ident) {
                self.bump().span
            } else {
                self.error("expected type parameter name after `where`");
                self.cur().span
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
            if matches!(
                k,
                Keyword::Class | Keyword::Struct | Keyword::New | Keyword::Delete | Keyword::Var
            ) {
                let span = self.bump().span;
                return Type::Path {
                    span,
                    segments: vec![TypeSeg {
                        name: span,
                        args: Vec::new(),
                    }],
                };
            }
        }
        self.ty()
    }

    // ── members ─────────────────────────────────────────────────────────

    fn members(&mut self, kind: TypeKind) -> Vec<Member> {
        let mut out = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let before = self.pos;
            out.push(self.member(kind));
            if self.pos == before {
                self.bump();
            }
        }
        out
    }

    fn member(&mut self, kind: TypeKind) -> Member {
        let lo = self.start();
        let attributes = self.attributes();
        let modifiers = self.modifiers();

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

        // Nested type
        if self.at_type_kind_kw() {
            return Member::Nested(self.type_decl(lo, attributes, modifiers));
        }

        // Constructor: `this(...)`
        if self.at_kw(Keyword::This) && self.nth_kind(1) == TokenKind::LParen {
            self.bump(); // this
            let params = self.params();
            // Optional constructor chain: `: this(args)` / `: base(args)` /
            // `: this[Friend](args)` etc. We just consume an expression.
            if self.eat(TokenKind::Colon) {
                let _ = self.expr();
            }
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
            };
        }

        // Otherwise: `Type Name …` — a field / method / property /
        // operator. After the return type we may instead see `operator`
        // (operator method) — the "name" then spans the operator symbol(s).
        let ty = self.ty();

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
            };
        }
        let name = if self.at_kw(Keyword::Operator) {
            let op_lo = self.start();
            self.bump(); // operator
            while !self.at(TokenKind::LParen)
                && !self.at(TokenKind::Eof)
                && !matches!(self.kind(), TokenKind::Ident | TokenKind::Keyword(_))
            {
                self.bump();
            }
            self.finish(op_lo)
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

        // Explicit interface implementation: `IFoo<T>.Bar(…)`. Walk
        // qualifying `.Ident` segments; the *last* one is the member
        // name (the qualifier is dropped — interface-impl info is
        // recovered later in sema).
        let mut name = name;
        while self.at(TokenKind::Dot) && matches!(self.nth_kind(1), TokenKind::Ident) {
            self.bump(); // .
            name = self.bump().span;
            generic_params = if self.at(TokenKind::Lt) {
                self.generic_params()
            } else {
                Vec::new()
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
                }
            }
            _ => {
                let init = if self.eat(TokenKind::Assign) {
                    Some(self.expr())
                } else {
                    None
                };
                // Beef field destructor: `= new T() ~ delete _;` — `~`
                // introduces a destructor expression that runs when the
                // field is reclaimed. We consume it for coverage.
                if self.eat(TokenKind::Tilde) {
                    let _ = self.expr();
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
        let attributes = self.attributes();
        let modifier = self.peek_param_modifier().map(|m| {
            let span = self.cur().span;
            self.bump();
            (m, span)
        });
        let ty = self.ty();
        let name = if self.at(TokenKind::Ident) {
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
pub fn parse_file(src: &str, file: FileId) -> (CompUnit, Vec<Diagnostic>) {
    let mut p = Parser::new(src, file);
    let unit = p.comp_unit();
    (unit, p.diagnostics)
}
