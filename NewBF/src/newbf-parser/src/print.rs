//! Schema-stable pretty-printer for the AST — the `dump-parse` report.
//!
//! Renders an indented tree (two spaces per level). Heterogeneous or
//! optional children are printed under explicit `label:` lines so the
//! shape is unambiguous and diff-friendly.

use crate::ast::*;
use newbf_lexer::Span;

/// Render a parsed statement fragment as an indented AST tree.
pub fn format_parse(src: &str, stmts: &[Stmt]) -> String {
    let mut p = Printer {
        src,
        out: String::new(),
    };
    for s in stmts {
        p.stmt(s, 0);
    }
    p.out
}

/// Render a single expression as an indented AST tree.
pub fn format_expr(src: &str, e: &Expr) -> String {
    let mut p = Printer {
        src,
        out: String::new(),
    };
    p.expr(e, 0);
    p.out
}

struct Printer<'a> {
    src: &'a str,
    out: String,
}

impl Printer<'_> {
    fn line(&mut self, depth: usize, text: &str) {
        for _ in 0..depth {
            self.out.push_str("  ");
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn txt(&self, span: Span) -> String {
        let s = span.text(self.src);
        const MAX: usize = 40;
        if s.chars().count() > MAX {
            let end = s.char_indices().nth(MAX).map_or(s.len(), |(i, _)| i);
            format!("{:?}…", &s[..end])
        } else {
            format!("{s:?}")
        }
    }

    fn labeled_expr(&mut self, depth: usize, label: &str, e: &Expr) {
        self.line(depth, &format!("{label}:"));
        self.expr(e, depth + 1);
    }

    fn labeled_stmt(&mut self, depth: usize, label: &str, s: &Stmt) {
        self.line(depth, &format!("{label}:"));
        self.stmt(s, depth + 1);
    }

    fn expr(&mut self, e: &Expr, d: usize) {
        match e {
            Expr::Int(s) => self.line(d, &format!("Int {}", self.txt(*s))),
            Expr::Float(s) => self.line(d, &format!("Float {}", self.txt(*s))),
            Expr::Char(s) => self.line(d, &format!("Char {}", self.txt(*s))),
            Expr::Str(s) => self.line(d, &format!("Str {}", self.txt(*s))),
            Expr::Bool(s) => self.line(d, &format!("Bool {}", self.txt(*s))),
            Expr::Null(_) => self.line(d, "Null"),
            Expr::Ident(s) => self.line(d, &format!("Ident {}", self.txt(*s))),
            Expr::This(_) => self.line(d, "This"),
            Expr::Base(_) => self.line(d, "Base"),
            Expr::Error(_) => self.line(d, "Error"),
            Expr::Paren { inner, .. } => {
                self.line(d, "Paren");
                self.expr(inner, d + 1);
            }
            Expr::Unary { op, operand, .. } => {
                self.line(d, &format!("Unary {:?}", op.as_str()));
                self.expr(operand, d + 1);
            }
            Expr::PostInc { operand, .. } => {
                self.line(d, "PostInc");
                self.expr(operand, d + 1);
            }
            Expr::PostDec { operand, .. } => {
                self.line(d, "PostDec");
                self.expr(operand, d + 1);
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                self.line(d, &format!("Binary {:?}", op.as_str()));
                self.expr(lhs, d + 1);
                self.expr(rhs, d + 1);
            }
            Expr::Assign {
                op, target, value, ..
            } => {
                self.line(d, &format!("Assign {:?}", op.as_str()));
                self.expr(target, d + 1);
                self.expr(value, d + 1);
            }
            Expr::Ternary {
                cond, then, els, ..
            } => {
                self.line(d, "Ternary");
                self.labeled_expr(d + 1, "cond", cond);
                self.labeled_expr(d + 1, "then", then);
                self.labeled_expr(d + 1, "else", els);
            }
            Expr::Call { callee, args, .. } => {
                self.line(d, "Call");
                self.labeled_expr(d + 1, "callee", callee);
                self.line(d + 1, "args:");
                for a in args {
                    self.expr(a, d + 2);
                }
            }
            Expr::Index { base, args, .. } => {
                self.line(d, "Index");
                self.labeled_expr(d + 1, "base", base);
                self.line(d + 1, "args:");
                for a in args {
                    self.expr(a, d + 2);
                }
            }
            Expr::Member {
                base,
                name,
                conditional,
                ..
            } => {
                let dot = if *conditional { "?." } else { "." };
                self.line(d, &format!("Member {dot}{}", self.txt(*name)));
                self.expr(base, d + 1);
            }
            Expr::Prefix {
                kw,
                qualifier,
                operand,
                ..
            } => {
                let q = match qualifier {
                    Some(s) => format!(":{}", self.txt(*s)),
                    None => String::new(),
                };
                self.line(d, &format!("Prefix {:?}{q}", kw.as_str()));
                self.expr(operand, d + 1);
            }
        }
    }

    fn stmt(&mut self, s: &Stmt, d: usize) {
        match s {
            Stmt::Empty(_) => self.line(d, "Empty"),
            Stmt::Error(_) => self.line(d, "Error"),
            Stmt::Block { stmts, .. } => {
                self.line(d, "Block");
                for st in stmts {
                    self.stmt(st, d + 1);
                }
            }
            Stmt::Expr { expr, .. } => {
                self.line(d, "ExprStmt");
                self.expr(expr, d + 1);
            }
            Stmt::Local {
                is_let, name, init, ..
            } => {
                let kw = if *is_let { "let" } else { "var" };
                self.line(d, &format!("Local {kw} {}", self.txt(*name)));
                if let Some(e) = init {
                    self.labeled_expr(d + 1, "init", e);
                }
            }
            Stmt::If {
                cond, then, els, ..
            } => {
                self.line(d, "If");
                self.labeled_expr(d + 1, "cond", cond);
                self.labeled_stmt(d + 1, "then", then);
                if let Some(e) = els {
                    self.labeled_stmt(d + 1, "else", e);
                }
            }
            Stmt::While { cond, body, .. } => {
                self.line(d, "While");
                self.labeled_expr(d + 1, "cond", cond);
                self.labeled_stmt(d + 1, "body", body);
            }
            Stmt::DoWhile { body, cond, .. } => {
                self.line(d, "DoWhile");
                self.labeled_stmt(d + 1, "body", body);
                self.labeled_expr(d + 1, "cond", cond);
            }
            Stmt::For {
                init,
                cond,
                update,
                body,
                ..
            } => {
                self.line(d, "For");
                match init {
                    Some(s) => self.labeled_stmt(d + 1, "init", s),
                    None => self.line(d + 1, "init: <none>"),
                }
                match cond {
                    Some(e) => self.labeled_expr(d + 1, "cond", e),
                    None => self.line(d + 1, "cond: <none>"),
                }
                match update {
                    Some(e) => self.labeled_expr(d + 1, "update", e),
                    None => self.line(d + 1, "update: <none>"),
                }
                self.labeled_stmt(d + 1, "body", body);
            }
            Stmt::ForEach {
                name, iter, body, ..
            } => {
                self.line(d, &format!("ForEach {}", self.txt(*name)));
                self.labeled_expr(d + 1, "iter", iter);
                self.labeled_stmt(d + 1, "body", body);
            }
            Stmt::Return { value, .. } => {
                self.line(d, "Return");
                if let Some(e) = value {
                    self.expr(e, d + 1);
                }
            }
            Stmt::Break { label, .. } => match label {
                Some(s) => self.line(d, &format!("Break {}", self.txt(*s))),
                None => self.line(d, "Break"),
            },
            Stmt::Continue { label, .. } => match label {
                Some(s) => self.line(d, &format!("Continue {}", self.txt(*s))),
                None => self.line(d, "Continue"),
            },
            Stmt::Defer { body, .. } => {
                self.line(d, "Defer");
                self.stmt(body, d + 1);
            }
        }
    }
}
