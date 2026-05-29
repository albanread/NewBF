//! The NewBF abstract syntax tree (expressions + statements).
//!
//! Every node carries a [`Span`]. NewBF collapses upstream Beef's two
//! layers (raw parse tree → `BfReducer` → AST) into a single AST produced
//! directly by the Pratt parser — a deliberate, documented simplification
//! (we don't need Beef's IDE-incremental raw tree yet). Declarations and
//! the full type grammar arrive in Sprint 04; this is the
//! expression/statement core.

use newbf_lexer::Span;

/// Binary operators, with Beef's exact precedence. Higher binds tighter.
/// Table lifted from `E:\beef\IDEHelper\Compiler\BfAst.cpp`
/// (`BfGetBinaryOpPrecendence`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinOp {
    Mul,          // 14
    Div,          // 14
    Mod,          // 14
    Add,          // 13
    Sub,          // 13
    Shl,          // 12
    Shr,          // 12
    BitAnd,       // 11
    BitXor,       // 10
    BitOr,        // 9
    Range,        // 8  (..<)
    ClosedRange,  // 8  (...)
    Is,           // 7
    As,           // 7
    Compare,      // 6  (<=>)
    Lt,           // 5
    Gt,           // 5
    Le,           // 5
    Ge,           // 5
    Eq,           // 4
    Ne,           // 4
    And,          // 3  (&&)
    Or,           // 2  (||)
    NullCoalesce, // 1  (??)
}

impl BinOp {
    /// Binding power; higher binds tighter. Matches Beef exactly.
    pub fn precedence(self) -> u8 {
        match self {
            BinOp::Mul | BinOp::Div | BinOp::Mod => 14,
            BinOp::Add | BinOp::Sub => 13,
            BinOp::Shl | BinOp::Shr => 12,
            BinOp::BitAnd => 11,
            BinOp::BitXor => 10,
            BinOp::BitOr => 9,
            BinOp::Range | BinOp::ClosedRange => 8,
            BinOp::Is | BinOp::As => 7,
            BinOp::Compare => 6,
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => 5,
            BinOp::Eq | BinOp::Ne => 4,
            BinOp::And => 3,
            BinOp::Or => 2,
            BinOp::NullCoalesce => 1,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Mod => "%",
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
            BinOp::BitAnd => "&",
            BinOp::BitXor => "^",
            BinOp::BitOr => "|",
            BinOp::Range => "..<",
            BinOp::ClosedRange => "...",
            BinOp::Is => "is",
            BinOp::As => "as",
            BinOp::Compare => "<=>",
            BinOp::Lt => "<",
            BinOp::Gt => ">",
            BinOp::Le => "<=",
            BinOp::Ge => ">=",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::And => "&&",
            BinOp::Or => "||",
            BinOp::NullCoalesce => "??",
        }
    }
}

/// Prefix unary operators.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnOp {
    Neg,    // -
    Pos,    // +
    Not,    // !
    BitNot, // ~
    PreInc, // ++
    PreDec, // --
    Deref,  // *
    AddrOf, // &
}

impl UnOp {
    pub fn as_str(self) -> &'static str {
        match self {
            UnOp::Neg => "-",
            UnOp::Pos => "+",
            UnOp::Not => "!",
            UnOp::BitNot => "~",
            UnOp::PreInc => "++",
            UnOp::PreDec => "--",
            UnOp::Deref => "*",
            UnOp::AddrOf => "&",
        }
    }
}

/// Assignment operators (right-associative, lowest precedence).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AssignOp {
    Assign,       // =
    Add,          // +=
    Sub,          // -=
    Mul,          // *=
    Div,          // /=
    Mod,          // %=
    And,          // &=
    Or,           // |=
    Xor,          // ^=
    Shl,          // <<=
    Shr,          // >>=
    NullCoalesce, // ??=
}

impl AssignOp {
    pub fn as_str(self) -> &'static str {
        match self {
            AssignOp::Assign => "=",
            AssignOp::Add => "+=",
            AssignOp::Sub => "-=",
            AssignOp::Mul => "*=",
            AssignOp::Div => "/=",
            AssignOp::Mod => "%=",
            AssignOp::And => "&=",
            AssignOp::Or => "|=",
            AssignOp::Xor => "^=",
            AssignOp::Shl => "<<=",
            AssignOp::Shr => ">>=",
            AssignOp::NullCoalesce => "??=",
        }
    }
}

/// A Beef keyword used in prefix-expression position (`new Foo()`,
/// `scope:alloc T()`, `delete:null x`, `sizeof(T)`, `ref x`, …). The
/// operand is parsed at unary precedence; an optional `:qualifier`
/// captures Beef's allocator/scope qualifiers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrefixKw {
    New,
    Scope,
    Append,
    Delete,
    Box,
    Sizeof,
    Alignof,
    Strideof,
    Typeof,
    Nameof,
    Default,
    Comptype,
    Ref,
    Out,
    Mut,
    In,
    Params,
}

impl PrefixKw {
    pub fn as_str(self) -> &'static str {
        match self {
            PrefixKw::New => "new",
            PrefixKw::Scope => "scope",
            PrefixKw::Append => "append",
            PrefixKw::Delete => "delete",
            PrefixKw::Box => "box",
            PrefixKw::Sizeof => "sizeof",
            PrefixKw::Alignof => "alignof",
            PrefixKw::Strideof => "strideof",
            PrefixKw::Typeof => "typeof",
            PrefixKw::Nameof => "nameof",
            PrefixKw::Default => "default",
            PrefixKw::Comptype => "comptype",
            PrefixKw::Ref => "ref",
            PrefixKw::Out => "out",
            PrefixKw::Mut => "mut",
            PrefixKw::In => "in",
            PrefixKw::Params => "params",
        }
    }
}

/// An expression.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Expr {
    Int(Span),
    Float(Span),
    Char(Span),
    Str(Span),
    Bool(Span),
    Null(Span),
    Ident(Span),
    This(Span),
    Base(Span),
    /// `( inner )`
    Paren {
        span: Span,
        inner: Box<Expr>,
    },
    /// prefix unary: `-x`, `!x`, `++x`, `*p`, `&x`
    Unary {
        span: Span,
        op: UnOp,
        operand: Box<Expr>,
    },
    /// postfix `x++` / `x--`
    PostInc {
        span: Span,
        operand: Box<Expr>,
    },
    PostDec {
        span: Span,
        operand: Box<Expr>,
    },
    /// binary op (incl. `is`/`as`, ranges, `<=>`)
    Binary {
        span: Span,
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// assignment (right-assoc)
    Assign {
        span: Span,
        op: AssignOp,
        target: Box<Expr>,
        value: Box<Expr>,
    },
    /// `cond ? then : else`
    Ternary {
        span: Span,
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
    },
    /// `callee(args...)`
    Call {
        span: Span,
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    /// `base[args...]`
    Index {
        span: Span,
        base: Box<Expr>,
        args: Vec<Expr>,
    },
    /// `base.name` / `base?.name`
    Member {
        span: Span,
        base: Box<Expr>,
        name: Span,
        conditional: bool,
    },
    /// keyword-prefixed expression with optional `:qualifier`
    Prefix {
        span: Span,
        kw: PrefixKw,
        qualifier: Option<Span>,
        operand: Box<Expr>,
    },
    /// recovery placeholder for a malformed expression
    Error(Span),
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Int(s)
            | Expr::Float(s)
            | Expr::Char(s)
            | Expr::Str(s)
            | Expr::Bool(s)
            | Expr::Null(s)
            | Expr::Ident(s)
            | Expr::This(s)
            | Expr::Base(s)
            | Expr::Error(s) => *s,
            Expr::Paren { span, .. }
            | Expr::Unary { span, .. }
            | Expr::PostInc { span, .. }
            | Expr::PostDec { span, .. }
            | Expr::Binary { span, .. }
            | Expr::Assign { span, .. }
            | Expr::Ternary { span, .. }
            | Expr::Call { span, .. }
            | Expr::Index { span, .. }
            | Expr::Member { span, .. }
            | Expr::Prefix { span, .. } => *span,
        }
    }
}

/// A statement.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Stmt {
    /// `{ stmts... }`
    Block { span: Span, stmts: Vec<Stmt> },
    /// `expr ;`
    Expr { span: Span, expr: Expr },
    /// `;`
    Empty(Span),
    /// `var`/`let` local: `var name = init;` (typed locals → Sprint 04)
    Local {
        span: Span,
        is_let: bool,
        name: Span,
        init: Option<Expr>,
    },
    /// `if (cond) then [else els]`
    If {
        span: Span,
        cond: Expr,
        then: Box<Stmt>,
        els: Option<Box<Stmt>>,
    },
    /// `while (cond) body`
    While {
        span: Span,
        cond: Expr,
        body: Box<Stmt>,
    },
    /// `do body while (cond);` / `repeat body while (cond);`
    DoWhile {
        span: Span,
        body: Box<Stmt>,
        cond: Expr,
    },
    /// `for (init; cond; update) body` (C-style)
    For {
        span: Span,
        init: Option<Box<Stmt>>,
        cond: Option<Expr>,
        update: Option<Expr>,
        body: Box<Stmt>,
    },
    /// `for (var name in iter) body` (for-each)
    ForEach {
        span: Span,
        name: Span,
        iter: Expr,
        body: Box<Stmt>,
    },
    /// `return [value];`
    Return { span: Span, value: Option<Expr> },
    /// `break [label];`
    Break { span: Span, label: Option<Span> },
    /// `continue [label];`
    Continue { span: Span, label: Option<Span> },
    /// `defer body`
    Defer { span: Span, body: Box<Stmt> },
    /// recovery placeholder for a malformed statement
    Error(Span),
}

impl Stmt {
    pub fn span(&self) -> Span {
        match self {
            Stmt::Empty(s) | Stmt::Error(s) => *s,
            Stmt::Block { span, .. }
            | Stmt::Expr { span, .. }
            | Stmt::Local { span, .. }
            | Stmt::If { span, .. }
            | Stmt::While { span, .. }
            | Stmt::DoWhile { span, .. }
            | Stmt::For { span, .. }
            | Stmt::ForEach { span, .. }
            | Stmt::Return { span, .. }
            | Stmt::Break { span, .. }
            | Stmt::Continue { span, .. }
            | Stmt::Defer { span, .. } => *span,
        }
    }
}
