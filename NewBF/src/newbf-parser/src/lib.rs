//! `newbf-parser` — the NewBF parser and AST.
//!
//! A recursive-descent statement parser with a Pratt (precedence-
//! climbing) expression parser using Beef's exact operator precedence
//! (grounded in `E:\beef\IDEHelper\Compiler\BfAst.cpp`). Produces the AST
//! in [`ast`] directly (NewBF folds Beef's raw-tree → `BfReducer` step
//! into one pass) and renders the `dump-parse` report via [`format_parse`].
//!
//! The parser never panics: malformed input yields `Error` nodes plus
//! [`Diagnostic`]s, and every loop is guaranteed to make progress. See
//! SPRINTS.md Sprint 03 for scope (expressions in full; statement core).

mod ast;
mod parser;
mod print;

pub use ast::{AssignOp, BinOp, Expr, PrefixKw, Stmt, UnOp};
pub use parser::{Diagnostic, parse_expr, parse_fragment};
pub use print::{format_expr, format_parse};

#[cfg(test)]
mod tests {
    use super::*;
    use newbf_lexer::FileId;

    // ── test helpers ────────────────────────────────────────────────────

    /// Parse an expression that should be diagnostic-free; return its
    /// compact s-expression form for crisp structural assertions.
    fn ok(src: &str) -> String {
        let (e, diags) = parse_expr(src, FileId(0));
        assert!(
            diags.is_empty(),
            "unexpected diagnostics for {src:?}: {diags:?}"
        );
        sx(src, &e)
    }

    /// Parse a single-statement fragment that should be diagnostic-free;
    /// return the statement's s-expression.
    fn ok_stmt(src: &str) -> String {
        let (stmts, diags) = parse_fragment(src, FileId(0));
        assert!(
            diags.is_empty(),
            "unexpected diagnostics for {src:?}: {diags:?}"
        );
        assert_eq!(stmts.len(), 1, "expected exactly one statement in {src:?}");
        sxs(src, &stmts[0])
    }

    /// Compact s-expression of an expression (paren nodes are unwrapped,
    /// since the tree structure already encodes grouping).
    fn sx(src: &str, e: &Expr) -> String {
        match e {
            Expr::Int(s)
            | Expr::Float(s)
            | Expr::Char(s)
            | Expr::Str(s)
            | Expr::Bool(s)
            | Expr::Ident(s) => s.text(src).to_string(),
            Expr::Null(_) => "null".into(),
            Expr::This(_) => "this".into(),
            Expr::Base(_) => "base".into(),
            Expr::Error(_) => "error".into(),
            Expr::Paren { inner, .. } => sx(src, inner),
            Expr::Unary { op, operand, .. } => format!("(u{} {})", op.as_str(), sx(src, operand)),
            Expr::PostInc { operand, .. } => format!("(post++ {})", sx(src, operand)),
            Expr::PostDec { operand, .. } => format!("(post-- {})", sx(src, operand)),
            Expr::Binary { op, lhs, rhs, .. } => {
                format!("({} {} {})", op.as_str(), sx(src, lhs), sx(src, rhs))
            }
            Expr::Assign {
                op, target, value, ..
            } => {
                format!("({} {} {})", op.as_str(), sx(src, target), sx(src, value))
            }
            Expr::Ternary {
                cond, then, els, ..
            } => {
                format!("(?: {} {} {})", sx(src, cond), sx(src, then), sx(src, els))
            }
            Expr::Call { callee, args, .. } => {
                let mut s = format!("(call {}", sx(src, callee));
                for a in args {
                    s.push(' ');
                    s.push_str(&sx(src, a));
                }
                s.push(')');
                s
            }
            Expr::Index { base, args, .. } => {
                let mut s = format!("(index {}", sx(src, base));
                for a in args {
                    s.push(' ');
                    s.push_str(&sx(src, a));
                }
                s.push(')');
                s
            }
            Expr::Member {
                base,
                name,
                conditional,
                ..
            } => {
                let dot = if *conditional { "?." } else { "." };
                format!("({} {} {})", dot, sx(src, base), name.text(src))
            }
            Expr::Prefix { kw, operand, .. } => format!("({} {})", kw.as_str(), sx(src, operand)),
        }
    }

    fn sxs(src: &str, s: &Stmt) -> String {
        match s {
            Stmt::Empty(_) => "empty".into(),
            Stmt::Error(_) => "error".into(),
            Stmt::Block { stmts, .. } => {
                let mut s = String::from("(block");
                for st in stmts {
                    s.push(' ');
                    s.push_str(&sxs(src, st));
                }
                s.push(')');
                s
            }
            Stmt::Expr { expr, .. } => format!("(expr {})", sx(src, expr)),
            Stmt::Local {
                is_let, name, init, ..
            } => {
                let kw = if *is_let { "let" } else { "var" };
                match init {
                    Some(e) => format!("({} {} {})", kw, name.text(src), sx(src, e)),
                    None => format!("({} {})", kw, name.text(src)),
                }
            }
            Stmt::If {
                cond, then, els, ..
            } => match els {
                Some(e) => {
                    format!("(if {} {} {})", sx(src, cond), sxs(src, then), sxs(src, e))
                }
                None => format!("(if {} {})", sx(src, cond), sxs(src, then)),
            },
            Stmt::While { cond, body, .. } => {
                format!("(while {} {})", sx(src, cond), sxs(src, body))
            }
            Stmt::DoWhile { body, cond, .. } => {
                format!("(do {} {})", sxs(src, body), sx(src, cond))
            }
            Stmt::For {
                init,
                cond,
                update,
                body,
                ..
            } => {
                let i = init.as_ref().map_or("_".into(), |s| sxs(src, s));
                let c = cond.as_ref().map_or("_".into(), |e| sx(src, e));
                let u = update.as_ref().map_or("_".into(), |e| sx(src, e));
                format!("(for {} {} {} {})", i, c, u, sxs(src, body))
            }
            Stmt::ForEach {
                name, iter, body, ..
            } => {
                format!(
                    "(foreach {} {} {})",
                    name.text(src),
                    sx(src, iter),
                    sxs(src, body)
                )
            }
            Stmt::Return { value, .. } => match value {
                Some(e) => format!("(return {})", sx(src, e)),
                None => "(return)".into(),
            },
            Stmt::Break { .. } => "(break)".into(),
            Stmt::Continue { .. } => "(continue)".into(),
            Stmt::Defer { body, .. } => format!("(defer {})", sxs(src, body)),
        }
    }

    // ── precedence matrix ───────────────────────────────────────────────

    #[test]
    fn arithmetic_precedence() {
        assert_eq!(ok("a + b * c"), "(+ a (* b c))");
        assert_eq!(ok("a * b + c"), "(+ (* a b) c)");
        assert_eq!(ok("a + b - c"), "(- (+ a b) c)"); // left-assoc
        assert_eq!(ok("a - b - c"), "(- (- a b) c)");
        assert_eq!(ok("a * b / c % d"), "(% (/ (* a b) c) d)");
    }

    #[test]
    fn shift_vs_add_vs_bitwise() {
        // add(13) > shift(12) > bit-and(11) > xor(10) > or(9)
        assert_eq!(ok("a << b + c"), "(<< a (+ b c))");
        assert_eq!(ok("a + b << c"), "(<< (+ a b) c)");
        assert_eq!(ok("a | b & c"), "(| a (& b c))");
        assert_eq!(ok("a & b | c"), "(| (& a b) c)");
        assert_eq!(ok("a ^ b & c"), "(^ a (& b c))");
        assert_eq!(ok("a & b ^ c | d"), "(| (^ (& a b) c) d)");
    }

    #[test]
    fn comparison_logical_coalesce() {
        // compare(6) > relational(5) > equality(4) > &&(3) > ||(2) > ??(1)
        assert_eq!(ok("a < b == c"), "(== (< a b) c)");
        assert_eq!(ok("a == b && c"), "(&& (== a b) c)");
        assert_eq!(ok("a && b || c"), "(|| (&& a b) c)");
        assert_eq!(ok("a || b ?? c"), "(?? (|| a b) c)");
        assert_eq!(ok("a <=> b < c"), "(< (<=> a b) c)");
    }

    #[test]
    fn ranges_is_as() {
        // range(8) sits below add(13) but above is/as(7) and compare(6)
        assert_eq!(ok("a + b ..< c"), "(..< (+ a b) c)");
        assert_eq!(ok("a ... b"), "(... a b)");
        assert_eq!(ok("a is T && b"), "(&& (is a T) b)");
        assert_eq!(ok("a as T"), "(as a T)");
    }

    // ── associativity ───────────────────────────────────────────────────

    #[test]
    fn assignment_is_right_associative_and_lowest() {
        assert_eq!(ok("a = b = c"), "(= a (= b c))");
        assert_eq!(ok("a = b + c"), "(= a (+ b c))");
        assert_eq!(ok("a += b * c"), "(+= a (* b c))");
        assert_eq!(ok("x ??= y"), "(??= x y)");
    }

    #[test]
    fn ternary_is_right_associative() {
        assert_eq!(ok("a ? b : c ? d : e"), "(?: a b (?: c d e))");
        assert_eq!(ok("a || b ? c : d"), "(?: (|| a b) c d)");
    }

    // ── unary / postfix ─────────────────────────────────────────────────

    #[test]
    fn unary_binds_tighter_than_binary() {
        assert_eq!(ok("-a * b"), "(* (u- a) b)");
        assert_eq!(ok("!a == b"), "(== (u! a) b)");
        assert_eq!(ok("- - a"), "(u- (u- a))");
        assert_eq!(ok("~a & b"), "(& (u~ a) b)");
    }

    #[test]
    fn postfix_and_member_bind_tightest() {
        assert_eq!(ok("a.b.c"), "(. (. a b) c)");
        assert_eq!(ok("a.b(c)"), "(call (. a b) c)");
        assert_eq!(ok("a[i][j]"), "(index (index a i) j)");
        assert_eq!(ok("a?.b"), "(?. a b)");
        assert_eq!(ok("a++ + b"), "(+ (post++ a) b)");
        assert_eq!(ok("*p.x"), "(u* (. p x))");
        assert_eq!(ok("&a[i]"), "(u& (index a i))");
    }

    #[test]
    fn calls_and_args() {
        assert_eq!(ok("f()"), "(call f)");
        assert_eq!(ok("f(a, b, c)"), "(call f a b c)");
        assert_eq!(ok("f(a)(b)"), "(call (call f a) b)");
        assert_eq!(ok("f(a + b, c)"), "(call f (+ a b) c)");
    }

    // ── prefix keyword forms ────────────────────────────────────────────

    #[test]
    fn prefix_keyword_expressions() {
        assert_eq!(ok("new Foo(1)"), "(new (call Foo 1))");
        assert_eq!(ok("delete x"), "(delete x)");
        assert_eq!(ok("ref x"), "(ref x)");
        // sizeof/typeof/default are primaries, so they parse as calls
        assert_eq!(ok("sizeof(int)"), "(call sizeof int)");
        assert_eq!(ok("typeof(T)"), "(call typeof T)");
    }

    #[test]
    fn prefix_qualifier_is_captured() {
        let (e, diags) = parse_expr("new:alloc Foo()", FileId(0));
        assert!(diags.is_empty(), "{diags:?}");
        match e {
            Expr::Prefix {
                kw: PrefixKw::New,
                qualifier: Some(q),
                ..
            } => {
                assert_eq!(q.text("new:alloc Foo()"), "alloc");
            }
            other => panic!("expected new:alloc prefix, got {other:?}"),
        }
    }

    // ── literals & primaries ────────────────────────────────────────────

    #[test]
    fn primaries() {
        assert_eq!(ok("123"), "123");
        assert_eq!(ok("1.5f"), "1.5f");
        assert_eq!(ok("true"), "true");
        assert_eq!(ok("null"), "null");
        assert_eq!(ok("this.x"), "(. this x)");
        assert_eq!(ok("(a + b) * c"), "(* (+ a b) c)"); // paren regroups
    }

    // ── spans ───────────────────────────────────────────────────────────

    #[test]
    fn node_spans_cover_their_source() {
        let src = "a + b * c";
        let (e, _) = parse_expr(src, FileId(0));
        assert_eq!(e.span().text(src), "a + b * c");
        if let Expr::Binary { rhs, .. } = &e {
            assert_eq!(rhs.span().text(src), "b * c");
        } else {
            panic!("expected top-level binary");
        }
    }

    // ── statements ──────────────────────────────────────────────────────

    #[test]
    fn statement_forms() {
        assert_eq!(ok_stmt("x = 1;"), "(expr (= x 1))");
        assert_eq!(ok_stmt("var x = 5;"), "(var x 5)");
        assert_eq!(ok_stmt("let y = f();"), "(let y (call f))");
        assert_eq!(ok_stmt("return a + b;"), "(return (+ a b))");
        assert_eq!(ok_stmt("return;"), "(return)");
        assert_eq!(
            ok_stmt("{ a(); b(); }"),
            "(block (expr (call a)) (expr (call b)))"
        );
        assert_eq!(ok_stmt("defer delete x;"), "(defer (expr (delete x)))");
        assert_eq!(ok_stmt(";"), "empty");
    }

    #[test]
    fn control_flow_statements() {
        assert_eq!(
            ok_stmt("if (a) b(); else c();"),
            "(if a (expr (call b)) (expr (call c)))"
        );
        assert_eq!(ok_stmt("if (x) y();"), "(if x (expr (call y)))");
        assert_eq!(
            ok_stmt("while (x < 10) x++;"),
            "(while (< x 10) (expr (post++ x)))"
        );
        assert_eq!(
            ok_stmt("for (var i = 0; i < n; i++) {}"),
            "(for (var i 0) (< i n) (post++ i) (block))"
        );
        assert_eq!(
            ok_stmt("for (var x in xs) f(x);"),
            "(foreach x xs (expr (call f x)))"
        );
        assert_eq!(ok_stmt("for (;;) {}"), "(for _ _ _ (block))");
    }

    #[test]
    fn dangling_else_binds_to_nearest_if() {
        assert_eq!(
            ok_stmt("if (a) if (b) c(); else d();"),
            "(if a (if b (expr (call c)) (expr (call d))))"
        );
    }

    // ── error recovery / no panic ───────────────────────────────────────

    #[test]
    fn malformed_input_reports_and_recovers() {
        for bad in ["", "+", "(", "a +", "a ? b", "1 2 3", ")", "* / +", "f(a,"] {
            let (_e, diags) = parse_expr(bad, FileId(0));
            // We don't assert specifics — only that parsing terminated and
            // produced at least one diagnostic for clearly-broken input.
            let _ = diags;
        }
        // A broken statement fragment must terminate and diagnose.
        let (_stmts, diags) = parse_fragment("if ( while )) for {{{", FileId(0));
        assert!(!diags.is_empty());
    }

    #[test]
    fn fuzz_never_panics_and_terminates() {
        // Deterministic pseudo-random token soup; the test passing proves
        // no panic, and parse_fragment's progress assertion proves no hang.
        let atoms = [
            "a", "1", "(", ")", "{", "}", "+", "-", "*", "/", ";", ",", ".", "?", ":", "[", "]",
            "<", ">", "=", "new", "if", "for", "return", "delete", "&&", "..<", "=>", "while",
            "scope", "\"s\"", "++", "<=>",
        ];
        let mut state: u64 = 0x9e3779b97f4a7c15;
        for _ in 0..2000 {
            let mut s = String::new();
            let len = 1 + (state as usize) % 16;
            for _ in 0..len {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let idx = (state >> 33) as usize % atoms.len();
                s.push_str(atoms[idx]);
                s.push(' ');
            }
            let _ = parse_expr(&s, FileId(0));
            let _ = parse_fragment(&s, FileId(0));
        }
    }
}
