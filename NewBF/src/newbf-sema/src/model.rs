//! The NewBF definition graph — the authoritative semantic model.
//!
//! The def builder walks the entire parse tree and records *everything*
//! here: every namespace, type, and member, with full shapes (modifiers,
//! attributes, generic params, bases, constraints, parameter signatures,
//! accessors). Type references are normalized into [`TypeRef`] so that
//! downstream phases read the model and never re-walk the raw AST.
//!
//! Reference for the entity set: `E:\beef\IDEHelper\Compiler\BfDefBuilder.cpp`
//! and `BfSystem.cpp`.

use newbf_lexer::{FileId, Span};
use newbf_parser::{AccessorKind, Modifier, ParamModifier};

use crate::intern::Symbol;

// ── ids ──────────────────────────────────────────────────────────────────

/// Index into [`DefGraph::namespaces`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NsId(pub u32);
/// Index into [`DefGraph::types`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TypeId(pub u32);
/// Index into [`DefGraph::members`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct MemberId(pub u32);

// ── the graph ──────────────────────────────────────────────────────────────

/// The complete definition graph for a program (one or more files).
pub struct DefGraph {
    pub namespaces: Vec<NamespaceDef>,
    pub types: Vec<TypeDef>,
    pub members: Vec<MemberDef>,
    pub usings: Vec<UsingDef>,
    /// The root (unnamed) namespace; everything declared with no enclosing
    /// `namespace` lives here.
    pub global: NsId,
}

impl DefGraph {
    pub fn ns(&self, id: NsId) -> &NamespaceDef {
        &self.namespaces[id.0 as usize]
    }
    pub fn ty(&self, id: TypeId) -> &TypeDef {
        &self.types[id.0 as usize]
    }
    pub fn member(&self, id: MemberId) -> &MemberDef {
        &self.members[id.0 as usize]
    }
}

// ── namespaces ─────────────────────────────────────────────────────────────

/// A namespace node. Namespaces are *open*: `namespace A {…}` in two files
/// (or twice in one) merge into a single node.
pub struct NamespaceDef {
    /// Last path segment; the global namespace's name is the empty symbol.
    pub name: Symbol,
    /// Dotted full path (`""` for the global namespace).
    pub full: String,
    pub parent: Option<NsId>,
    pub children: Vec<NsId>,
    /// Types declared directly in this namespace (not nested types).
    pub types: Vec<TypeId>,
}

// ── types ──────────────────────────────────────────────────────────────────

/// The flavour of a named type-level entity. Extends the parser's
/// [`newbf_parser::TypeKind`] with delegates and type aliases, which are
/// also named entities that participate in name resolution.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TypeKindD {
    Class,
    Struct,
    Interface,
    Enum,
    Extension,
    Delegate,
    Alias,
}

impl TypeKindD {
    pub fn as_str(self) -> &'static str {
        match self {
            TypeKindD::Class => "class",
            TypeKindD::Struct => "struct",
            TypeKindD::Interface => "interface",
            TypeKindD::Enum => "enum",
            TypeKindD::Extension => "extension",
            TypeKindD::Delegate => "delegate",
            TypeKindD::Alias => "typealias",
        }
    }

    pub fn from_parser(k: newbf_parser::TypeKind) -> Self {
        use newbf_parser::TypeKind as K;
        match k {
            K::Class => TypeKindD::Class,
            K::Struct => TypeKindD::Struct,
            K::Interface => TypeKindD::Interface,
            K::Enum => TypeKindD::Enum,
            K::Extension => TypeKindD::Extension,
        }
    }
}

/// A named type-level entity: class/struct/interface/enum/extension, plus
/// delegate and typealias. Captures the full declared shape.
pub struct TypeDef {
    pub name: Symbol,
    pub kind: TypeKindD,
    /// Number of generic parameters (the type's arity).
    pub arity: u32,
    pub generic_params: Vec<Symbol>,
    pub modifiers: Vec<Modifier>,
    pub attributes: Vec<AttrRef>,
    pub bases: Vec<TypeRef>,
    pub constraints: Vec<WhereRef>,
    /// The namespace this type belongs to (the enclosing type's namespace
    /// for nested types).
    pub parent_ns: NsId,
    /// `Some` if this is a nested type.
    pub enclosing_type: Option<TypeId>,
    pub members: Vec<MemberId>,
    pub nested_types: Vec<TypeId>,
    /// For `typealias` — the aliased type.
    pub alias_target: Option<TypeRef>,
    /// For `delegate` — the signature.
    pub delegate_sig: Option<DelegateSig>,
    pub file: FileId,
    pub span: Span,
}

/// The signature of a delegate type.
pub struct DelegateSig {
    pub return_ty: TypeRef,
    pub params: Vec<ParamDef>,
}

// ── members ────────────────────────────────────────────────────────────────

/// A member of a type. Nested types are *not* members here — they live in
/// [`TypeDef::nested_types`] and in the type table — but every value-level
/// member (field/method/ctor/dtor/property/enum-case) is captured.
pub enum MemberDef {
    Field(FieldDef),
    Method(MethodDef),
    Property(PropertyDef),
    EnumCase(EnumCaseDef),
}

impl MemberDef {
    pub fn owner(&self) -> TypeId {
        match self {
            MemberDef::Field(f) => f.owner,
            MemberDef::Method(m) => m.owner,
            MemberDef::Property(p) => p.owner,
            MemberDef::EnumCase(c) => c.owner,
        }
    }

    pub fn name(&self) -> Symbol {
        match self {
            MemberDef::Field(f) => f.name,
            MemberDef::Method(m) => m.name,
            MemberDef::Property(p) => p.name,
            MemberDef::EnumCase(c) => c.name,
        }
    }

    pub fn span(&self) -> Span {
        match self {
            MemberDef::Field(f) => f.span,
            MemberDef::Method(m) => m.span,
            MemberDef::Property(p) => p.span,
            MemberDef::EnumCase(c) => c.span,
        }
    }
}

pub struct FieldDef {
    pub owner: TypeId,
    pub name: Symbol,
    pub ty: TypeRef,
    pub modifiers: Vec<Modifier>,
    pub attributes: Vec<AttrRef>,
    pub has_init: bool,
    /// `true` for a `using` field (member forwarding).
    pub is_using: bool,
    pub span: Span,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MethodKind {
    Method,
    Constructor,
    Destructor,
}

impl MethodKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MethodKind::Method => "method",
            MethodKind::Constructor => "ctor",
            MethodKind::Destructor => "dtor",
        }
    }
}

pub struct MethodDef {
    pub owner: TypeId,
    /// The method name; the canonical `this` / `~this` for ctor/dtor.
    pub name: Symbol,
    pub method_kind: MethodKind,
    pub modifiers: Vec<Modifier>,
    pub attributes: Vec<AttrRef>,
    /// `None` for constructors and destructors.
    pub return_ty: Option<TypeRef>,
    pub generic_params: Vec<Symbol>,
    pub params: Vec<ParamDef>,
    pub constraints: Vec<WhereRef>,
    pub body: BodyKind,
    /// `Some` for an explicit interface implementation — the qualifying
    /// interface. Such a member doesn't collide with a same-named regular
    /// member.
    pub explicit_iface: Option<TypeRef>,
    pub span: Span,
}

pub struct PropertyDef {
    pub owner: TypeId,
    pub name: Symbol,
    pub ty: TypeRef,
    pub modifiers: Vec<Modifier>,
    pub attributes: Vec<AttrRef>,
    pub accessors: Vec<AccessorDef>,
    /// `Some` for an explicit interface implementation (see [`MethodDef`]).
    pub explicit_iface: Option<TypeRef>,
    pub span: Span,
}

pub struct AccessorDef {
    pub kind: AccessorKind,
    pub modifiers: Vec<Modifier>,
    pub body: BodyKind,
}

pub struct EnumCaseDef {
    pub owner: TypeId,
    pub name: Symbol,
    pub payload: Vec<ParamDef>,
    pub has_value: bool,
    pub span: Span,
}

/// A method/ctor/delegate/enum-case parameter signature.
pub struct ParamDef {
    pub name: Option<Symbol>,
    pub ty: TypeRef,
    pub modifier: Option<ParamModifier>,
    pub has_default: bool,
    pub span: Span,
}

/// What follows a signature.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BodyKind {
    Block,
    Expr,
    None,
}

impl BodyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BodyKind::Block => "block",
            BodyKind::Expr => "expr",
            BodyKind::None => "none",
        }
    }
}

// ── attributes / constraints ─────────────────────────────────────────────

/// A captured attribute application: its name (as a type reference) and the
/// number of arguments. Argument expressions are evaluated later (comptime).
pub struct AttrRef {
    pub name: TypeRef,
    pub arg_count: usize,
    pub span: Span,
}

/// A `where T : …` constraint clause.
pub struct WhereRef {
    pub name: Symbol,
    pub constraints: Vec<TypeRef>,
    pub span: Span,
}

// ── using directives ─────────────────────────────────────────────────────

/// How a `using` directive's target resolved against the program.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UsingRes {
    /// Resolved to an in-program namespace.
    Namespace(NsId),
    /// Resolved to an in-program type (`using static T` / alias).
    Type(TypeId),
    /// Not declared in this program (e.g. a corlib namespace/type). Recorded
    /// verbatim, not an error — corlib lands in a later sprint.
    External,
}

pub struct UsingDef {
    pub file: FileId,
    pub span: Span,
    pub is_static: bool,
    /// Access modifier on the directive (`using internal NS;`).
    pub access: Option<Modifier>,
    pub alias: Option<Symbol>,
    pub target: TypeRef,
    pub resolution: UsingRes,
}

// ── normalized type references ─────────────────────────────────────────────

/// A type reference, normalized from the parser's [`newbf_parser::Type`].
/// The full structure (qualified path, per-segment generic args, pointer/
/// nullable/array/tuple suffixes) is preserved so downstream phases have
/// everything they need without touching the AST. Binding a path to a
/// concrete [`TypeId`] is full type resolution (a later sprint); this phase
/// records the shape.
pub enum TypeRef {
    Path {
        span: Span,
        segments: Vec<TypeRefSeg>,
    },
    Pointer {
        span: Span,
        inner: Box<TypeRef>,
    },
    Nullable {
        span: Span,
        inner: Box<TypeRef>,
    },
    Array {
        span: Span,
        inner: Box<TypeRef>,
        rank: u32,
    },
    /// `T[N]` — the size expression is not evaluated at this phase.
    Sized {
        span: Span,
        inner: Box<TypeRef>,
    },
    Tuple {
        span: Span,
        elems: Vec<TypeRef>,
    },
    /// `function Ret(params)` / `delegate Ret(params)`.
    Function {
        span: Span,
        is_delegate: bool,
        return_ty: Box<TypeRef>,
        params: Vec<TypeRef>,
    },
    /// `comptype(e)` / `decltype(e)` / `rettype(e)` / `alloctype(e)` — a type
    /// computed from an expression. The expression is comptime-evaluated in a
    /// later phase; the kind and span are recorded here.
    Computed {
        span: Span,
        kind: ComputedKindD,
    },
    /// An anonymous type used in type position (`struct { … }`) — captured as
    /// a real nameless nested [`TypeDef`] in the graph, referenced by id (so
    /// its members are not lost).
    Anonymous(TypeId),
    /// A const-value generic argument (`Foo<16>`): the value is an expression
    /// (recoverable from `span`), evaluated during monomorphization.
    ConstArg {
        span: Span,
    },
    Var(Span),
    Error(Span),
}

/// The flavour of a [`TypeRef::Computed`] (mirrors the parser's
/// `ComputedKind`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ComputedKindD {
    Comptype,
    Decltype,
    RetType,
    Alloctype,
}

impl ComputedKindD {
    pub fn as_str(self) -> &'static str {
        match self {
            ComputedKindD::Comptype => "comptype",
            ComputedKindD::Decltype => "decltype",
            ComputedKindD::RetType => "rettype",
            ComputedKindD::Alloctype => "alloctype",
        }
    }

    pub fn from_parser(k: newbf_parser::ComputedKind) -> Self {
        use newbf_parser::ComputedKind as K;
        match k {
            K::Comptype => ComputedKindD::Comptype,
            K::Decltype => ComputedKindD::Decltype,
            K::RetType => ComputedKindD::RetType,
            K::Alloctype => ComputedKindD::Alloctype,
        }
    }
}

impl TypeRef {
    pub fn span(&self) -> Span {
        match self {
            TypeRef::Var(s) | TypeRef::Error(s) => *s,
            TypeRef::Path { span, .. }
            | TypeRef::Pointer { span, .. }
            | TypeRef::Nullable { span, .. }
            | TypeRef::Array { span, .. }
            | TypeRef::Sized { span, .. }
            | TypeRef::Tuple { span, .. }
            | TypeRef::Function { span, .. }
            | TypeRef::Computed { span, .. }
            | TypeRef::ConstArg { span } => *span,
            // An anonymous type's span lives on its TypeDef; callers that
            // need it look it up by id.
            TypeRef::Anonymous(_) => Span::new(FileId(0), 0, 0),
        }
    }
}

/// One segment of a [`TypeRef::Path`]: a name and its generic arguments.
pub struct TypeRefSeg {
    pub name: Symbol,
    pub args: Vec<TypeRef>,
}
