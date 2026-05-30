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

/// Render a parsed compilation unit as an indented AST tree. This is the
/// `dump-ast` report.
pub fn format_ast(src: &str, unit: &CompUnit) -> String {
    let mut p = Printer {
        src,
        out: String::new(),
    };
    p.line(0, "CompUnit");
    for item in &unit.items {
        p.item(item, 1);
    }
    p.out
}

struct Printer<'a> {
    src: &'a str,
    out: String,
}

fn mods_string(mods: &[(Modifier, Span)]) -> String {
    let mut s = String::new();
    for (i, (m, _)) in mods.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(m.as_str());
    }
    s
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
            Expr::Generic { base, args, .. } => {
                self.line(d, "Generic");
                self.labeled_expr(d + 1, "base", base);
                self.line(d + 1, "args:");
                for a in args {
                    self.ty(a, d + 2);
                }
            }
            Expr::Cast { ty, operand, .. } => {
                self.line(d, "Cast");
                self.line(d + 1, "ty:");
                self.ty(ty, d + 2);
                self.labeled_expr(d + 1, "operand", operand);
            }
            Expr::DotIdent { name, .. } => {
                self.line(d, &format!("DotIdent .{}", self.txt(*name)));
            }
            Expr::Tuple { elems, .. } => {
                self.line(d, "Tuple");
                for e in elems {
                    self.expr(e, d + 1);
                }
            }
            Expr::Lambda { body, .. } => {
                self.line(d, "Lambda");
                self.stmt(body, d + 1);
            }
            Expr::Named { name, value, .. } => {
                self.line(d, &format!("Named {}", self.txt(*name)));
                self.expr(value, d + 1);
            }
        }
    }

    fn ty(&mut self, t: &Type, d: usize) {
        match t {
            Type::Var(_) => self.line(d, "TyVar"),
            Type::Error(_) => self.line(d, "TyError"),
            Type::Path { segments, .. } => {
                let mut head = String::from("TyPath");
                for (i, seg) in segments.iter().enumerate() {
                    if i > 0 {
                        head.push('.');
                    } else {
                        head.push(' ');
                    }
                    head.push_str(seg.name.text(self.src));
                }
                self.line(d, &head);
                for seg in segments {
                    if !seg.args.is_empty() {
                        self.line(d + 1, &format!("args of {}:", seg.name.text(self.src)));
                        for a in &seg.args {
                            self.ty(a, d + 2);
                        }
                    }
                }
            }
            Type::Pointer { inner, .. } => {
                self.line(d, "TyPointer");
                self.ty(inner, d + 1);
            }
            Type::Nullable { inner, .. } => {
                self.line(d, "TyNullable");
                self.ty(inner, d + 1);
            }
            Type::Array { inner, rank, .. } => {
                self.line(d, &format!("TyArray rank={rank}"));
                self.ty(inner, d + 1);
            }
            Type::Sized { inner, size, .. } => {
                self.line(d, "TySized");
                self.ty(inner, d + 1);
                self.labeled_expr(d + 1, "size", size);
            }
            Type::Tuple { elems, .. } => {
                self.line(d, "TyTuple");
                for e in elems {
                    self.ty(e, d + 1);
                }
            }
            Type::Function {
                is_delegate,
                return_ty,
                params,
                ..
            } => {
                self.line(
                    d,
                    if *is_delegate {
                        "TyDelegate"
                    } else {
                        "TyFunction"
                    },
                );
                self.line(d + 1, "returns:");
                self.ty(return_ty, d + 2);
                if !params.is_empty() {
                    self.line(d + 1, "params:");
                    for p in params {
                        self.ty(p, d + 2);
                    }
                }
            }
        }
    }

    fn item(&mut self, item: &Item, d: usize) {
        match item {
            Item::Using {
                attributes,
                is_static,
                alias,
                target,
                ..
            } => {
                self.attrs(attributes, d);
                let head = match alias {
                    Some(a) => format!(
                        "Using{} alias={}",
                        if *is_static { " static" } else { "" },
                        self.txt(*a)
                    ),
                    None => format!("Using{}", if *is_static { " static" } else { "" }),
                };
                self.line(d, &head);
                self.ty(target, d + 1);
            }
            Item::Namespace {
                attributes,
                path,
                body,
                ..
            } => {
                self.attrs(attributes, d);
                let head = match body {
                    Some(_) => "Namespace",
                    None => "Namespace (file-scoped)",
                };
                self.line(d, head);
                self.line(d + 1, "path:");
                self.ty(path, d + 2);
                if let Some(items) = body {
                    self.line(d + 1, "items:");
                    for it in items {
                        self.item(it, d + 2);
                    }
                }
            }
            Item::Type(td) => self.type_decl(td, d),
            Item::Delegate {
                attributes,
                modifiers,
                return_ty,
                name,
                generic_params,
                params,
                ..
            } => {
                self.attrs(attributes, d);
                let mods = mods_string(modifiers);
                let head = if mods.is_empty() {
                    format!("Delegate {}", self.txt(*name))
                } else {
                    format!("Delegate [{mods}] {}", self.txt(*name))
                };
                self.line(d, &head);
                if !generic_params.is_empty() {
                    self.line(d + 1, "generics:");
                    for g in generic_params {
                        self.line(d + 2, &format!("GP {}", self.txt(g.name)));
                    }
                }
                self.line(d + 1, "returns:");
                self.ty(return_ty, d + 2);
                if !params.is_empty() {
                    self.line(d + 1, "params:");
                    for p in params {
                        self.param(p, d + 2);
                    }
                }
            }
            Item::TypeAlias {
                attributes,
                modifiers,
                name,
                generic_params,
                target,
                ..
            } => {
                self.attrs(attributes, d);
                let mods = mods_string(modifiers);
                let head = if mods.is_empty() {
                    format!("TypeAlias {}", self.txt(*name))
                } else {
                    format!("TypeAlias [{mods}] {}", self.txt(*name))
                };
                self.line(d, &head);
                if !generic_params.is_empty() {
                    self.line(d + 1, "generics:");
                    for g in generic_params {
                        self.line(d + 2, &format!("GP {}", self.txt(g.name)));
                    }
                }
                self.line(d + 1, "target:");
                self.ty(target, d + 2);
            }
            Item::Error(_) => self.line(d, "Item:Error"),
        }
    }

    fn type_decl(&mut self, td: &TypeDecl, d: usize) {
        self.attrs(&td.attributes, d);
        let mods = mods_string(&td.modifiers);
        let head = format!(
            "{kind}{mods} {name}",
            kind = td.kind.as_str(),
            mods = if mods.is_empty() {
                String::new()
            } else {
                format!(" [{mods}]")
            },
            name = self.txt(td.name),
        );
        self.line(d, &head);
        if !td.generic_params.is_empty() {
            self.line(d + 1, "generics:");
            for g in &td.generic_params {
                self.line(d + 2, &format!("GP {}", self.txt(g.name)));
            }
        }
        if !td.bases.is_empty() {
            self.line(d + 1, "bases:");
            for b in &td.bases {
                self.ty(b, d + 2);
            }
        }
        if !td.constraints.is_empty() {
            self.line(d + 1, "where:");
            for w in &td.constraints {
                self.line(d + 2, &format!("on {}:", self.txt(w.name)));
                for c in &w.constraints {
                    self.ty(c, d + 3);
                }
            }
        }
        if !td.members.is_empty() {
            self.line(d + 1, "members:");
            for m in &td.members {
                self.member(m, d + 2);
            }
        }
    }

    fn attrs(&mut self, attrs: &[Attribute], d: usize) {
        for a in attrs {
            self.line(d, "Attribute");
            self.ty(&a.name, d + 1);
            if !a.args.is_empty() {
                self.line(d + 1, "args:");
                for e in &a.args {
                    self.expr(e, d + 2);
                }
            }
        }
    }

    fn member(&mut self, m: &Member, d: usize) {
        match m {
            Member::Field {
                attributes,
                modifiers,
                ty,
                name,
                init,
                ..
            } => {
                self.attrs(attributes, d);
                let mods = mods_string(modifiers);
                let head = if mods.is_empty() {
                    format!("Field {}", self.txt(*name))
                } else {
                    format!("Field [{mods}] {}", self.txt(*name))
                };
                self.line(d, &head);
                self.line(d + 1, "ty:");
                self.ty(ty, d + 2);
                if let Some(e) = init {
                    self.labeled_expr(d + 1, "init", e);
                }
            }
            Member::Method {
                attributes,
                modifiers,
                return_ty,
                name,
                generic_params,
                params,
                constraints,
                body,
                ..
            } => {
                self.attrs(attributes, d);
                let mods = mods_string(modifiers);
                let head = if mods.is_empty() {
                    format!("Method {}", self.txt(*name))
                } else {
                    format!("Method [{mods}] {}", self.txt(*name))
                };
                self.line(d, &head);
                if !generic_params.is_empty() {
                    self.line(d + 1, "generics:");
                    for g in generic_params {
                        self.line(d + 2, &format!("GP {}", self.txt(g.name)));
                    }
                }
                self.line(d + 1, "returns:");
                self.ty(return_ty, d + 2);
                if !params.is_empty() {
                    self.line(d + 1, "params:");
                    for p in params {
                        self.param(p, d + 2);
                    }
                }
                if !constraints.is_empty() {
                    self.line(d + 1, "where:");
                    for w in constraints {
                        self.line(d + 2, &format!("on {}:", self.txt(w.name)));
                        for c in &w.constraints {
                            self.ty(c, d + 3);
                        }
                    }
                }
                self.method_body(body, d + 1);
            }
            Member::Constructor {
                attributes,
                modifiers,
                params,
                body,
                ..
            } => {
                self.attrs(attributes, d);
                let mods = mods_string(modifiers);
                let head = if mods.is_empty() {
                    "Constructor".to_string()
                } else {
                    format!("Constructor [{mods}]")
                };
                self.line(d, &head);
                if !params.is_empty() {
                    self.line(d + 1, "params:");
                    for p in params {
                        self.param(p, d + 2);
                    }
                }
                self.method_body(body, d + 1);
            }
            Member::Destructor {
                attributes,
                modifiers,
                body,
                ..
            } => {
                self.attrs(attributes, d);
                let mods = mods_string(modifiers);
                let head = if mods.is_empty() {
                    "Destructor".to_string()
                } else {
                    format!("Destructor [{mods}]")
                };
                self.line(d, &head);
                self.method_body(body, d + 1);
            }
            Member::Property {
                attributes,
                modifiers,
                ty,
                name,
                accessors,
                ..
            } => {
                self.attrs(attributes, d);
                let mods = mods_string(modifiers);
                let head = if mods.is_empty() {
                    format!("Property {}", self.txt(*name))
                } else {
                    format!("Property [{mods}] {}", self.txt(*name))
                };
                self.line(d, &head);
                self.line(d + 1, "ty:");
                self.ty(ty, d + 2);
                for a in accessors {
                    self.accessor(a, d + 1);
                }
            }
            Member::EnumCase {
                attributes,
                name,
                payload,
                value,
                ..
            } => {
                self.attrs(attributes, d);
                self.line(d, &format!("EnumCase {}", self.txt(*name)));
                if !payload.is_empty() {
                    self.line(d + 1, "payload:");
                    for p in payload {
                        self.param(p, d + 2);
                    }
                }
                if let Some(e) = value {
                    self.labeled_expr(d + 1, "value", e);
                }
            }
            Member::Nested(td) => self.type_decl(td, d),
            Member::TypeAlias {
                attributes,
                modifiers,
                name,
                generic_params,
                target,
                ..
            } => {
                self.attrs(attributes, d);
                let mods = mods_string(modifiers);
                let head = if mods.is_empty() {
                    format!("TypeAlias {}", self.txt(*name))
                } else {
                    format!("TypeAlias [{mods}] {}", self.txt(*name))
                };
                self.line(d, &head);
                if !generic_params.is_empty() {
                    self.line(d + 1, "generics:");
                    for g in generic_params {
                        self.line(d + 2, &format!("GP {}", self.txt(g.name)));
                    }
                }
                self.line(d + 1, "target:");
                self.ty(target, d + 2);
            }
            Member::Error(_) => self.line(d, "Member:Error"),
        }
    }

    fn param(&mut self, p: &Param, d: usize) {
        self.attrs(&p.attributes, d);
        let m = p.modifier.map_or("", |(m, _)| m.as_str());
        let n = p
            .name
            .map_or("_".to_string(), |s| s.text(self.src).to_string());
        let head = if m.is_empty() {
            format!("Param {n}")
        } else {
            format!("Param {m} {n}")
        };
        self.line(d, &head);
        self.line(d + 1, "ty:");
        self.ty(&p.ty, d + 2);
        if let Some(e) = &p.default {
            self.labeled_expr(d + 1, "default", e);
        }
    }

    fn accessor(&mut self, a: &Accessor, d: usize) {
        self.attrs(&a.attributes, d);
        let mods = mods_string(&a.modifiers);
        let head = if mods.is_empty() {
            format!("Accessor {}", a.kind.as_str())
        } else {
            format!("Accessor [{mods}] {}", a.kind.as_str())
        };
        self.line(d, &head);
        self.method_body(&a.body, d + 1);
    }

    fn method_body(&mut self, body: &MethodBody, d: usize) {
        match body {
            MethodBody::None => self.line(d, "body: (none)"),
            MethodBody::Block(s) => {
                self.line(d, "body:");
                self.stmt(s, d + 1);
            }
            MethodBody::Expr(e) => {
                self.line(d, "body: => expr");
                self.expr(e, d + 1);
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
                is_let,
                ty,
                name,
                init,
                ..
            } => {
                let kw = if *is_let { "let" } else { "var" };
                self.line(d, &format!("Local {kw} {}", self.txt(*name)));
                if let Some(t) = ty {
                    self.line(d + 1, "ty:");
                    self.ty(t, d + 2);
                }
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
            Stmt::LocalFunction {
                return_ty,
                name,
                generic_params,
                params,
                body,
                ..
            } => {
                self.line(d, &format!("LocalFunction {}", self.txt(*name)));
                if !generic_params.is_empty() {
                    self.line(d + 1, "generics:");
                    for g in generic_params {
                        self.line(d + 2, &format!("GP {}", self.txt(g.name)));
                    }
                }
                self.line(d + 1, "returns:");
                self.ty(return_ty, d + 2);
                if !params.is_empty() {
                    self.line(d + 1, "params:");
                    for p in params {
                        self.param(p, d + 2);
                    }
                }
                self.line(d + 1, "body:");
                self.stmt(body, d + 2);
            }
            Stmt::Switch {
                scrutinee, arms, ..
            } => {
                self.line(d, "Switch");
                self.labeled_expr(d + 1, "on", scrutinee);
                for arm in arms {
                    match &arm.pattern {
                        Some(p) => {
                            self.line(d + 1, "Case");
                            self.labeled_expr(d + 2, "pattern", p);
                        }
                        None => self.line(d + 1, "Default"),
                    }
                    for s in &arm.body {
                        self.stmt(s, d + 2);
                    }
                }
            }
        }
    }
}
