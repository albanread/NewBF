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
    Case,         // 7  (Beef: `expr case pattern`)
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
            BinOp::Is | BinOp::As | BinOp::Case => 7,
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
            BinOp::Case => "case",
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
    /// `base<T1, T2, …>` — a generic-instantiated name in expression
    /// position (built when generic-arg disambiguation succeeds; the
    /// following `(args)` then makes it a [`Expr::Call`]).
    Generic {
        span: Span,
        base: Box<Expr>,
        args: Vec<Type>,
    },
    /// `(Type)expr` — C-style cast.
    Cast {
        span: Span,
        ty: Type,
        operand: Box<Expr>,
    },
    /// `sizeof(Type)` — the byte size of a type (a compile-time `int`).
    SizeOf {
        span: Span,
        ty: Type,
    },
    /// `.Variant` — leading-dot enum-case shorthand (the type is inferred
    /// from context).
    DotIdent {
        span: Span,
        name: Span,
    },
    /// `(a, b, …)` — tuple literal.
    Tuple {
        span: Span,
        elems: Vec<Expr>,
    },
    /// A lambda / closure: `x => e`, `(a, b) => e`, `=> { … }`. The
    /// parameters are parsed and discarded for now; `body` is retained.
    Lambda {
        span: Span,
        /// Parameter names (untyped — the types are target-typed from the
        /// `function R(P)` the lambda is assigned to). Empty for `() => …` /
        /// `=> …`, or when params couldn't be captured as plain identifiers.
        params: Vec<Span>,
        body: Box<Stmt>,
    },
    /// A named argument in a call/index/attribute argument list:
    /// `name: value`. Only valid in argument position.
    Named {
        span: Span,
        name: Span,
        value: Box<Expr>,
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
            | Expr::Prefix { span, .. }
            | Expr::Generic { span, .. }
            | Expr::Cast { span, .. }
            | Expr::SizeOf { span, .. }
            | Expr::DotIdent { span, .. }
            | Expr::Tuple { span, .. }
            | Expr::Lambda { span, .. }
            | Expr::Named { span, .. } => *span,
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
    /// Local variable declaration. `ty` is `None` for `var`/`let`
    /// (inferred); `Some(Type)` for typed locals like `int x = 5;`.
    Local {
        span: Span,
        is_let: bool,
        ty: Option<Type>,
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
    /// `switch (e) { (case p: …)* (default: …)? }`
    Switch {
        span: Span,
        scrutinee: Expr,
        arms: Vec<SwitchArm>,
    },
    /// Local function declaration nested inside a method body.
    LocalFunction {
        span: Span,
        return_ty: Type,
        name: Span,
        generic_params: Vec<GenericParam>,
        params: Vec<Param>,
        body: Box<Stmt>,
    },
    /// recovery placeholder for a malformed statement
    Error(Span),
}

/// One arm of a `switch` statement. `pattern` is `None` for `default:`,
/// `Some(expr)` for `case <expr>:` (full pattern syntax is incremental;
/// for now we accept any expression as the pattern).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SwitchArm {
    pub span: Span,
    pub pattern: Option<Expr>,
    pub body: Vec<Stmt>,
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
            | Stmt::Defer { span, .. }
            | Stmt::Switch { span, .. }
            | Stmt::LocalFunction { span, .. } => *span,
        }
    }
}

/// One segment of a qualified type path, optionally with generic args:
/// `A`, `A<T>`, `Outer<T>.Inner<U>` is two segments.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TypeSeg {
    pub name: Span,
    pub args: Vec<Type>,
}

/// A type reference. Function/delegate types and `decltype`/`comptype`
/// are deferred — they're rare in current corpus material.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Type {
    /// `A`, `A.B`, `List<int>`, `Outer<T>.Inner` — a qualified path with
    /// optional generic args on any segment.
    Path { span: Span, segments: Vec<TypeSeg> },
    /// `T*`
    Pointer { span: Span, inner: Box<Type> },
    /// `T?`
    Nullable { span: Span, inner: Box<Type> },
    /// `T[]`, `T[,]` (multi-dim by `rank`)
    Array {
        span: Span,
        inner: Box<Type>,
        rank: u32,
    },
    /// `T[N]` — fixed-size array
    Sized {
        span: Span,
        inner: Box<Type>,
        size: Box<Expr>,
    },
    /// `(A, B, …)` tuple type
    Tuple { span: Span, elems: Vec<Type> },
    /// `function Ret(params)` / `delegate Ret(params)` — a function-pointer
    /// or delegate type. Parameter names (if any) are dropped; only the
    /// parameter types are part of the type.
    Function {
        span: Span,
        is_delegate: bool,
        return_ty: Box<Type>,
        params: Vec<Type>,
    },
    /// A type computed from an expression: `comptype(e)` / `decltype(e)` /
    /// `rettype(e)` / `alloctype(e)`.
    Computed {
        span: Span,
        kind: ComputedKind,
        expr: Box<Expr>,
    },
    /// An anonymous type used in type position: `struct { … }`, `enum { … }`,
    /// `enum : Base { … }`. The full declaration (members, base) is kept.
    Anonymous(Box<TypeDecl>),
    /// A const-value generic argument in a type-argument list: the `16` in
    /// `Foo<16>` / `Foo<const 16>` / `Foo<"+">`. Only valid as a generic arg.
    ConstArg { span: Span, value: Box<Expr> },
    /// `var` used as a type position (inferred local)
    Var(Span),
    /// recovery placeholder for a malformed type
    Error(Span),
}

impl Type {
    pub fn span(&self) -> Span {
        match self {
            Type::Var(s) | Type::Error(s) => *s,
            Type::Path { span, .. }
            | Type::Pointer { span, .. }
            | Type::Nullable { span, .. }
            | Type::Array { span, .. }
            | Type::Sized { span, .. }
            | Type::Tuple { span, .. }
            | Type::Function { span, .. }
            | Type::Computed { span, .. }
            | Type::ConstArg { span, .. } => *span,
            Type::Anonymous(td) => td.span,
        }
    }
}

/// The flavour of a computed type ([`Type::Computed`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ComputedKind {
    Comptype,
    Decltype,
    RetType,
    Alloctype,
}

impl ComputedKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ComputedKind::Comptype => "comptype",
            ComputedKind::Decltype => "decltype",
            ComputedKind::RetType => "rettype",
            ComputedKind::Alloctype => "alloctype",
        }
    }
}

// ── declarations ────────────────────────────────────────────────────────

/// A compilation unit — a single source file.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CompUnit {
    pub span: Span,
    pub items: Vec<Item>,
}

/// A top-level (or namespace-nested) item.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Item {
    /// `using NS;` / `using static NS;` / `using A = NS.Type;` /
    /// `using internal NS;`
    Using {
        span: Span,
        attributes: Vec<Attribute>,
        is_static: bool,
        /// Access modifier on the directive (`using internal NS;`).
        access: Option<Modifier>,
        alias: Option<Span>,
        target: Type,
    },
    /// `namespace A.B { items… }` or file-scoped `namespace A.B;`
    Namespace {
        span: Span,
        attributes: Vec<Attribute>,
        path: Type,
        body: Option<Vec<Item>>,
    },
    /// A type declaration (class/struct/interface/enum/extension).
    Type(TypeDecl),
    /// Top-level delegate declaration: `delegate Return Name<G>(params);`
    Delegate {
        span: Span,
        attributes: Vec<Attribute>,
        modifiers: Vec<(Modifier, Span)>,
        return_ty: Type,
        name: Span,
        generic_params: Vec<GenericParam>,
        params: Vec<Param>,
    },
    /// Top-level / namespace-level type alias: `typealias Name = Type;`
    TypeAlias {
        span: Span,
        attributes: Vec<Attribute>,
        modifiers: Vec<(Modifier, Span)>,
        name: Span,
        generic_params: Vec<GenericParam>,
        target: Type,
    },
    /// Recovery placeholder for a malformed item.
    Error(Span),
}

impl Item {
    pub fn span(&self) -> Span {
        match self {
            Item::Using { span, .. }
            | Item::Namespace { span, .. }
            | Item::Delegate { span, .. }
            | Item::TypeAlias { span, .. } => *span,
            Item::Type(t) => t.span,
            Item::Error(s) => *s,
        }
    }
}

/// `[Attr]`, `[Attr(args)]`, or `[A, B(x)]` (multiple in one bracket).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Attribute {
    pub span: Span,
    pub name: Type,
    pub args: Vec<Expr>,
}

/// A keyword modifier on a type or member.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Modifier {
    Public,
    Private,
    Protected,
    Internal,
    Static,
    Abstract,
    Sealed,
    Virtual,
    Override,
    Extern,
    ReadOnly,
    Const,
    Mut,
    Ref,
    New,
    Inline,
    Mixin,
    Append,
    Concrete,
    Implicit,
    Explicit,
    Volatile,
}

impl Modifier {
    pub fn as_str(self) -> &'static str {
        match self {
            Modifier::Public => "public",
            Modifier::Private => "private",
            Modifier::Protected => "protected",
            Modifier::Internal => "internal",
            Modifier::Static => "static",
            Modifier::Abstract => "abstract",
            Modifier::Sealed => "sealed",
            Modifier::Virtual => "virtual",
            Modifier::Override => "override",
            Modifier::Extern => "extern",
            Modifier::ReadOnly => "readonly",
            Modifier::Const => "const",
            Modifier::Mut => "mut",
            Modifier::Ref => "ref",
            Modifier::New => "new",
            Modifier::Inline => "inline",
            Modifier::Mixin => "mixin",
            Modifier::Append => "append",
            Modifier::Concrete => "concrete",
            Modifier::Implicit => "implicit",
            Modifier::Explicit => "explicit",
            Modifier::Volatile => "volatile",
        }
    }
}

/// A type declaration: class/struct/interface/enum/extension.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TypeDecl {
    pub span: Span,
    pub attributes: Vec<Attribute>,
    pub modifiers: Vec<(Modifier, Span)>,
    pub kind: TypeKind,
    pub name: Span,
    pub generic_params: Vec<GenericParam>,
    pub bases: Vec<Type>,
    pub constraints: Vec<WhereClause>,
    pub members: Vec<Member>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TypeKind {
    Class,
    Struct,
    Interface,
    Enum,
    Extension,
}

impl TypeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TypeKind::Class => "class",
            TypeKind::Struct => "struct",
            TypeKind::Interface => "interface",
            TypeKind::Enum => "enum",
            TypeKind::Extension => "extension",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GenericParam {
    pub span: Span,
    pub name: Span,
}

/// `where T : Base, IFoo, new` — name + comma-separated constraint
/// "types" (parsed as types but may semantically be `class`/`struct`/
/// `new`/`delete` etc. ident-keywords).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WhereClause {
    pub span: Span,
    pub name: Span,
    pub constraints: Vec<Type>,
}

/// A member of a type declaration.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Member {
    /// `Type name [= init];` (single-name fields only for now).
    Field {
        span: Span,
        attributes: Vec<Attribute>,
        modifiers: Vec<(Modifier, Span)>,
        ty: Type,
        name: Span,
        init: Option<Expr>,
        /// `true` for a `using` field (member forwarding):
        /// `using public ClassA mInst;`.
        is_using: bool,
    },
    /// `Type name<G…>(params) where … { body }` — block or expression body.
    Method {
        span: Span,
        attributes: Vec<Attribute>,
        modifiers: Vec<(Modifier, Span)>,
        return_ty: Type,
        name: Span,
        generic_params: Vec<GenericParam>,
        params: Vec<Param>,
        constraints: Vec<WhereClause>,
        body: MethodBody,
        /// `Some` for an explicit interface implementation
        /// (`Ret IFace<Args>.Name(…)`): the qualifying interface type. The
        /// `name` is the final segment; this is the part before it.
        explicit_iface: Option<Type>,
    },
    /// `this[<G…>](params) [where …] { body }` (constructor).
    Constructor {
        span: Span,
        attributes: Vec<Attribute>,
        modifiers: Vec<(Modifier, Span)>,
        generic_params: Vec<GenericParam>,
        params: Vec<Param>,
        constraints: Vec<WhereClause>,
        body: MethodBody,
    },
    /// `~this() { body }` (destructor).
    Destructor {
        span: Span,
        attributes: Vec<Attribute>,
        modifiers: Vec<(Modifier, Span)>,
        body: MethodBody,
    },
    /// `Type name { get; set; }` / `Type name { get => …; set { … } }`.
    Property {
        span: Span,
        attributes: Vec<Attribute>,
        modifiers: Vec<(Modifier, Span)>,
        ty: Type,
        name: Span,
        accessors: Vec<Accessor>,
        /// `Some` for an explicit interface implementation
        /// (`Ret IFace<Args>.Name { … }`): the qualifying interface type.
        explicit_iface: Option<Type>,
    },
    /// An enum payload-bearing case: `case Foo(int x) [= value];`.
    EnumCase {
        span: Span,
        attributes: Vec<Attribute>,
        name: Span,
        payload: Vec<Param>,
        value: Option<Expr>,
    },
    /// A nested type declaration.
    Nested(TypeDecl),
    /// `typealias Name [<G…>] = Type;`
    TypeAlias {
        span: Span,
        attributes: Vec<Attribute>,
        modifiers: Vec<(Modifier, Span)>,
        name: Span,
        generic_params: Vec<GenericParam>,
        target: Type,
    },
    /// Recovery placeholder for a malformed member.
    Error(Span),
}

impl Member {
    pub fn span(&self) -> Span {
        match self {
            Member::Field { span, .. }
            | Member::Method { span, .. }
            | Member::Constructor { span, .. }
            | Member::Destructor { span, .. }
            | Member::Property { span, .. }
            | Member::EnumCase { span, .. }
            | Member::TypeAlias { span, .. } => *span,
            Member::Nested(t) => t.span,
            Member::Error(s) => *s,
        }
    }
}

/// What follows a method/constructor/accessor signature.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MethodBody {
    /// `{ stmts… }`
    Block(Stmt),
    /// `=> expr;`
    Expr(Expr),
    /// `;` (interface signature, abstract, extern, …)
    None,
}

/// A method or constructor parameter.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Param {
    pub span: Span,
    pub attributes: Vec<Attribute>,
    pub modifier: Option<(ParamModifier, Span)>,
    pub ty: Type,
    pub name: Option<Span>, // optional only for `this` / discards in patterns
    pub default: Option<Expr>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ParamModifier {
    Ref,
    Out,
    Mut,
    Params,
    In,
    This, // first-parameter `this` for extension methods
}

impl ParamModifier {
    pub fn as_str(self) -> &'static str {
        match self {
            ParamModifier::Ref => "ref",
            ParamModifier::Out => "out",
            ParamModifier::Mut => "mut",
            ParamModifier::Params => "params",
            ParamModifier::In => "in",
            ParamModifier::This => "this",
        }
    }
}

/// A property accessor: `get`/`set` (with optional modifiers, body).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Accessor {
    pub span: Span,
    pub attributes: Vec<Attribute>,
    pub modifiers: Vec<(Modifier, Span)>,
    pub kind: AccessorKind,
    pub body: MethodBody,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AccessorKind {
    Get,
    Set,
}

impl AccessorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AccessorKind::Get => "get",
            AccessorKind::Set => "set",
        }
    }
}
