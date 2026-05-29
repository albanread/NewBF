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

struct Parser {
    toks: Vec<Token>,
    file: FileId,
    pos: usize,
    diagnostics: Vec<Diagnostic>,
}

impl Parser {
    fn new(src: &str, file: FileId) -> Self {
        let toks: Vec<Token> = lex(src, file)
            .into_iter()
            .filter(|t| !t.kind.is_trivia())
            .collect();
        Self {
            toks,
            file,
            pos: 0,
            diagnostics: Vec::new(),
        }
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
            // `is`/`as` take a type on the right; we parse it as a unary
            // expression (a type stand-in) until the type grammar lands.
            let rhs = if matches!(op, BinOp::Is | BinOp::As) {
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
                TokenKind::MinusMinus => {
                    self.bump();
                    e = Expr::PostDec {
                        span: self.finish(lo),
                        operand: Box::new(e),
                    };
                }
                _ => break,
            }
        }
        e
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
            TokenKind::LParen => {
                let lo = span.lo;
                self.bump();
                let inner = self.expr();
                self.expect(TokenKind::RParen, "`)` to close parenthesized expression");
                Expr::Paren {
                    span: self.finish(lo),
                    inner: Box::new(inner),
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
            Keyword::SizeOf
            | Keyword::AlignOf
            | Keyword::StrideOf
            | Keyword::TypeOf
            | Keyword::NameOf
            | Keyword::Comptype
            | Keyword::Default => {
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
            _ => self.expr_stmt(),
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

    /// A for-init: a `var`/`let` local without trailing `;`, or an expr.
    fn for_init(&mut self) -> Stmt {
        if self.at_kw(Keyword::Var) || self.at_kw(Keyword::Let) {
            self.local(false)
        } else {
            let lo = self.start();
            let e = self.expr();
            Stmt::Expr {
                span: self.finish(lo),
                expr: e,
            }
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
}
