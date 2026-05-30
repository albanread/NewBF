//! Conditional-compilation preprocessing.
//!
//! The lexer emits each `#…` directive line as a single `PreprocLine`
//! trivia token. This pass walks the lexed token stream, tracks the
//! `#if`/`#elif`/`#else`/`#endif` nesting, evaluates the conditions
//! against a set of defined symbols, and **drops the tokens that fall in
//! inactive branches** before the parser sees them. Without this, both
//! arms of an `#if`/`#else` parse as live code and later phases see
//! contradictory duplicate definitions.
//!
//! The directive expression grammar is the C#/Beef subset: identifiers
//! (true iff defined), `true`/`false`, `!`, `&&`, `||`, and parentheses.
//! `#define`/`#undef` in active regions mutate the symbol set. Unknown
//! directives (`#pragma`, `#region`, `#warning`, `#unwarn`, …) are ignored.
//!
//! The default symbol set is empty: the goal here is *deterministic
//! single-branch selection* (which kills the duplicate-definition noise),
//! not faithful build-configuration emulation — a config-driven define set
//! is future work.

use std::collections::HashSet;

use newbf_lexer::{Token, TokenKind};

/// Filter `toks` (a full lexed stream, trivia included) down to the tokens
/// that survive conditional compilation, using an empty initial symbol set.
pub(crate) fn preprocess(src: &str, toks: Vec<Token>) -> Vec<Token> {
    let mut defines = HashSet::new();
    preprocess_with(src, toks, &mut defines)
}

/// One `#if`/`#elif`/`#else` nesting level.
struct Frame {
    /// Whether tokens at this level are currently kept.
    active: bool,
    /// Whether any branch of this `#if` chain has been taken yet.
    any_taken: bool,
}

fn current_active(frames: &[Frame]) -> bool {
    frames.last().is_none_or(|f| f.active)
}

pub(crate) fn preprocess_with(
    src: &str,
    toks: Vec<Token>,
    defines: &mut HashSet<String>,
) -> Vec<Token> {
    let mut out = Vec::with_capacity(toks.len());
    let mut frames: Vec<Frame> = Vec::new();

    for t in toks {
        if t.kind == TokenKind::PreprocLine {
            if let Some((dir, arg)) = parse_directive(t.span.text(src)) {
                apply_directive(dir, arg, &mut frames, defines);
            }
            // Keep the directive line itself (it's trivia; the parser skips
            // it) so spans still tile and reports stay legible.
            out.push(t);
            continue;
        }
        // Always keep trivia (harmless — the parser filters it) and EOF;
        // keep real tokens only inside an active branch.
        if t.kind == TokenKind::Eof || t.kind.is_trivia() || current_active(&frames) {
            out.push(t);
        }
    }
    out
}

/// Split a directive line into `(keyword, argument)`. The argument has any
/// trailing line comment stripped. Returns `None` for a bare `#`.
fn parse_directive(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_start();
    let rest = line.strip_prefix('#')?.trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(rest.len());
    let (dir, arg) = rest.split_at(end);
    if dir.is_empty() {
        return None;
    }
    // Strip a trailing `// …` comment from the argument.
    let arg = match arg.find("//") {
        Some(i) => &arg[..i],
        None => arg,
    };
    Some((dir, arg.trim()))
}

fn apply_directive(dir: &str, arg: &str, frames: &mut Vec<Frame>, defines: &mut HashSet<String>) {
    match dir {
        "if" => {
            let parent = current_active(frames);
            let cond = parent && eval(arg, defines);
            frames.push(Frame {
                active: cond,
                any_taken: cond,
            });
        }
        "elif" => {
            if let Some(top) = frames.pop() {
                let parent = current_active(frames);
                let cond = parent && !top.any_taken && eval(arg, defines);
                frames.push(Frame {
                    active: cond,
                    any_taken: top.any_taken || cond,
                });
            }
        }
        "else" => {
            if let Some(top) = frames.pop() {
                let parent = current_active(frames);
                frames.push(Frame {
                    active: parent && !top.any_taken,
                    any_taken: true,
                });
            }
        }
        "endif" => {
            frames.pop();
        }
        "define" => {
            if current_active(frames)
                && let Some(name) = arg.split_whitespace().next()
            {
                defines.insert(name.to_string());
            }
        }
        "undef" => {
            if current_active(frames)
                && let Some(name) = arg.split_whitespace().next()
            {
                defines.remove(name);
            }
        }
        _ => {} // pragma / region / warning / error / unwarn / …
    }
}

// ── directive-expression evaluation ────────────────────────────────────────

#[derive(Clone, PartialEq, Eq, Debug)]
enum PpTok {
    Ident(String),
    True,
    False,
    Not,
    And,
    Or,
    LParen,
    RParen,
}

fn lex_pp(expr: &str) -> Vec<PpTok> {
    let bytes = expr.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b'(' => {
                toks.push(PpTok::LParen);
                i += 1;
            }
            b')' => {
                toks.push(PpTok::RParen);
                i += 1;
            }
            b'!' => {
                toks.push(PpTok::Not);
                i += 1;
            }
            b'&' if bytes.get(i + 1) == Some(&b'&') => {
                toks.push(PpTok::And);
                i += 2;
            }
            b'|' if bytes.get(i + 1) == Some(&b'|') => {
                toks.push(PpTok::Or);
                i += 2;
            }
            c if c == b'_' || c.is_ascii_alphanumeric() => {
                let start = i;
                while i < bytes.len() && (bytes[i] == b'_' || bytes[i].is_ascii_alphanumeric()) {
                    i += 1;
                }
                let word = &expr[start..i];
                toks.push(match word {
                    "true" => PpTok::True,
                    "false" => PpTok::False,
                    _ => PpTok::Ident(word.to_string()),
                });
            }
            // Unknown char (e.g. an unsupported `==`): skip it. The
            // surrounding identifiers still evaluate; worst case we pick a
            // branch rather than crash.
            _ => i += 1,
        }
    }
    toks
}

struct PpParser<'a> {
    toks: &'a [PpTok],
    pos: usize,
    defines: &'a HashSet<String>,
}

impl PpParser<'_> {
    fn peek(&self) -> Option<&PpTok> {
        self.toks.get(self.pos)
    }

    fn or(&mut self) -> bool {
        let mut v = self.and();
        while self.peek() == Some(&PpTok::Or) {
            self.pos += 1;
            v = self.and() || v;
        }
        v
    }
    fn and(&mut self) -> bool {
        let mut v = self.unary();
        while self.peek() == Some(&PpTok::And) {
            self.pos += 1;
            v = self.unary() && v;
        }
        v
    }
    fn unary(&mut self) -> bool {
        if self.peek() == Some(&PpTok::Not) {
            self.pos += 1;
            return !self.unary();
        }
        self.primary()
    }
    fn primary(&mut self) -> bool {
        let tok = self.toks.get(self.pos).cloned();
        self.pos += 1;
        match tok {
            Some(PpTok::LParen) => {
                let v = self.or();
                if self.peek() == Some(&PpTok::RParen) {
                    self.pos += 1;
                }
                v
            }
            Some(PpTok::True) => true,
            Some(PpTok::False) => false,
            Some(PpTok::Ident(name)) => self.defines.contains(&name),
            _ => false,
        }
    }
}

fn eval(expr: &str, defines: &HashSet<String>) -> bool {
    let toks = lex_pp(expr);
    if toks.is_empty() {
        return false;
    }
    let mut p = PpParser {
        toks: &toks,
        pos: 0,
        defines,
    };
    p.or()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defs(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn expr_evaluation() {
        let d = defs(&["DEBUG", "WIN"]);
        assert!(eval("DEBUG", &d));
        assert!(!eval("RELEASE", &d));
        assert!(eval("!RELEASE", &d));
        assert!(eval("DEBUG && WIN", &d));
        assert!(!eval("DEBUG && LINUX", &d));
        assert!(eval("DEBUG || LINUX", &d));
        assert!(eval("(DEBUG || LINUX) && WIN", &d));
        assert!(eval("true", &d));
        assert!(!eval("false", &d));
        assert!(!eval("", &d));
    }

    #[test]
    fn directive_split() {
        assert_eq!(parse_directive("#if DEBUG"), Some(("if", "DEBUG")));
        assert_eq!(parse_directive("  #  endif"), Some(("endif", "")));
        assert_eq!(parse_directive("#if FOO // note"), Some(("if", "FOO")));
        assert_eq!(parse_directive("#"), None);
    }

    /// Lex `src`, preprocess, and return the surviving non-trivia token
    /// kinds' source text joined by spaces.
    fn kept(src: &str) -> String {
        use newbf_lexer::{FileId, lex};
        let toks = lex(src, FileId(0));
        let pre = preprocess(src, toks);
        pre.iter()
            .filter(|t| !t.kind.is_trivia() && t.kind != TokenKind::Eof)
            .map(|t| t.span.text(src))
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn if_else_keeps_one_branch() {
        // With no symbols defined, the `#else` branch wins.
        let src = "#if DEBUG\nint a;\n#else\nint b;\n#endif";
        assert_eq!(kept(src), "int b ;");
    }

    #[test]
    fn nested_inactive_stays_inactive() {
        // Inner `#if true` must not resurrect tokens inside a dead outer arm.
        let src = "#if FOO\n#if true\nint a;\n#endif\n#endif\nint c;";
        assert_eq!(kept(src), "int c ;");
    }

    #[test]
    fn define_then_use() {
        let src = "#define X\n#if X\nint a;\n#endif";
        assert_eq!(kept(src), "int a ;");
    }

    #[test]
    fn elif_chain() {
        let src = "#if A\nint a;\n#elif B\nint b;\n#else\nint c;\n#endif";
        assert_eq!(kept(src), "int c ;");
        let src2 = "#define B\n#if A\nint a;\n#elif B\nint b;\n#else\nint c;\n#endif";
        assert_eq!(kept(src2), "int b ;");
    }
}
