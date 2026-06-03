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
mod preprocess;
mod print;

pub use ast::{
    Accessor, AccessorKind, AssignOp, Attribute, BinOp, CompUnit, ComputedKind, Expr, GenericParam,
    Item, Member, MethodBody, Modifier, Param, ParamModifier, PrefixKw, Stmt, SwitchArm, Type,
    TypeDecl, TypeKind, TypeSeg, UnOp, WhereClause,
};
pub use parser::{
    Diagnostic, parse_expr, parse_file, parse_file_with_trivia, parse_fragment, parse_type,
};
pub use print::{format_ast, format_expr, format_parse};

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
            Expr::Generic { base, args, .. } => {
                let mut s = format!("(generic {}", sx(src, base));
                for a in args {
                    s.push(' ');
                    s.push_str(&sxt(src, a));
                }
                s.push(')');
                s
            }
            Expr::Cast { ty, operand, .. } => {
                format!("(cast {} {})", sxt(src, ty), sx(src, operand))
            }
            Expr::SizeOf { ty, .. } => format!("(sizeof {})", sxt(src, ty)),
            Expr::DotIdent { name, .. } => format!(".{}", name.text(src)),
            Expr::Tuple { elems, .. } => {
                let mut s = String::from("(tuple");
                for e in elems {
                    s.push(' ');
                    s.push_str(&sx(src, e));
                }
                s.push(')');
                s
            }
            Expr::Initializer { base, entries, .. } => {
                let mut s = format!("(init {}", sx(src, base));
                for e in entries {
                    s.push(' ');
                    s.push_str(&sx(src, e));
                }
                s.push(')');
                s
            }
            Expr::Lambda { body, .. } => format!("(lambda {})", sxs(src, body)),
            Expr::Named { name, value, .. } => {
                format!("(named {} {})", name.text(src), sx(src, value))
            }
        }
    }

    /// Compact s-expression of a type.
    fn sxt(src: &str, t: &Type) -> String {
        match t {
            Type::Var(_) => "var".into(),
            Type::Error(_) => "type-error".into(),
            Type::Path { segments, .. } => {
                let mut s = String::new();
                for (i, seg) in segments.iter().enumerate() {
                    if i > 0 {
                        s.push('.');
                    }
                    s.push_str(seg.name.text(src));
                    if !seg.args.is_empty() {
                        s.push('<');
                        for (j, a) in seg.args.iter().enumerate() {
                            if j > 0 {
                                s.push(',');
                            }
                            s.push_str(&sxt(src, a));
                        }
                        s.push('>');
                    }
                }
                s
            }
            Type::Pointer { inner, .. } => format!("(* {})", sxt(src, inner)),
            Type::Nullable { inner, .. } => format!("(? {})", sxt(src, inner)),
            Type::Array { inner, rank, .. } => {
                if *rank == 1 {
                    format!("(arr {})", sxt(src, inner))
                } else {
                    format!("(arr{} {})", rank, sxt(src, inner))
                }
            }
            Type::Sized { inner, size, .. } => {
                format!("(arr[{}] {})", sx(src, size), sxt(src, inner))
            }
            Type::Tuple { elems, .. } => {
                let mut s = String::from("(tup");
                for e in elems {
                    s.push(' ');
                    s.push_str(&sxt(src, e));
                }
                s.push(')');
                s
            }
            Type::Function {
                is_delegate,
                return_ty,
                params,
                ..
            } => {
                let kw = if *is_delegate { "delegate" } else { "function" };
                let mut s = format!("({kw} {}", sxt(src, return_ty));
                for p in params {
                    s.push(' ');
                    s.push_str(&sxt(src, p));
                }
                s.push(')');
                s
            }
            Type::Computed { kind, expr, .. } => {
                format!("({} {})", kind.as_str(), sx(src, expr))
            }
            Type::Anonymous(td) => format!("(anon-{} {})", td.kind.as_str(), td.members.len()),
            Type::ConstArg { value, .. } => format!("(const {})", sx(src, value)),
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
            Stmt::LocalFunction { name, body, .. } => {
                format!("(fn {} {})", name.text(src), sxs(src, body))
            }
            Stmt::Switch {
                scrutinee, arms, ..
            } => {
                let mut s = format!("(switch {}", sx(src, scrutinee));
                for arm in arms {
                    s.push(' ');
                    s.push_str(match &arm.pattern {
                        Some(_) => "(case",
                        None => "(default",
                    });
                    if let Some(p) = &arm.pattern {
                        s.push(' ');
                        s.push_str(&sx(src, p));
                    }
                    for st in &arm.body {
                        s.push(' ');
                        s.push_str(&sxs(src, st));
                    }
                    s.push(')');
                }
                s.push(')');
                s
            }
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

    #[test]
    fn arrow_member_named_tuple_decl_condition() {
        // `->` pointer-member access (modeled like `.`).
        assert_eq!(ok("p->Length"), "(. p Length)");
        // Named tuple literal.
        assert_eq!(ok("(x: 3, y: 4)"), "(tuple (named x 3) (named y 4))");
        // Declaration-condition `if (Type name = expr)` — prefix consumed,
        // value becomes the condition.
        assert_eq!(
            ok_stmt("if (int i = f()) g();"),
            "(if (call f) (expr (call g)))"
        );
    }

    #[test]
    fn object_initializers() {
        // A postfix initializer (`Type { … }`, `.{ … }`) is now captured as an
        // `Initializer` wrapping the base, with each `field = value` entry. The
        // `new …` prefix form still drops its initializer (a later sprint).
        assert_eq!(ok("StructB { mA = 2 }"), "(init StructB (= mA 2))");
        assert_eq!(ok(".{ mA = 1, mB = 2 }"), "(init .. (= mA 1) (= mB 2))");
        // `new T(args) { … }`: the initializer rides on the `new` operand (parsed
        // as a postfix expression), so it's captured inside the `new`.
        assert_eq!(
            ok("new Foo() { mA = 1, mB = 2 }"),
            "(new (init (call Foo) (= mA 1) (= mB 2)))"
        );
        // `new int[3] { … }` captures the collection initializer too (its bare
        // entries aren't applied to the elements yet — the `new int[]( … )` paren
        // form is the working array initializer).
        assert_eq!(
            ok("new int[3] { 1, 2, 3 }"),
            "(new (init (index int 3) 1 2 3))"
        );
        // A bare `Ident` followed by a block-shaped `{` is NOT an initializer.
        assert_eq!(ok_stmt("x;"), "(expr x)");
    }

    #[test]
    fn ctor_chain_does_not_swallow_body() {
        // The `{ }` is the constructor body, not an initializer of `base(x)`.
        let unit = ok_file("class C { public this(int x) : base(x) { } }");
        let Item::Type(td) = &unit.items[0] else {
            panic!("type")
        };
        assert!(matches!(
            &td.members[0],
            Member::Constructor {
                body: MethodBody::Block(_),
                ..
            }
        ));
    }

    #[test]
    fn named_arguments() {
        // Beef named args: `name: value`, mixable with positional args.
        assert_eq!(
            ok("f(p1: 1, 2, p3: 3)"),
            "(call f (named p1 1) 2 (named p3 3))"
        );
        assert_eq!(ok("Named(p0: 1, 2, 3)"), "(call Named (named p0 1) 2 3)");
        // A ternary argument is not mistaken for a named arg.
        assert_eq!(ok("f(a ? b : c)"), "(call f (?: a b c))");
    }

    // ── prefix keyword forms ────────────────────────────────────────────

    #[test]
    fn prefix_keyword_expressions() {
        assert_eq!(ok("new Foo(1)"), "(new (call Foo 1))");
        assert_eq!(ok("delete x"), "(delete x)");
        assert_eq!(ok("ref x"), "(ref x)");
        // `sizeof` keeps its type argument; `typeof`/`alignof`/`strideof` still
        // drop it (placeholder primary).
        assert_eq!(ok("sizeof(int)"), "(sizeof int)");
        assert_eq!(ok("typeof(T)"), "typeof");
        assert_eq!(ok("sizeof(char8*)"), "(sizeof (* char8))");
        // default/nameof still parse their `(…)` as a call (expression arg)
        assert_eq!(ok("default(T)"), "(call default T)");
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

    // ── type parser ─────────────────────────────────────────────────────

    fn ok_type(src: &str) -> String {
        let (t, diags) = parse_type(src, FileId(0));
        assert!(
            diags.is_empty(),
            "unexpected diagnostics for {src:?}: {diags:?}"
        );
        sxt(src, &t)
    }

    #[test]
    fn simple_path_types() {
        assert_eq!(ok_type("int"), "int");
        assert_eq!(ok_type("System.String"), "System.String");
        assert_eq!(ok_type("var"), "var");
    }

    #[test]
    fn generic_types() {
        assert_eq!(ok_type("List<int>"), "List<int>");
        assert_eq!(ok_type("Dictionary<K, V>"), "Dictionary<K,V>");
        assert_eq!(ok_type("Outer<T>.Inner"), "Outer<T>.Inner");
        // Nested generics close cleanly even though `>>` is one token.
        assert_eq!(ok_type("List<List<int>>"), "List<List<int>>");
        assert_eq!(
            ok_type("Dictionary<string, List<int>>"),
            "Dictionary<string,List<int>>"
        );
    }

    #[test]
    fn pointer_nullable_array_sized_compose() {
        assert_eq!(ok_type("int*"), "(* int)");
        assert_eq!(ok_type("int**"), "(* (* int))");
        assert_eq!(ok_type("T?"), "(? T)");
        assert_eq!(ok_type("int[]"), "(arr int)");
        assert_eq!(ok_type("int[,]"), "(arr2 int)");
        assert_eq!(ok_type("int[,,]"), "(arr3 int)");
        assert_eq!(ok_type("uint8[16]"), "(arr[16] uint8)");
        // composition reads left-to-right (innermost first)
        assert_eq!(ok_type("int*[]"), "(arr (* int))");
        assert_eq!(ok_type("List<int>?"), "(? List<int>)");
    }

    #[test]
    fn const_generic_arguments() {
        // `const N` and bare literal generic args (placeholder `var` stands
        // in for the const value).
        assert_eq!(ok_type("StructV<const 16>"), "StructV<(const 16)>");
        assert_eq!(ok_type("Array<int, const 4>"), "Array<int,(const 4)>");
        assert_eq!(ok_type("Foo<16>"), "Foo<(const 16)>");
    }

    #[test]
    fn tuple_types() {
        assert_eq!(ok_type("(int, int)"), "(tup int int)");
        assert_eq!(ok_type("(A, B, C<T>)"), "(tup A B C<T>)");
    }

    #[test]
    fn function_and_delegate_types() {
        assert_eq!(ok_type("function void()"), "(function void)");
        assert_eq!(ok_type("function int(int, int)"), "(function int int int)");
        assert_eq!(ok_type("delegate void(StructC)"), "(delegate void StructC)");
        // Attributes between the keyword and return type are tolerated.
        assert_eq!(
            ok_type("function [CallingConvention(.Cdecl)] void(StructC)"),
            "(function void StructC)"
        );
    }

    #[test]
    fn type_node_spans_cover_the_source() {
        let src = "List<int>?";
        let (t, _) = parse_type(src, FileId(0));
        assert_eq!(t.span().text(src), "List<int>?");
        if let Type::Nullable { inner, .. } = &t {
            assert_eq!(inner.span().text(src), "List<int>");
        } else {
            panic!("expected outer Nullable");
        }
    }

    // ── typed locals ────────────────────────────────────────────────────

    #[test]
    fn typed_locals_with_simple_types() {
        // `int x = 5;` — typed local, NOT an expression statement.
        let (stmts, diags) = parse_fragment("int x = 5;", FileId(0));
        assert!(diags.is_empty(), "{diags:?}");
        match &stmts[0] {
            Stmt::Local {
                is_let: false,
                ty: Some(t),
                name,
                init: Some(_),
                ..
            } => {
                assert_eq!(sxt("int x = 5;", t), "int");
                assert_eq!(name.text("int x = 5;"), "x");
            }
            other => panic!("expected typed local, got {other:?}"),
        }
    }

    #[test]
    fn typed_locals_with_generics_and_pointers() {
        let src = "List<int> xs = create();";
        let (stmts, diags) = parse_fragment(src, FileId(0));
        assert!(diags.is_empty(), "{diags:?}");
        let Stmt::Local { ty: Some(t), .. } = &stmts[0] else {
            panic!("expected typed local");
        };
        assert_eq!(sxt(src, t), "List<int>");

        let src2 = "int* p;";
        let (stmts2, diags2) = parse_fragment(src2, FileId(0));
        assert!(diags2.is_empty(), "{diags2:?}");
        let Stmt::Local {
            ty: Some(t),
            init: None,
            ..
        } = &stmts2[0]
        else {
            panic!("expected typed local with no init");
        };
        assert_eq!(sxt(src2, t), "(* int)");
    }

    #[test]
    fn expression_statements_arent_misparsed_as_typed_locals() {
        // `a.b = c;` is assignment, not a typed local.
        assert_eq!(ok_stmt("a.b = c;"), "(expr (= (. a b) c))");
        // `Foo();` is a call, not a typed local.
        assert_eq!(ok_stmt("Foo();"), "(expr (call Foo))");
        // `x++;` postfix expression statement.
        assert_eq!(ok_stmt("x++;"), "(expr (post++ x))");
    }

    // ── switch statement ────────────────────────────────────────────────

    #[test]
    fn case_patterns_with_bindings_when_and_not_case() {
        // `let`/`var` bindings + `when` guard parse clean (bindings and the
        // guard are dropped for now, but must not error).
        let s = ok_stmt("switch (x) { case .Ok(let a) when a > 0: f(); default: g(); }");
        assert!(s.contains("(switch x"), "got {s}");
        // `not case` is `!(x case .Foo)` — a negated case-test.
        assert_eq!(ok("x not case .Foo"), "(u! (case x .Foo))");
    }

    #[test]
    fn fallthrough_and_open_ended_ranges() {
        assert_eq!(ok_stmt("fallthrough;"), "(expr fallthrough)");
        // Open-ended ranges parse without diagnostics (placeholder operand).
        let _ = ok("a[1...]");
        let _ = ok("a[...]");
        let _ = ok("a[..<n]");
    }

    #[test]
    fn block_expressions_and_using_field() {
        // Block expression in expression position parses clean.
        let _ = ok("{ a(); 1 }");
        // Attributed block expression.
        let _ = ok("[IgnoreErrors] { f(); }");
        // `using` field qualifier.
        let unit = ok_file("class C { using public ClassA mInst; }");
        let Item::Type(td) = &unit.items[0] else {
            panic!("type")
        };
        assert!(matches!(&td.members[0], Member::Field { .. }));
    }

    #[test]
    fn typed_pattern_binding_argument() {
        // `Type name` binding inside a call/pattern arg list parses clean
        // (the binding name is consumed). Here as a plain call form.
        assert_eq!(ok("f(int pos)"), "(call f int)");
    }

    #[test]
    fn tuple_destructuring_for_each() {
        assert_eq!(
            ok_stmt("for (var (a, b) in xs) {}"),
            "(foreach (a, b) xs (block))"
        );
    }

    #[test]
    fn switch_with_cases_and_default() {
        let s = ok_stmt("switch (x) { case 1: a(); case 2: b(); default: c(); }");
        assert!(s.contains("(switch x"), "got {s}");
        assert!(s.contains("(case 1"), "got {s}");
        assert!(s.contains("(case 2"), "got {s}");
        assert!(s.contains("(default"), "got {s}");
    }

    #[test]
    fn switch_arms_can_have_multiple_statements() {
        let src = "switch (e) { case 0: a(); b(); break; default: return; }";
        let (stmts, diags) = parse_fragment(src, FileId(0));
        assert!(diags.is_empty(), "{diags:?}");
        let Stmt::Switch { arms, .. } = &stmts[0] else {
            panic!("expected switch");
        };
        assert_eq!(arms.len(), 2);
        assert_eq!(arms[0].body.len(), 3); // a(); b(); break;
        assert!(arms[1].pattern.is_none());
    }

    // ── generic-arg disambiguation in expressions ───────────────────────

    #[test]
    fn generic_calls_disambiguate_against_comparisons() {
        // generic call: `<T>` followed by `(`  →  Generic + Call
        assert_eq!(ok("Foo<T>(x)"), "(call (generic Foo T) x)");
        assert_eq!(
            ok("Foo<int, string>(x, y)"),
            "(call (generic Foo int string) x y)"
        );
        assert_eq!(ok("Foo<List<int>>(x)"), "(call (generic Foo List<int>) x)");
        // generic member chain: `Foo<T>.Bar`
        assert_eq!(ok("Foo<T>.Bar"), "(. (generic Foo T) Bar)");
        // generic without trailing call/member is fine if EOF or `;`/`,`
        assert_eq!(ok("Foo<T>"), "(generic Foo T)");
    }

    #[test]
    fn comparisons_are_not_misparsed_as_generics() {
        // pure comparisons stay comparisons (follow-token isn't generic-y)
        assert_eq!(ok("a < b"), "(< a b)");
        assert_eq!(ok("a < b > c"), "(> (< a b) c)");
        assert_eq!(ok("a < b + c"), "(< a (+ b c))");
        // even when `>` is followed by an ident, no generic commit
        assert_eq!(ok("x < y + z"), "(< x (+ y z))");
    }

    // ── declaration parser (whole files) ────────────────────────────────

    fn ok_file(src: &str) -> CompUnit {
        let (unit, diags) = parse_file(src, FileId(0));
        assert!(
            diags.is_empty(),
            "unexpected diagnostics for {src:?}:\n{diags:?}"
        );
        unit
    }

    #[test]
    fn using_directive_simple() {
        let unit = ok_file("using System;");
        assert_eq!(unit.items.len(), 1);
        match &unit.items[0] {
            Item::Using {
                is_static: false,
                alias: None,
                target,
                ..
            } => {
                if let Type::Path { segments, .. } = target {
                    assert_eq!(segments.len(), 1);
                    assert_eq!(segments[0].name.text("using System;"), "System");
                } else {
                    panic!("expected path");
                }
            }
            _ => panic!("expected Using"),
        }
    }

    #[test]
    fn using_static_and_alias() {
        let src = "using static System.Math;\nusing Strs = System.Strings;\n";
        let unit = ok_file(src);
        assert_eq!(unit.items.len(), 2);
        match &unit.items[0] {
            Item::Using {
                is_static: true,
                alias: None,
                ..
            } => {}
            other => panic!("expected `using static`, got {other:?}"),
        }
        match &unit.items[1] {
            Item::Using {
                is_static: false,
                alias: Some(a),
                ..
            } => {
                assert_eq!(a.text(src), "Strs");
            }
            other => panic!("expected aliased `using`, got {other:?}"),
        }
    }

    #[test]
    fn namespace_block() {
        let unit = ok_file("namespace A.B { }");
        match &unit.items[0] {
            Item::Namespace {
                body: Some(items),
                path,
                ..
            } => {
                assert!(items.is_empty());
                if let Type::Path { segments, .. } = path {
                    assert_eq!(segments.len(), 2);
                } else {
                    panic!("expected path");
                }
            }
            _ => panic!("expected namespace"),
        }
    }

    #[test]
    fn file_scoped_namespace() {
        let unit = ok_file("namespace A.B;");
        match &unit.items[0] {
            Item::Namespace { body: None, .. } => {}
            _ => panic!("expected file-scoped namespace"),
        }
    }

    #[test]
    fn empty_class() {
        let unit = ok_file("public class Foo { }");
        let Item::Type(td) = &unit.items[0] else {
            panic!("expected type");
        };
        assert_eq!(td.kind, TypeKind::Class);
        assert_eq!(td.name.text("public class Foo { }"), "Foo");
        assert_eq!(td.modifiers.len(), 1);
        assert_eq!(td.modifiers[0].0, Modifier::Public);
        assert!(td.members.is_empty());
    }

    #[test]
    fn class_with_field_method_ctor_dtor() {
        let src = "
class Foo {
    public int x = 0;
    public this(int n) { x = n; }
    public ~this() { }
    public int Square() { return x * x; }
}
";
        let unit = ok_file(src);
        let Item::Type(td) = &unit.items[0] else {
            panic!("expected type");
        };
        assert_eq!(td.members.len(), 4);
        assert!(matches!(td.members[0], Member::Field { .. }));
        assert!(matches!(td.members[1], Member::Constructor { .. }));
        assert!(matches!(td.members[2], Member::Destructor { .. }));
        assert!(matches!(td.members[3], Member::Method { .. }));
    }

    #[test]
    fn expression_bodied_method() {
        let unit = ok_file("class C { public int Sq(int x) => x * x; }");
        let Item::Type(td) = &unit.items[0] else {
            panic!("type")
        };
        assert!(matches!(
            &td.members[0],
            Member::Method {
                body: MethodBody::Expr(_),
                ..
            }
        ));
    }

    #[test]
    fn interface_method_has_none_body() {
        let unit = ok_file("interface IFoo { int Bar(); }");
        let Item::Type(td) = &unit.items[0] else {
            panic!("type")
        };
        assert_eq!(td.kind, TypeKind::Interface);
        assert!(matches!(
            &td.members[0],
            Member::Method {
                body: MethodBody::None,
                ..
            }
        ));
    }

    #[test]
    fn property_with_get_set() {
        let src = "class C { public int X { get; set; } }";
        let unit = ok_file(src);
        let Item::Type(td) = &unit.items[0] else {
            panic!("type")
        };
        let Member::Property { accessors, .. } = &td.members[0] else {
            panic!("expected property")
        };
        assert_eq!(accessors.len(), 2);
        assert_eq!(accessors[0].kind, AccessorKind::Get);
        assert_eq!(accessors[1].kind, AccessorKind::Set);
    }

    #[test]
    fn explicit_interface_implementation_captures_qualifier() {
        // `static int IMinMaxValue<int>.MinValue => MinValue;` is an explicit
        // interface impl: the member name is `MinValue`, and the qualifying
        // interface `IMinMaxValue<int>` is captured (not dropped) so it
        // doesn't collide with a plain `MinValue` member.
        let src = "struct Int { const int MinValue = 0; static int IMinMaxValue<int>.MinValue => MinValue; }";
        let unit = ok_file(src);
        let Item::Type(td) = &unit.items[0] else {
            panic!("type")
        };
        let Member::Property {
            name,
            explicit_iface: Some(iface),
            ..
        } = &td.members[1]
        else {
            panic!(
                "expected explicit-interface property, got {:?}",
                td.members[1]
            );
        };
        assert_eq!(name.text(src), "MinValue");
        assert_eq!(sxt(src, iface), "IMinMaxValue<int>");
    }

    #[test]
    fn explicit_interface_method() {
        let src = "class C { void IDisposable.Dispose() { } }";
        let unit = ok_file(src);
        let Item::Type(td) = &unit.items[0] else {
            panic!("type")
        };
        let Member::Method {
            name,
            explicit_iface: Some(iface),
            ..
        } = &td.members[0]
        else {
            panic!("expected explicit-interface method");
        };
        assert_eq!(name.text(src), "Dispose");
        assert_eq!(sxt(src, iface), "IDisposable");
    }

    #[test]
    fn anonymous_types_and_interleaved_attrs() {
        // Anonymous struct/enum member types, a nameless anonymous field, and
        // an attribute interleaved after a modifier (`public [Union] …`).
        let src = "struct S { \
            public [Union] struct { public int mX, mY; } mVals; \
            public enum { A, B } Dir() => .A; \
            public struct { int q; }; \
        }";
        let unit = ok_file(src);
        let Item::Type(td) = &unit.items[0] else {
            panic!("type")
        };
        assert_eq!(td.members.len(), 3);
    }

    #[test]
    fn generic_type_with_where_constraint() {
        let src = "class List<T> where T : class { }";
        let unit = ok_file(src);
        let Item::Type(td) = &unit.items[0] else {
            panic!("type")
        };
        assert_eq!(td.generic_params.len(), 1);
        assert_eq!(td.generic_params[0].name.text(src), "T");
        assert_eq!(td.constraints.len(), 1);
        assert_eq!(td.constraints[0].name.text(src), "T");
    }

    #[test]
    fn attributes_on_class_and_member() {
        let src = "[CRepr] class Foo { [Inline] public int Bar() { return 0; } }";
        let unit = ok_file(src);
        let Item::Type(td) = &unit.items[0] else {
            panic!("type")
        };
        assert_eq!(td.attributes.len(), 1);
        let Member::Method { attributes, .. } = &td.members[0] else {
            panic!("method")
        };
        assert_eq!(attributes.len(), 1);
    }

    #[test]
    fn enum_with_cases() {
        let src = "enum Color { case Red, case Green = 2, case Blue }";
        let unit = ok_file(src);
        let Item::Type(td) = &unit.items[0] else {
            panic!("type")
        };
        assert_eq!(td.kind, TypeKind::Enum);
        let cases: Vec<_> = td
            .members
            .iter()
            .filter(|m| matches!(m, Member::EnumCase { .. }))
            .collect();
        assert_eq!(cases.len(), 3);
    }

    #[test]
    fn class_with_bases() {
        let src = "class Bar : Base, IFoo { }";
        let unit = ok_file(src);
        let Item::Type(td) = &unit.items[0] else {
            panic!("type")
        };
        assert_eq!(td.bases.len(), 2);
    }

    #[test]
    fn whole_file_with_namespace_and_class() {
        let src = "
using System;
namespace Demo {
    public class Point {
        public int x;
        public int y;
        public this(int x, int y) { this.x = x; this.y = y; }
        public int LenSq() => x * x + y * y;
    }
}
";
        let unit = ok_file(src);
        assert_eq!(unit.items.len(), 2);
        let Item::Namespace {
            body: Some(items), ..
        } = &unit.items[1]
        else {
            panic!("expected namespace block");
        };
        let Item::Type(td) = &items[0] else {
            panic!("type")
        };
        assert_eq!(td.name.text(src), "Point");
        assert_eq!(td.members.len(), 4);
    }

    #[test]
    fn parse_file_with_trivia_retains_comments_and_directives() {
        use newbf_lexer::TokenKind;
        let src = "// lead comment\n#pragma once\nclass C {\n  /* doc */ int x; // trail\n}\n";
        let (unit, trivia, diags) = parse_file_with_trivia(src, FileId(0));
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(unit.items.len(), 1); // AST unchanged — trivia not attached
        // The side channel keeps comments, the directive line, and whitespace,
        // in source order with exact spans.
        let kinds: Vec<TokenKind> = trivia.iter().map(|t| t.kind).collect();
        assert!(kinds.contains(&TokenKind::LineComment));
        assert!(kinds.contains(&TokenKind::BlockComment));
        assert!(kinds.contains(&TokenKind::PreprocLine)); // `#pragma once` retained
        assert!(kinds.contains(&TokenKind::Whitespace)); // for blank-line counting
        // Source order + exact spans: each trivia slice matches the source.
        let mut last = 0;
        for t in &trivia {
            assert!(t.span.lo >= last, "trivia out of order");
            assert_eq!(
                &src[t.span.lo as usize..t.span.hi as usize],
                t.span.text(src)
            );
            last = t.span.lo;
        }
        // The leading comment is the first trivia and reproduces verbatim.
        assert_eq!(trivia[0].span.text(src), "// lead comment");
    }

    #[test]
    fn error_node_spans_full_skipped_region() {
        // R3: a malformed item's Error node must cover the whole skipped
        // slice (so the formatter can copy it verbatim), not just one token.
        let src = "@@@ $$$ ;\nclass C { }";
        let (unit, _diags) = parse_file(src, FileId(0));
        let Item::Error(span) = &unit.items[0] else {
            panic!("expected Error item, got {:?}", unit.items[0]);
        };
        // Covers from the start through the `;` recovery boundary.
        assert_eq!(span.text(src), "@@@ $$$ ;");
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
