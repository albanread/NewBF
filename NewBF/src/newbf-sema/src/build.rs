//! The build pass: an exhaustive walk of every parse tree into the
//! [`DefGraph`]. Nothing is dropped — every namespace, type, member,
//! parameter, attribute, constraint, and using directive is recorded.

use std::collections::HashMap;

use newbf_lexer::FileId;
use newbf_parser::{
    Attribute, CompUnit, Item, Member, MethodBody, Modifier, Param, Type, TypeDecl, WhereClause,
};

use crate::intern::{Interner, Symbol};
use crate::model::*;

/// One parsed source file fed to the analyzer.
pub struct SourceFile<'a> {
    pub file: FileId,
    pub src: &'a str,
    pub unit: &'a CompUnit,
    /// The source file's path / logical name (MS-T7). Used only as
    /// metadata for the heap-allocation site table (`<function> @ file:line`);
    /// `""` is fine for callers that don't have a path (e.g. in-memory tests).
    pub name: &'a str,
}

/// Accumulates the def graph across files. The global namespace is id 0.
pub(crate) struct Builder {
    pub interner: Interner,
    pub namespaces: Vec<NamespaceDef>,
    pub types: Vec<TypeDef>,
    pub members: Vec<MemberDef>,
    pub usings: Vec<UsingDef>,
    pub ns_index: HashMap<String, NsId>,
    /// Current lowering context, so `lower_type` can build anonymous types
    /// (`struct { … }` used as a member type) as real nested `TypeDef`s.
    cur_ns: NsId,
    cur_enclosing: Option<TypeId>,
}

impl Builder {
    pub fn new() -> Self {
        let interner = Interner::new();
        let empty = interner.empty();
        let global = NamespaceDef {
            name: empty,
            full: String::new(),
            parent: None,
            children: Vec::new(),
            types: Vec::new(),
        };
        let mut ns_index = HashMap::new();
        ns_index.insert(String::new(), NsId(0));
        Self {
            interner,
            namespaces: vec![global],
            types: Vec::new(),
            members: Vec::new(),
            usings: Vec::new(),
            ns_index,
            cur_ns: NsId(0),
            cur_enclosing: None,
        }
    }

    pub fn global(&self) -> NsId {
        NsId(0)
    }

    pub fn build_file(&mut self, f: &SourceFile<'_>) {
        let global = self.global();
        self.build_items(&f.unit.items, global, f);
    }

    /// Process a sequence of items under `enclosing_ns`. A file-scoped
    /// `namespace A.B;` mutates the namespace for items that follow it in
    /// the same sequence; a block `namespace A.B { … }` recurses without
    /// affecting the rest.
    fn build_items(&mut self, items: &[Item], enclosing_ns: NsId, f: &SourceFile<'_>) {
        let mut current = enclosing_ns;
        for item in items {
            // Namespace-level lowering context (no enclosing type).
            self.cur_ns = current;
            self.cur_enclosing = None;
            match item {
                Item::Using {
                    span,
                    is_static,
                    access,
                    alias,
                    target,
                    ..
                } => {
                    let target = self.lower_type(target, f);
                    let alias = alias.map(|a| self.interner.intern(a.text(f.src)));
                    self.usings.push(UsingDef {
                        file: f.file,
                        span: *span,
                        is_static: *is_static,
                        access: *access,
                        alias,
                        target,
                        resolution: UsingRes::External,
                    });
                }
                Item::Namespace { path, body, .. } => {
                    let segs = path_segments(path, f.src);
                    let ns = self.intern_namespace(current, &segs);
                    match body {
                        Some(body_items) => self.build_items(body_items, ns, f),
                        None => current = ns, // file-scoped: applies to the rest
                    }
                }
                Item::Type(td) => {
                    self.build_type(td, current, None, f);
                }
                Item::Delegate {
                    span,
                    attributes,
                    modifiers,
                    return_ty,
                    name,
                    generic_params,
                    params,
                } => {
                    let ret = self.lower_type(return_ty, f);
                    let params = self.lower_params(params, f);
                    let attrs = self.lower_attrs(attributes, f);
                    let gps = self.lower_generic_params(generic_params, f);
                    let name_sym = self.interner.intern(name.text(f.src));
                    let tid = TypeId(self.types.len() as u32);
                    self.types.push(TypeDef {
                        name: name_sym,
                        kind: TypeKindD::Delegate,
                        arity: gps.len() as u32,
                        generic_params: gps,
                        modifiers: strip_mods(modifiers),
                        attributes: attrs,
                        bases: Vec::new(),
                        constraints: Vec::new(),
                        parent_ns: current,
                        enclosing_type: None,
                        members: Vec::new(),
                        nested_types: Vec::new(),
                        alias_target: None,
                        delegate_sig: Some(DelegateSig {
                            return_ty: ret,
                            params,
                        }),
                        file: f.file,
                        span: *span,
                    });
                    self.namespaces[current.0 as usize].types.push(tid);
                }
                Item::TypeAlias {
                    span,
                    attributes,
                    modifiers,
                    name,
                    generic_params,
                    target,
                } => {
                    let target = self.lower_type(target, f);
                    let attrs = self.lower_attrs(attributes, f);
                    let gps = self.lower_generic_params(generic_params, f);
                    let name_sym = self.interner.intern(name.text(f.src));
                    let tid = TypeId(self.types.len() as u32);
                    self.types.push(TypeDef {
                        name: name_sym,
                        kind: TypeKindD::Alias,
                        arity: gps.len() as u32,
                        generic_params: gps,
                        modifiers: strip_mods(modifiers),
                        attributes: attrs,
                        bases: Vec::new(),
                        constraints: Vec::new(),
                        parent_ns: current,
                        enclosing_type: None,
                        members: Vec::new(),
                        nested_types: Vec::new(),
                        alias_target: Some(target),
                        delegate_sig: None,
                        file: f.file,
                        span: *span,
                    });
                    self.namespaces[current.0 as usize].types.push(tid);
                }
                Item::Error(_) => {}
            }
        }
    }

    /// Build a type declaration (class/struct/interface/enum/extension) and
    /// all of its members and nested types.
    fn build_type(
        &mut self,
        td: &TypeDecl,
        parent_ns: NsId,
        enclosing_type: Option<TypeId>,
        f: &SourceFile<'_>,
    ) -> TypeId {
        let name_sym = self.interner.intern(td.name.text(f.src));
        let gps = self.lower_generic_params(&td.generic_params, f);
        let attrs = self.lower_attrs(&td.attributes, f);
        let bases: Vec<TypeRef> = td.bases.iter().map(|b| self.lower_type(b, f)).collect();
        let constraints = self.lower_where(&td.constraints, f);

        // Reserve this type's id before walking members so nested types and
        // member owners can reference it.
        let tid = TypeId(self.types.len() as u32);
        self.types.push(TypeDef {
            name: name_sym,
            kind: TypeKindD::from_parser(td.kind),
            arity: gps.len() as u32,
            generic_params: gps,
            modifiers: strip_mods(&td.modifiers),
            attributes: attrs,
            bases,
            constraints,
            parent_ns,
            enclosing_type,
            members: Vec::new(),
            nested_types: Vec::new(),
            alias_target: None,
            delegate_sig: None,
            file: f.file,
            span: td.span,
        });
        // Top-level types belong to their namespace's type list; nested
        // types are linked into the enclosing type's `nested_types` by the
        // caller instead.
        if enclosing_type.is_none() {
            self.namespaces[parent_ns.0 as usize].types.push(tid);
        }

        // Lowering context for anonymous member types (`struct { … } f;`):
        // they become nested `TypeDef`s under this type. Saved/restored so
        // nested and anonymous types don't clobber the caller's context.
        let saved_ns = self.cur_ns;
        let saved_enc = self.cur_enclosing;
        self.cur_ns = parent_ns;
        self.cur_enclosing = Some(tid);

        let mut member_ids = Vec::new();
        let mut nested_ids = Vec::new();
        for m in &td.members {
            match m {
                Member::Field {
                    span,
                    attributes,
                    modifiers,
                    ty,
                    name,
                    init,
                    is_using,
                } => {
                    let ty = self.lower_type(ty, f);
                    let attrs = self.lower_attrs(attributes, f);
                    let name_sym = self.interner.intern(name.text(f.src));
                    member_ids.push(self.push_member(MemberDef::Field(FieldDef {
                        owner: tid,
                        name: name_sym,
                        ty,
                        modifiers: strip_mods(modifiers),
                        attributes: attrs,
                        has_init: init.is_some(),
                        is_using: *is_using,
                        span: *span,
                    })));
                }
                Member::Method {
                    span,
                    attributes,
                    modifiers,
                    return_ty,
                    name,
                    generic_params,
                    params,
                    constraints,
                    body,
                    explicit_iface,
                } => {
                    let ret = self.lower_type(return_ty, f);
                    let attrs = self.lower_attrs(attributes, f);
                    let gps = self.lower_generic_params(generic_params, f);
                    let params = self.lower_params(params, f);
                    let constraints = self.lower_where(constraints, f);
                    let iface = explicit_iface.as_ref().map(|t| self.lower_type(t, f));
                    let name_sym = self.interner.intern(name.text(f.src));
                    member_ids.push(self.push_member(MemberDef::Method(MethodDef {
                        owner: tid,
                        name: name_sym,
                        method_kind: MethodKind::Method,
                        modifiers: strip_mods(modifiers),
                        attributes: attrs,
                        return_ty: Some(ret),
                        generic_params: gps,
                        params,
                        constraints,
                        body: body_kind(body),
                        explicit_iface: iface,
                        span: *span,
                    })));
                }
                // A member mixin (mixins.md §3.1). MX-T1 keeps the *model*
                // identical to the pre-MX-T1 world, where a member mixin was a
                // `Member::Method { return_ty: Type::Error, … }`: register it as
                // a `MethodKind::Method` with an `Error` return type and no
                // explicit interface. Sema's mixin expansion (MX-T3) reads the
                // mixin from the parser AST, not this model entry, so this
                // registration is purely behavior-preserving for `analyze`'s
                // diagnostics. (No mixin expansion happens in MX-T1.)
                Member::Mixin {
                    span,
                    attributes,
                    modifiers,
                    name,
                    generic_params,
                    params,
                    body,
                } => {
                    let ret = self.lower_type(&Type::Error(*name), f);
                    let attrs = self.lower_attrs(attributes, f);
                    let gps = self.lower_generic_params(generic_params, f);
                    let params = self.lower_params(params, f);
                    let name_sym = self.interner.intern(name.text(f.src));
                    member_ids.push(self.push_member(MemberDef::Method(MethodDef {
                        owner: tid,
                        name: name_sym,
                        method_kind: MethodKind::Method,
                        modifiers: strip_mods(modifiers),
                        attributes: attrs,
                        return_ty: Some(ret),
                        generic_params: gps,
                        params,
                        constraints: Vec::new(),
                        body: body_kind(body),
                        explicit_iface: None,
                        span: *span,
                    })));
                }
                Member::Constructor {
                    span,
                    attributes,
                    modifiers,
                    generic_params,
                    params,
                    constraints,
                    body,
                } => {
                    let attrs = self.lower_attrs(attributes, f);
                    let gps = self.lower_generic_params(generic_params, f);
                    let params = self.lower_params(params, f);
                    let constraints = self.lower_where(constraints, f);
                    let name_sym = self.interner.intern("this");
                    member_ids.push(self.push_member(MemberDef::Method(MethodDef {
                        owner: tid,
                        name: name_sym,
                        method_kind: MethodKind::Constructor,
                        modifiers: strip_mods(modifiers),
                        attributes: attrs,
                        return_ty: None,
                        generic_params: gps,
                        params,
                        constraints,
                        body: body_kind(body),
                        explicit_iface: None,
                        span: *span,
                    })));
                }
                Member::Destructor {
                    span,
                    attributes,
                    modifiers,
                    body,
                } => {
                    let attrs = self.lower_attrs(attributes, f);
                    let name_sym = self.interner.intern("~this");
                    member_ids.push(self.push_member(MemberDef::Method(MethodDef {
                        owner: tid,
                        name: name_sym,
                        method_kind: MethodKind::Destructor,
                        modifiers: strip_mods(modifiers),
                        attributes: attrs,
                        return_ty: None,
                        generic_params: Vec::new(),
                        params: Vec::new(),
                        constraints: Vec::new(),
                        body: body_kind(body),
                        explicit_iface: None,
                        span: *span,
                    })));
                }
                Member::Property {
                    span,
                    attributes,
                    modifiers,
                    ty,
                    name,
                    accessors,
                    explicit_iface,
                    ..
                } => {
                    let ty = self.lower_type(ty, f);
                    let attrs = self.lower_attrs(attributes, f);
                    let iface = explicit_iface.as_ref().map(|t| self.lower_type(t, f));
                    let name_sym = self.interner.intern(name.text(f.src));
                    let accessors = accessors
                        .iter()
                        .map(|a| AccessorDef {
                            kind: a.kind,
                            modifiers: strip_mods(&a.modifiers),
                            body: body_kind(&a.body),
                        })
                        .collect();
                    member_ids.push(self.push_member(MemberDef::Property(PropertyDef {
                        owner: tid,
                        name: name_sym,
                        ty,
                        modifiers: strip_mods(modifiers),
                        attributes: attrs,
                        accessors,
                        explicit_iface: iface,
                        span: *span,
                    })));
                }
                Member::EnumCase {
                    span,
                    attributes: _,
                    name,
                    payload,
                    value,
                } => {
                    let payload = self.lower_params(payload, f);
                    let name_sym = self.interner.intern(name.text(f.src));
                    member_ids.push(self.push_member(MemberDef::EnumCase(EnumCaseDef {
                        owner: tid,
                        name: name_sym,
                        payload,
                        has_value: value.is_some(),
                        span: *span,
                    })));
                }
                Member::Nested(nested) => {
                    let nid = self.build_type(nested, parent_ns, Some(tid), f);
                    nested_ids.push(nid);
                }
                Member::TypeAlias {
                    span,
                    attributes,
                    modifiers,
                    name,
                    generic_params,
                    target,
                } => {
                    let target = self.lower_type(target, f);
                    let attrs = self.lower_attrs(attributes, f);
                    let gps = self.lower_generic_params(generic_params, f);
                    let name_sym = self.interner.intern(name.text(f.src));
                    let nid = TypeId(self.types.len() as u32);
                    self.types.push(TypeDef {
                        name: name_sym,
                        kind: TypeKindD::Alias,
                        arity: gps.len() as u32,
                        generic_params: gps,
                        modifiers: strip_mods(modifiers),
                        attributes: attrs,
                        bases: Vec::new(),
                        constraints: Vec::new(),
                        parent_ns,
                        enclosing_type: Some(tid),
                        members: Vec::new(),
                        nested_types: Vec::new(),
                        alias_target: Some(target),
                        delegate_sig: None,
                        file: f.file,
                        span: *span,
                    });
                    nested_ids.push(nid);
                }
                Member::Error(_) => {}
            }
        }

        self.types[tid.0 as usize].members = member_ids;
        self.types[tid.0 as usize].nested_types = nested_ids;
        self.cur_ns = saved_ns;
        self.cur_enclosing = saved_enc;
        tid
    }

    // ── helpers ──────────────────────────────────────────────────────────

    fn push_member(&mut self, m: MemberDef) -> MemberId {
        let id = MemberId(self.members.len() as u32);
        self.members.push(m);
        id
    }

    fn intern_namespace(&mut self, base: NsId, segs: &[String]) -> NsId {
        let mut cur = base;
        for seg in segs {
            cur = self.child_ns(cur, seg);
        }
        cur
    }

    fn child_ns(&mut self, parent: NsId, name: &str) -> NsId {
        let parent_full = self.namespaces[parent.0 as usize].full.clone();
        let full = if parent_full.is_empty() {
            name.to_string()
        } else {
            format!("{parent_full}.{name}")
        };
        if let Some(&id) = self.ns_index.get(&full) {
            return id;
        }
        let name_sym = self.interner.intern(name);
        let id = NsId(self.namespaces.len() as u32);
        self.namespaces.push(NamespaceDef {
            name: name_sym,
            full: full.clone(),
            parent: Some(parent),
            children: Vec::new(),
            types: Vec::new(),
        });
        self.ns_index.insert(full, id);
        self.namespaces[parent.0 as usize].children.push(id);
        id
    }

    fn lower_generic_params(
        &mut self,
        gps: &[newbf_parser::GenericParam],
        f: &SourceFile<'_>,
    ) -> Vec<Symbol> {
        gps.iter()
            .map(|g| self.interner.intern(g.name.text(f.src)))
            .collect()
    }

    fn lower_params(&mut self, params: &[Param], f: &SourceFile<'_>) -> Vec<ParamDef> {
        params
            .iter()
            .map(|p| ParamDef {
                name: p.name.map(|n| self.interner.intern(n.text(f.src))),
                ty: self.lower_type(&p.ty, f),
                modifier: p.modifier.map(|(m, _)| m),
                has_default: p.default.is_some(),
                span: p.span,
            })
            .collect()
    }

    fn lower_attrs(&mut self, attrs: &[Attribute], f: &SourceFile<'_>) -> Vec<AttrRef> {
        attrs
            .iter()
            .map(|a| AttrRef {
                name: self.lower_type(&a.name, f),
                arg_count: a.args.len(),
                span: a.span,
            })
            .collect()
    }

    fn lower_where(&mut self, wcs: &[WhereClause], f: &SourceFile<'_>) -> Vec<WhereRef> {
        wcs.iter()
            .map(|w| WhereRef {
                name: self.interner.intern(w.name.text(f.src)),
                constraints: w
                    .constraints
                    .iter()
                    .map(|c| self.lower_type(c, f))
                    .collect(),
                span: w.span,
            })
            .collect()
    }

    fn lower_type(&mut self, ty: &Type, f: &SourceFile<'_>) -> TypeRef {
        match ty {
            Type::Path { span, segments } => TypeRef::Path {
                span: *span,
                segments: segments
                    .iter()
                    .map(|s| TypeRefSeg {
                        name: self.interner.intern(s.name.text(f.src)),
                        args: s.args.iter().map(|a| self.lower_type(a, f)).collect(),
                    })
                    .collect(),
            },
            Type::Pointer { span, inner } => TypeRef::Pointer {
                span: *span,
                inner: Box::new(self.lower_type(inner, f)),
            },
            Type::Nullable { span, inner } => TypeRef::Nullable {
                span: *span,
                inner: Box::new(self.lower_type(inner, f)),
            },
            Type::Array {
                span, inner, rank, ..
            } => TypeRef::Array {
                span: *span,
                inner: Box::new(self.lower_type(inner, f)),
                rank: *rank,
            },
            Type::Sized { span, inner, .. } => TypeRef::Sized {
                span: *span,
                inner: Box::new(self.lower_type(inner, f)),
            },
            Type::Tuple { span, elems } => TypeRef::Tuple {
                span: *span,
                elems: elems.iter().map(|e| self.lower_type(e, f)).collect(),
            },
            Type::Function {
                span,
                is_delegate,
                return_ty,
                params,
            } => TypeRef::Function {
                span: *span,
                is_delegate: *is_delegate,
                return_ty: Box::new(self.lower_type(return_ty, f)),
                params: params.iter().map(|p| self.lower_type(p, f)).collect(),
            },
            Type::Computed { span, kind, .. } => TypeRef::Computed {
                span: *span,
                kind: ComputedKindD::from_parser(*kind),
            },
            // An anonymous type becomes a real nameless nested `TypeDef` in
            // the graph (under the current type / namespace), so its members
            // are captured, not dropped.
            Type::Anonymous(td) => {
                let tid = self.build_type(td, self.cur_ns, self.cur_enclosing, f);
                TypeRef::Anonymous(tid)
            }
            Type::ConstArg { span, .. } => TypeRef::ConstArg { span: *span },
            Type::Var(s) => TypeRef::Var(*s),
            Type::Error(s) => TypeRef::Error(*s),
        }
    }
}

fn strip_mods(mods: &[(Modifier, newbf_lexer::Span)]) -> Vec<Modifier> {
    mods.iter().map(|(m, _)| *m).collect()
}

fn body_kind(b: &MethodBody) -> BodyKind {
    match b {
        MethodBody::Block(_) => BodyKind::Block,
        MethodBody::Expr(_) => BodyKind::Expr,
        MethodBody::None => BodyKind::None,
    }
}

/// Extract the dotted segment names from a namespace path (a `Type::Path`).
/// Non-path / malformed namespace paths yield no segments (recorded under
/// the enclosing namespace).
fn path_segments(ty: &Type, src: &str) -> Vec<String> {
    match ty {
        Type::Path { segments, .. } => segments
            .iter()
            .map(|s| s.name.text(src).to_string())
            .collect(),
        _ => Vec::new(),
    }
}
