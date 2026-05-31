//! AST → typed-SSA-IR lowering for the **primitive kernel**.
//!
//! This is the thin end-to-end slice (SPRINTS.md Sprint 06b): it lowers
//! method bodies over primitive types — integer/float arithmetic, locals,
//! `if`/`while`, `return`, and direct calls — into [`newbf_ir`]. Everything
//! richer (generics, member access, `new`/`scope`, pattern matching, …) is
//! **skipped without panicking** (an unsupported expression yields `undef`,
//! an unsupported statement is a no-op), so the lowering runs over the whole
//! corpus without crashing while producing correct IR for the kernel subset.
//!
//! Bodies live only in the AST at this phase (sema's def graph records
//! shapes, not statement trees — body elaboration is Sprint 12+), so the
//! lowerer walks the AST for bodies and does a small bottom-up type
//! propagation in lieu of a full type checker. Reference for the eventual
//! full lowering: `E:\beef\IDEHelper\Compiler\BfIRCodeGen.cpp`.

use std::collections::HashMap;

use newbf_ir::{
    BinOp as IrBin, BlockId, CastKind, CmpPred, Const, FieldDef, Function, FunctionBuilder, IrType,
    Module, Param as IrParam, StructDef, StructId, Value,
};
use newbf_lexer::FileId;
use newbf_parser::{
    AssignOp, Attribute, BinOp as AstBin, CompUnit, Expr, Item, Member, MethodBody, Modifier,
    Param as AstParam, PrefixKw, Stmt, Type as AstType, TypeDecl, TypeKind, UnOp, parse_file,
};

use crate::Program;
use crate::build::SourceFile;

/// Whether a registered type is a value `struct` (inline aggregate) or a
/// `class` (heap object referenced by pointer).
#[derive(Clone, Copy)]
enum StructKind {
    Value,
    Ref,
}

fn struct_kind(td: &TypeDecl) -> Option<StructKind> {
    match td.kind {
        TypeKind::Struct => Some(StructKind::Value),
        TypeKind::Class => Some(StructKind::Ref),
        _ => None, // interface / enum / extension — not yet
    }
}

/// Type layouts collected before lowering: simple-name → id, the per-id kind
/// (value vs reference), and field lists (mirrored into [`Module::structs`] for
/// the backend). Two passes so a field whose type is another registered type
/// resolves.
#[derive(Default)]
struct StructTable {
    by_name: HashMap<String, StructId>,
    kinds: Vec<StructKind>,
    defs: Vec<StructDef>,
    /// Per-id mangled-name prefix (`"C."` / `"Outer.Inner."`) — matches the
    /// prefix `lower_type` builds, so ctor/dtor symbol names line up.
    prefixes: Vec<String>,
    /// Per-id constructor signatures (`this` + explicit params), one per
    /// distinct arity (overloaded by argument count), and the destructor
    /// symbol, for wiring `new`/`delete`.
    ctors: Vec<Vec<MethodSig>>,
    dtors: Vec<Option<String>>,
    /// Per-id method table (name → signature, this-aware), for resolving
    /// `obj.Method()` and same-type bare calls. First declaration wins.
    /// Per-id method table, keyed by name → all same-name overloads. Resolution
    /// picks among them by argument type (see [`pick_overload`]).
    methods: Vec<HashMap<String, Vec<MethodSig>>>,
    /// Per-id, per-field pointer element type (parallel to `defs[id].fields`):
    /// `Some` for a `T*` field, so `obj.field[i]` knows the element.
    field_elems: Vec<Vec<Option<IrType>>>,
}

impl StructTable {
    fn build(files: &[SourceFile<'_>]) -> Self {
        let mut t = StructTable::default();
        for f in files {
            register_struct_names(&f.unit.items, "", f.src, &mut t);
        }
        for f in files {
            fill_struct_fields(&f.unit.items, f.src, &mut t);
        }
        t
    }

    /// The IR type naming `name`: a value struct → `Struct(id)`, a class →
    /// `Ref(id)`. `None` if `name` isn't a registered type.
    fn ty_of(&self, name: &str) -> Option<IrType> {
        self.by_name
            .get(name)
            .map(|&id| match self.kinds[id.0 as usize] {
                StructKind::Value => IrType::Struct(id),
                StructKind::Ref => IrType::Ref(id),
            })
    }

    /// The constructor of `id` taking `arity` explicit args (its `params`
    /// include the leading `this`, so it has `arity + 1` entries).
    fn ctor_for(&self, id: StructId, arity: usize) -> Option<MethodSig> {
        self.ctors[id.0 as usize]
            .iter()
            .find(|c| c.params.len() == arity + 1)
            .cloned()
    }

    fn dtor_of(&self, id: StructId) -> Option<String> {
        self.dtors[id.0 as usize].clone()
    }
}

fn register_struct_names(items: &[Item], prefix: &str, src: &str, t: &mut StructTable) {
    for item in items {
        match item {
            Item::Namespace {
                body: Some(body), ..
            } => register_struct_names(body, prefix, src, t),
            Item::Type(td) => register_type_struct(td, prefix, src, t),
            _ => {}
        }
    }
}

fn register_type_struct(td: &TypeDecl, prefix: &str, src: &str, t: &mut StructTable) {
    let new_prefix = format!("{prefix}{}.", td.name.text(src));
    // Register non-generic value `struct`s (inline) and `class`es (referenced);
    // generics await monomorphization, interfaces/enums are separate.
    if let Some(kind) = struct_kind(td)
        && td.generic_params.is_empty()
    {
        let name = td.name.text(src).to_string();
        if !t.by_name.contains_key(&name) {
            let id = StructId(t.defs.len() as u32);
            t.defs.push(StructDef {
                name: name.clone(),
                fields: Vec::new(),
            });
            t.kinds.push(kind);
            t.prefixes.push(new_prefix.clone());
            t.ctors.push(Vec::new());
            t.dtors.push(None);
            t.methods.push(HashMap::new());
            t.field_elems.push(Vec::new());
            t.by_name.insert(name, id);
        }
    }
    for m in &td.members {
        if let Member::Nested(n) = m {
            register_type_struct(n, &new_prefix, src, t);
        }
    }
}

fn fill_struct_fields(items: &[Item], src: &str, t: &mut StructTable) {
    for item in items {
        match item {
            Item::Namespace {
                body: Some(body), ..
            } => fill_struct_fields(body, src, t),
            Item::Type(td) => fill_type_struct(td, src, t),
            _ => {}
        }
    }
}

fn fill_type_struct(td: &TypeDecl, src: &str, t: &mut StructTable) {
    let kind = struct_kind(td).filter(|_| td.generic_params.is_empty());
    let id = kind.and_then(|_| t.by_name.get(td.name.text(src)).copied());
    if let (Some(kind), Some(id)) = (kind, id) {
        let mut fields = Vec::new();
        // `elems[i]` is the pointer element type of `fields[i]` (so `obj.f[i]`
        // can index a pointer field); kept in lockstep with `fields`.
        let mut elems: Vec<Option<IrType>> = Vec::new();
        // A class instance carries an object header (ClassVData*) at offset 0;
        // a value struct has none. Real fields follow.
        if matches!(kind, StructKind::Ref) {
            fields.push(FieldDef {
                name: "$header".into(),
                ty: IrType::Ptr,
            });
            elems.push(None);
        }
        for m in &td.members {
            if let Member::Field {
                ty,
                name,
                modifiers,
                ..
            } = m
            {
                // Instance fields only — statics/consts aren't in the layout.
                if modifiers
                    .iter()
                    .any(|(mo, _)| matches!(mo, Modifier::Static | Modifier::Const))
                {
                    continue;
                }
                let fty = lower_ty(ty, src, t);
                elems.push(pointer_elem(ty, src, t));
                fields.push(FieldDef {
                    name: name.text(src).to_string(),
                    ty: fty,
                });
            }
        }
        t.defs[id.0 as usize].fields = fields;
        t.field_elems[id.0 as usize] = elems;

        // Record constructors (one per distinct arity → `$ctorN`), a destructor,
        // and the method table (this-aware) for call resolution. First of each
        // wins; the implicit `this` is a reference to the instance body.
        for m in &td.members {
            match m {
                Member::Constructor { params, .. } => {
                    let arity = params.len();
                    // Overload by arity; first constructor of each arity wins.
                    if t.ctors[id.0 as usize]
                        .iter()
                        .all(|c| c.params.len() != arity + 1)
                    {
                        let mut ps = vec![IrType::Ref(id)];
                        for p in params {
                            ps.push(lower_ty(&p.ty, src, t));
                        }
                        let full_name = format!("{}$ctor{arity}", t.prefixes[id.0 as usize]);
                        t.ctors[id.0 as usize].push(MethodSig {
                            full_name,
                            ret: IrType::Void,
                            params: ps,
                            is_instance: true,
                        });
                    }
                }
                Member::Destructor { .. } => {
                    t.dtors[id.0 as usize] = Some(format!("{}$dtor", t.prefixes[id.0 as usize]));
                }
                Member::Method {
                    return_ty,
                    name,
                    params,
                    body,
                    modifiers,
                    attributes,
                    ..
                } => {
                    let nm = name.text(src).to_string();
                    let explicit: Vec<IrType> =
                        params.iter().map(|p| lower_ty(&p.ty, src, t)).collect();
                    // A body-having method emits `{prefix}{name}` — suffixed by
                    // parameter types when it's a *later* overload of that name
                    // (the first keeps the plain symbol), so each overload's body
                    // is distinct. A body-less `[Intrinsic]`/`[LinkName]` extern
                    // resolves to its symbol; any other body-less member
                    // (abstract/interface) isn't callable and is skipped.
                    let full_name = if matches!(body, MethodBody::None) {
                        match extern_symbol(attributes, src) {
                            Some(sym) => sym,
                            None => continue,
                        }
                    } else {
                        let base = format!("{}{}", t.prefixes[id.0 as usize], nm);
                        if t.methods[id.0 as usize]
                            .get(&nm)
                            .is_some_and(|b| !b.is_empty())
                        {
                            format!("{base}${}", type_codes(&explicit))
                        } else {
                            base
                        }
                    };
                    let is_instance = !modifiers
                        .iter()
                        .any(|(mo, _)| matches!(mo, Modifier::Static));
                    let mut ps = Vec::new();
                    if is_instance {
                        ps.push(IrType::Ref(id));
                    }
                    ps.extend(explicit.iter().copied());
                    let sig = MethodSig {
                        full_name,
                        ret: lower_ty(return_ty, src, t),
                        params: ps,
                        is_instance,
                    };
                    // Keep all same-name overloads; the call site discriminates
                    // by argument type. Skip an exact-signature duplicate (e.g.
                    // a type re-walked) so the bucket stays a true overload set.
                    let bucket = t.methods[id.0 as usize].entry(nm).or_default();
                    if !bucket.iter().any(|s| s.params == sig.params) {
                        bucket.push(sig);
                    }
                }
                _ => {}
            }
        }
    }
    for m in &td.members {
        if let Member::Nested(n) = m {
            fill_type_struct(n, src, t);
        }
    }
}

/// Lower a whole program (the parsed files; the def graph is available for
/// future resolution) into one IR [`Module`].
pub fn lower_program(files: &[SourceFile<'_>], _program: &Program) -> Module {
    // Prepend the corlib prelude as source — parsed, then composed at the AST
    // and lowered once with the user program (STDLIB.md). The prelude units are
    // owned here for the duration of lowering.
    let prelude = newbf_corlib::prelude();
    let prelude_units: Vec<CompUnit> = prelude
        .iter()
        .enumerate()
        .map(|(i, ns)| parse_file(ns.1, FileId(10_000u32 + i as u32)).0)
        .collect();
    let mut all: Vec<SourceFile> = prelude_units
        .iter()
        .enumerate()
        .map(|(i, unit)| SourceFile {
            file: FileId(10_000u32 + i as u32),
            src: prelude[i].1,
            unit,
        })
        .collect();
    for f in files {
        all.push(SourceFile {
            file: f.file,
            src: f.src,
            unit: f.unit,
        });
    }

    let mut m = Module::new("program");
    let structs = StructTable::build(&all);
    m.structs = structs.defs.clone();
    for f in &all {
        lower_items(&f.unit.items, "", f.src, &structs, &mut m);
    }
    m
}

fn lower_items(items: &[Item], prefix: &str, src: &str, structs: &StructTable, m: &mut Module) {
    for item in items {
        match item {
            Item::Namespace {
                body: Some(body), ..
            } => lower_items(body, prefix, src, structs, m),
            Item::Type(td) => lower_type(td, prefix, src, structs, m),
            _ => {} // using / delegate / type-alias / file-scoped ns / error
        }
    }
}

/// A method's lowered signature — its mangled symbol, return type, and
/// parameter types — so a same-type call (`Foo()`) resolves to `{prefix}Foo`
/// with the right signature instead of a defaulted external.
#[derive(Clone)]
struct MethodSig {
    full_name: String,
    ret: IrType,
    /// Parameter types in ABI order. For an instance method this includes the
    /// leading `this` (a `Ref` to the body); a static method has only its
    /// explicit params.
    params: Vec<IrType>,
    is_instance: bool,
}

/// Pick the best-matching overload from `cands` (all sharing a name) for the
/// given argument types — the receiver is *not* among `arg_tys`. An instance
/// candidate matches against its params past the leading `this`; it's eligible
/// only at a member-call site (`members`), since a `this`-less site (a bare or
/// `Type.M` call) has no receiver to pass. Among arity-matching candidates the
/// one with the most exact type matches wins; ties keep the first registered (a
/// coercion bridges any non-exact arg, so arity alone resolves a lone overload).
fn pick_overload<'s>(
    cands: &'s [MethodSig],
    arg_tys: &[IrType],
    members: bool,
) -> Option<&'s MethodSig> {
    let mut best: Option<(&MethodSig, usize)> = None;
    for c in cands {
        if c.is_instance && !members {
            continue;
        }
        let formal: &[IrType] = if c.is_instance {
            &c.params[1..]
        } else {
            &c.params[..]
        };
        if formal.len() != arg_tys.len() {
            continue;
        }
        let score = formal.iter().zip(arg_tys).filter(|(f, a)| f == a).count();
        if best.is_none_or(|(_, bs)| score > bs) {
            best = Some((c, score));
        }
    }
    best.map(|(c, _)| c)
}

/// A compact, deterministic encoding of a parameter-type list, used to suffix
/// the mangled symbol of an overloaded method so each overload is distinct
/// (e.g. `Append(char8)` vs `Append(String)` → `…$i8` vs `…$R3`).
fn type_codes(tys: &[IrType]) -> String {
    let mut s = String::new();
    for t in tys {
        match t {
            IrType::Void => s.push('v'),
            IrType::Bool => s.push('b'),
            IrType::Int { bits, signed } => {
                s.push(if *signed { 'i' } else { 'u' });
                s.push_str(&bits.to_string());
            }
            IrType::Float { bits } => {
                s.push('f');
                s.push_str(&bits.to_string());
            }
            IrType::Ptr => s.push('p'),
            IrType::Ref(id) => {
                s.push('R');
                s.push_str(&id.0.to_string());
            }
            IrType::Struct(id) => {
                s.push('S');
                s.push_str(&id.0.to_string());
            }
        }
    }
    s
}

fn lower_type(td: &TypeDecl, prefix: &str, src: &str, structs: &StructTable, m: &mut Module) {
    let new_prefix = format!("{prefix}{}.", td.name.text(src));
    // The type's own method table (this-aware, built in the pre-pass) resolves
    // same-type bare calls; an empty map covers unregistered types (generics,
    // interfaces).
    let owner_id = structs.by_name.get(td.name.text(src)).copied();
    let empty: HashMap<String, Vec<MethodSig>> = HashMap::new();
    let sigs: &HashMap<String, Vec<MethodSig>> = match owner_id {
        Some(id) => &structs.methods[id.0 as usize],
        None => &empty,
    };
    for member in &td.members {
        match member {
            Member::Method {
                return_ty,
                name,
                params,
                body,
                modifiers,
                ..
            } => {
                // Reuse the table's mangled symbol (it disambiguates overloads)
                // by matching this overload's explicit parameter types; fall back
                // to the plain name for unregistered types (generics/interfaces).
                let explicit: Vec<IrType> = params
                    .iter()
                    .map(|p| lower_ty(&p.ty, src, structs))
                    .collect();
                let full_name = sigs
                    .get(name.text(src))
                    .and_then(|cands| {
                        cands.iter().find(|s| {
                            let formal: &[IrType] = if s.is_instance {
                                &s.params[1..]
                            } else {
                                &s.params[..]
                            };
                            formal == explicit.as_slice()
                        })
                    })
                    .map(|s| s.full_name.clone())
                    .unwrap_or_else(|| format!("{new_prefix}{}", name.text(src)));
                let ret = lower_ty(return_ty, src, structs);
                // Instance methods take a leading `this`; static ones don't.
                let is_static = modifiers
                    .iter()
                    .any(|(mo, _)| matches!(mo, Modifier::Static));
                let this_ty = match owner_id {
                    Some(id) if !is_static => Some(IrType::Ref(id)),
                    _ => None,
                };
                if let Some(func) =
                    lower_method(full_name, ret, params, body, src, sigs, structs, this_ty)
                {
                    m.add_function(func);
                }
            }
            // Constructors/destructors lower like instance methods with a
            // leading `this` (a reference to the body); `new`/`delete` call
            // them by the `$ctor`/`$dtor` mangled names recorded in the table.
            Member::Constructor { params, body, .. } => {
                if let Some(&id) = structs.by_name.get(td.name.text(src)) {
                    // Overloaded by arity: `$ctor{N}` matches what `new` calls.
                    let full_name = format!("{new_prefix}$ctor{}", params.len());
                    let this = Some(IrType::Ref(id));
                    if let Some(func) = lower_method(
                        full_name,
                        IrType::Void,
                        params,
                        body,
                        src,
                        sigs,
                        structs,
                        this,
                    ) {
                        m.add_function(func);
                    }
                }
            }
            Member::Destructor { body, .. } => {
                if let Some(&id) = structs.by_name.get(td.name.text(src)) {
                    let full_name = format!("{new_prefix}$dtor");
                    let this = Some(IrType::Ref(id));
                    if let Some(func) =
                        lower_method(full_name, IrType::Void, &[], body, src, sigs, structs, this)
                    {
                        m.add_function(func);
                    }
                }
            }
            Member::Nested(nested) => lower_type(nested, &new_prefix, src, structs, m),
            _ => {} // fields / properties / enum-cases — later
        }
    }
}

// Threads the per-type method table + program struct table alongside the
// signature; a context struct is a future cleanup, not worth the churn now.
#[allow(clippy::too_many_arguments)]
fn lower_method(
    full_name: String,
    ret: IrType,
    params: &[AstParam],
    body: &MethodBody,
    src: &str,
    methods: &HashMap<String, Vec<MethodSig>>,
    structs: &StructTable,
    this_ty: Option<IrType>,
) -> Option<Function> {
    // Body-less members (interface/abstract/extern) produce no IR yet.
    let body_stmt = match body {
        MethodBody::None => return None,
        other => other,
    };

    // Instance methods / ctors / dtors take a leading `this` (a reference to
    // the instance body); explicit params follow, so their LLVM `Param` index
    // is offset by one.
    let mut ir_params: Vec<IrParam> = Vec::new();
    if let Some(t) = this_ty {
        ir_params.push(IrParam {
            name: Some("this".to_string()),
            ty: t,
        });
    }
    ir_params.extend(params.iter().map(|p| IrParam {
        name: p.name.map(|s| s.text(src).to_string()),
        ty: lower_ty(&p.ty, src, structs),
    }));

    let fb = FunctionBuilder::new(full_name, ir_params, ret);
    let mut lw = Lowerer::new(fb, ret, methods, structs);

    // Spill `this` into a slot at entry, recorded for `Expr::This`.
    let base = if this_ty.is_some() { 1 } else { 0 };
    if let Some(t) = this_ty {
        let slot = lw.fb.alloca(t);
        lw.fb.store(slot.clone(), Value::Param(0));
        lw.this_slot = Some((slot, t));
    }

    // Make parameters addressable: spill each into a stack slot at entry so
    // reads are `load`s and assignments just `store` (LLVM mem2reg cleans up).
    for (i, p) in params.iter().enumerate() {
        if let Some(nm) = &p.name {
            let ty = lower_ty(&p.ty, src, structs);
            let elem = pointer_elem(&p.ty, src, structs);
            let slot = lw.fb.alloca(ty);
            lw.fb.store(slot.clone(), Value::Param((i + base) as u32));
            lw.bind(nm.text(src), slot, ty, elem);
        }
    }

    match body_stmt {
        MethodBody::Block(stmt) => lw.stmt(stmt, src),
        MethodBody::Expr(expr) => {
            let (v, t) = lw.expr(expr, src);
            if lw.ret_ty == IrType::Void {
                lw.ret(None);
            } else {
                let cv = lw.coerce(v, t, lw.ret_ty);
                lw.ret(Some(cv));
            }
        }
        MethodBody::None => unreachable!(),
    }
    lw.finish_ret();
    Some(lw.fb.finish())
}

/// A bound local/parameter: stack slot, slot type, and (for typed pointers)
/// the element type — see [`Lowerer::lookup_elem`].
type Binding = (Value, IrType, Option<IrType>);

struct Lowerer<'a> {
    fb: FunctionBuilder,
    ret_ty: IrType,
    /// Lexical scope stack: name → (stack-slot pointer, slot type, pointer
    /// element type). The third field is `Some` only for typed-pointer
    /// locals/params (`T*`), so `p[i]` knows the element width/stride.
    scopes: Vec<HashMap<String, Binding>>,
    /// Whether the current block already has a terminator (stop emitting).
    terminated: bool,
    /// Sibling methods (same type), for resolving bare-name calls. Each name
    /// maps to its overload set, discriminated by argument type at the call.
    methods: &'a HashMap<String, Vec<MethodSig>>,
    /// Value-struct layouts, for resolving `obj.field` and struct-typed locals.
    structs: &'a StructTable,
    /// Enclosing-loop target stack: `(continue_target, break_target)`. The
    /// innermost loop is last; `break`/`continue` branch to it. (Loop labels
    /// aren't honoured yet — the kernel always targets the innermost loop.)
    loops: Vec<(BlockId, BlockId)>,
    /// The `this` slot in an instance method / ctor / dtor: a stack slot
    /// holding the `Ref` to the instance body. `None` in static contexts.
    this_slot: Option<(Value, IrType)>,
}

impl<'a> Lowerer<'a> {
    fn new(
        fb: FunctionBuilder,
        ret_ty: IrType,
        methods: &'a HashMap<String, Vec<MethodSig>>,
        structs: &'a StructTable,
    ) -> Self {
        Self {
            fb,
            ret_ty,
            scopes: vec![HashMap::new()],
            terminated: false,
            methods,
            structs,
            loops: Vec::new(),
            this_slot: None,
        }
    }

    fn bind(&mut self, name: &str, slot: Value, ty: IrType, elem: Option<IrType>) {
        self.scopes
            .last_mut()
            .unwrap()
            .insert(name.to_string(), (slot, ty, elem));
    }

    fn lookup(&self, name: &str) -> Option<(Value, IrType)> {
        self.scopes
            .iter()
            .rev()
            .find_map(|s| s.get(name))
            .map(|(slot, ty, _)| (slot.clone(), *ty))
    }

    /// The element type recorded for a pointer local/param — for `p[i]`.
    fn lookup_elem(&self, name: &str) -> Option<IrType> {
        self.scopes
            .iter()
            .rev()
            .find_map(|s| s.get(name))
            .and_then(|(_, _, elem)| *elem)
    }

    fn switch(&mut self, b: BlockId) {
        self.fb.switch_to(b);
        self.terminated = false;
    }

    fn ret(&mut self, v: Option<Value>) {
        self.fb.ret(v);
        self.terminated = true;
    }

    /// Ensure the current (fall-through) block has a terminator.
    fn finish_ret(&mut self) {
        if !self.terminated {
            let v = if self.ret_ty == IrType::Void {
                None
            } else {
                Some(zero_of(self.ret_ty))
            };
            self.fb.ret(v);
        }
    }

    // ── statements ────────────────────────────────────────────────────────

    fn stmt(&mut self, s: &Stmt, src: &str) {
        if self.terminated {
            return;
        }
        match s {
            Stmt::Block { stmts, .. } => {
                self.scopes.push(HashMap::new());
                for st in stmts {
                    self.stmt(st, src);
                    if self.terminated {
                        break;
                    }
                }
                self.scopes.pop();
            }
            Stmt::Expr { expr, .. } => {
                self.expr(expr, src);
            }
            Stmt::Empty(_) => {}
            Stmt::Local { ty, name, init, .. } => {
                let (init_val, init_ty) = match init {
                    Some(e) => {
                        let (v, t) = self.expr(e, src);
                        (Some(v), Some(t))
                    }
                    None => (None, None),
                };
                let slot_ty = match ty {
                    Some(t) => lower_ty(t, src, self.structs),
                    None => init_ty.unwrap_or(IrType::I64), // `var`/`let`: infer from init
                };
                let slot = self.fb.alloca(slot_ty);
                // Coerce the initializer to the slot's type before storing —
                // otherwise e.g. `int32 x = 0` stores an i64 literal into a
                // 4-byte slot, overrunning the stack (a store's width follows
                // the value type under opaque pointers).
                if let (Some(v), Some(t)) = (init_val, init_ty) {
                    let v = self.coerce(v, t, slot_ty);
                    self.fb.store(slot.clone(), v);
                }
                // Record the element type for a typed-pointer local (`T* p`).
                let elem = ty.as_ref().and_then(|t| pointer_elem(t, src, self.structs));
                self.bind(name.text(src), slot, slot_ty, elem);
            }
            Stmt::Return { value, .. } => {
                // Coerce the returned value to the function's return type so
                // the IR's `ret` always matches the signature (a void function
                // discards any value).
                let v = if self.ret_ty == IrType::Void {
                    None
                } else {
                    value.as_ref().map(|e| {
                        let (v, t) = self.expr(e, src);
                        self.coerce(v, t, self.ret_ty)
                    })
                };
                self.ret(v);
            }
            Stmt::If {
                cond, then, els, ..
            } => {
                let (cv, ct) = self.expr(cond, src);
                let cond_v = self.coerce_bool(cv, ct);
                let then_b = self.fb.create_block("if.then");
                let join_b = self.fb.create_block("if.join");
                let else_b = if els.is_some() {
                    self.fb.create_block("if.else")
                } else {
                    join_b
                };
                self.fb.cond_br(cond_v, then_b, else_b);
                self.terminated = true;

                self.switch(then_b);
                self.stmt(then, src);
                if !self.terminated {
                    self.fb.br(join_b);
                }
                if let Some(e) = els {
                    self.switch(else_b);
                    self.stmt(e, src);
                    if !self.terminated {
                        self.fb.br(join_b);
                    }
                }
                self.switch(join_b);
            }
            Stmt::While { cond, body, .. } => {
                let head = self.fb.create_block("while.head");
                let body_b = self.fb.create_block("while.body");
                let exit = self.fb.create_block("while.exit");
                self.fb.br(head);
                self.switch(head);
                let (cv, ct) = self.expr(cond, src);
                let cond_v = self.coerce_bool(cv, ct);
                self.fb.cond_br(cond_v, body_b, exit);
                self.terminated = true;
                self.switch(body_b);
                self.loops.push((head, exit)); // continue → re-test the head
                self.stmt(body, src);
                self.loops.pop();
                if !self.terminated {
                    self.fb.br(head);
                }
                self.switch(exit);
            }
            Stmt::DoWhile { body, cond, .. } => {
                // Body runs once before the test. `continue` re-tests the cond.
                let body_b = self.fb.create_block("do.body");
                let cond_b = self.fb.create_block("do.cond");
                let exit = self.fb.create_block("do.exit");
                self.fb.br(body_b);
                self.switch(body_b);
                self.loops.push((cond_b, exit));
                self.stmt(body, src);
                self.loops.pop();
                if !self.terminated {
                    self.fb.br(cond_b);
                }
                self.switch(cond_b);
                let (cv, ct) = self.expr(cond, src);
                let cond_v = self.coerce_bool(cv, ct);
                self.fb.cond_br(cond_v, body_b, exit);
                self.terminated = true;
                self.switch(exit);
            }
            Stmt::For {
                init,
                cond,
                update,
                body,
                ..
            } => {
                // C-style `for (init; cond; update) body`. The loop variable
                // lives in its own scope; `continue` runs `update` then re-tests.
                self.scopes.push(HashMap::new());
                if let Some(init) = init {
                    self.stmt(init, src);
                }
                let head = self.fb.create_block("for.head");
                let body_b = self.fb.create_block("for.body");
                let cont = self.fb.create_block("for.cont");
                let exit = self.fb.create_block("for.exit");
                self.fb.br(head);
                // head: test the cond (an absent cond loops unconditionally).
                self.switch(head);
                let cond_v = match cond {
                    Some(c) => {
                        let (cv, ct) = self.expr(c, src);
                        self.coerce_bool(cv, ct)
                    }
                    None => Value::bool(true),
                };
                self.fb.cond_br(cond_v, body_b, exit);
                self.terminated = true;
                // body → cont
                self.switch(body_b);
                self.loops.push((cont, exit));
                self.stmt(body, src);
                self.loops.pop();
                if !self.terminated {
                    self.fb.br(cont);
                }
                // cont: run the update, then back to the head.
                self.switch(cont);
                if let Some(u) = update {
                    self.expr(u, src);
                }
                self.fb.br(head);
                self.switch(exit);
                self.scopes.pop();
            }
            Stmt::Break { .. } => {
                if let Some(&(_, brk)) = self.loops.last() {
                    self.fb.br(brk);
                    self.terminated = true;
                }
            }
            Stmt::Continue { .. } => {
                if let Some(&(cont, _)) = self.loops.last() {
                    self.fb.br(cont);
                    self.terminated = true;
                }
            }
            Stmt::Switch {
                scrutinee, arms, ..
            } => {
                // Value switch: evaluate the scrutinee once, then a chain of
                // `==` tests. Beef arms don't fall through, so each body
                // branches to a single exit. A `break` inside an arm exits the
                // switch; `continue` still targets the enclosing loop.
                let (sv, st) = self.expr(scrutinee, src);
                let exit = self.fb.create_block("switch.exit");
                let body_blocks: Vec<BlockId> = (0..arms.len())
                    .map(|i| self.fb.create_block(format!("switch.case{i}")))
                    .collect();
                // The `default` arm (a patternless arm) is the chain's final
                // else; with no default, a miss falls straight to the exit.
                let default_target = arms
                    .iter()
                    .position(|a| a.pattern.is_none())
                    .map(|i| body_blocks[i])
                    .unwrap_or(exit);
                let cont = self.loops.last().map(|&(c, _)| c).unwrap_or(exit);

                let case_idxs: Vec<usize> = (0..arms.len())
                    .filter(|&i| arms[i].pattern.is_some())
                    .collect();
                if case_idxs.is_empty() {
                    self.fb.br(default_target);
                    self.terminated = true;
                }
                for (chain_i, &arm_i) in case_idxs.iter().enumerate() {
                    let pat = arms[arm_i].pattern.as_ref().unwrap();
                    let (pv, pt) = self.expr(pat, src);
                    let ct = common_type(st, pt);
                    let l = self.coerce(sv.clone(), st, ct);
                    let r = self.coerce(pv, pt, ct);
                    let eq = self.fb.cmp(CmpPred::Eq, l, r);
                    let last = chain_i + 1 == case_idxs.len();
                    let next = if last {
                        default_target
                    } else {
                        self.fb.create_block("switch.test")
                    };
                    self.fb.cond_br(eq, body_blocks[arm_i], next);
                    self.terminated = true;
                    if !last {
                        self.switch(next);
                    }
                }
                self.loops.push((cont, exit));
                for i in 0..arms.len() {
                    self.switch(body_blocks[i]);
                    for s in &arms[i].body {
                        self.stmt(s, src);
                        if self.terminated {
                            break;
                        }
                    }
                    if !self.terminated {
                        self.fb.br(exit);
                    }
                }
                self.loops.pop();
                self.switch(exit);
            }
            // foreach (needs iterators), defer (scope-exit ordering),
            // local-function — not in the kernel yet. Skipped (no IR emitted),
            // never panicking.
            _ => {}
        }
    }

    // ── expressions ───────────────────────────────────────────────────────

    fn expr(&mut self, e: &Expr, src: &str) -> (Value, IrType) {
        match e {
            Expr::Int(s) => (Value::int(parse_int(s.text(src)), IrType::I64), IrType::I64),
            Expr::Float(s) => (
                Value::float(parse_float(s.text(src)), IrType::F64),
                IrType::F64,
            ),
            Expr::Bool(s) => (Value::bool(s.text(src) == "true"), IrType::Bool),
            Expr::Char(s) => (
                Value::int(decode_char_literal(s.text(src)), IrType::U8),
                IrType::U8,
            ),
            Expr::Null(_) => (Value::Const(Const::Null), IrType::Ptr),
            Expr::Str(s) => (Value::str(decode_string_literal(s.text(src))), IrType::Ptr),
            Expr::Paren { inner, .. } => self.expr(inner, src),
            Expr::Ident(s) => match self.lookup(s.text(src)) {
                Some((slot, ty)) => (self.fb.load(slot, ty), ty),
                None => (undef(IrType::I64), IrType::I64),
            },
            Expr::This(_) => match self.this_slot.clone() {
                Some((slot, ty)) => (self.fb.load(slot, ty), ty),
                None => (undef(IrType::Ptr), IrType::Ptr),
            },
            Expr::Unary { op, operand, .. } => self.unary(*op, operand, src),
            Expr::PostInc { operand, .. } => self.incdec(operand, 1, false, src),
            Expr::PostDec { operand, .. } => self.incdec(operand, -1, false, src),
            Expr::Binary { op, lhs, rhs, .. } => self.binary(*op, lhs, rhs, src),
            Expr::Assign {
                op, target, value, ..
            } => self.assign(*op, target, value, src),
            Expr::Call { callee, args, .. } => {
                // Method call on a receiver: `obj.Method(args)` / `this.M(args)`.
                if let Expr::Member { base, name, .. } = &**callee {
                    return self.lower_method_call(base, name.text(src), args, src);
                }
                let arg_vals: Vec<(Value, IrType)> =
                    args.iter().map(|a| self.expr(a, src)).collect();
                if let Expr::Ident(s) = &**callee {
                    let name = s.text(src);
                    // Resolve among the same-type overloads by argument type
                    // (`this`-less candidates — statics and free fns; an instance
                    // method's leading `this` won't match here, so a bare call
                    // reaches it only via `this.M(..)`). Coercion then makes each
                    // arg exactly the param type. No match → a defaulted external.
                    let arg_tys: Vec<IrType> = arg_vals.iter().map(|(_, t)| *t).collect();
                    let resolved = self
                        .methods
                        .get(name)
                        .and_then(|cands| pick_overload(cands, &arg_tys, false))
                        .cloned();
                    if let Some(sig) = resolved {
                        // Same-type call (incl. recursion).
                        let coerced: Vec<Value> = arg_vals
                            .into_iter()
                            .enumerate()
                            .map(|(i, (v, t))| self.coerce(v, t, sig.params[i]))
                            .collect();
                        let r = self.fb.call(sig.full_name, coerced, sig.ret);
                        (r, sig.ret)
                    } else {
                        // Unresolved / arity-mismatched bare name — an external
                        // (Win32/CRT) call; default the result to i64.
                        let plain: Vec<Value> = arg_vals.into_iter().map(|(v, _)| v).collect();
                        let r = self.fb.call(name, plain, IrType::I64);
                        (r, IrType::I64)
                    }
                } else {
                    (undef(IrType::I64), IrType::I64)
                }
            }
            // Heap allocation / free.
            Expr::Prefix {
                kw: PrefixKw::New,
                operand,
                ..
            } => self.lower_new(operand, src),
            Expr::Prefix {
                kw: PrefixKw::Delete,
                operand,
                ..
            } => self.lower_delete(operand, src),
            // Member read (`obj.field` / `ref.field`): load the resolved field;
            // degrade if the base isn't a known struct/reference place.
            Expr::Member { .. } => match self.lvalue(e, src) {
                Some((ptr, ty)) => (self.fb.load(ptr, ty), ty),
                None => (undef(IrType::I64), IrType::I64),
            },
            // Index read (`p[i]`): load the element at the computed address.
            Expr::Index { .. } => match self.lvalue(e, src) {
                Some((ptr, ty)) => (self.fb.load(ptr, ty), ty),
                None => (undef(IrType::I64), IrType::I64),
            },
            // this/base, index, generic, cast, tuple, lambda, new/scope
            // prefixes, .Variant, named args — not lowered yet.
            _ => (undef(IrType::I64), IrType::I64),
        }
    }

    fn unary(&mut self, op: UnOp, operand: &Expr, src: &str) -> (Value, IrType) {
        // Prefix `++`/`--` mutate an lvalue, so resolve the slot directly
        // rather than evaluating the operand to a loaded rvalue first.
        match op {
            UnOp::PreInc => return self.incdec(operand, 1, true, src),
            UnOp::PreDec => return self.incdec(operand, -1, true, src),
            _ => {}
        }
        let (v, t) = self.expr(operand, src);
        match op {
            UnOp::Pos => (v, t),
            // Negation and bit-complement are numeric (LLVM has no pointer
            // arithmetic op). `is_int()` includes `bool`. On a pointer /
            // aggregate / void there is no meaningful kernel lowering, so
            // yield a typed `undef` instead of an integer op that lies about
            // its result type.
            UnOp::Neg if t.is_float() => (self.fb.bin(IrBin::FSub, zero_of(t), v, t), t),
            UnOp::Neg if t.is_int() => (self.fb.bin(IrBin::Sub, zero_of(t), v, t), t),
            UnOp::BitNot if t.is_int() => (self.fb.bin(IrBin::Xor, v, Value::int(-1, t), t), t),
            UnOp::Neg | UnOp::BitNot => (undef(t), t),
            UnOp::Not => (self.fb.cmp(CmpPred::Eq, v, zero_of(t)), IrType::Bool),
            // `*deref` / `&addr-of` need pointer lvalues — later. (PreInc/
            // PreDec are handled above; PostInc/PostDec are separate exprs.)
            _ => (undef(t), t),
        }
    }

    fn binary(&mut self, op: AstBin, lhs: &Expr, rhs: &Expr, src: &str) -> (Value, IrType) {
        let (l, lt) = self.expr(lhs, src);
        let (r, rt) = self.expr(rhs, src);
        match op {
            AstBin::Add
            | AstBin::Sub
            | AstBin::Mul
            | AstBin::Div
            | AstBin::Mod
            | AstBin::BitAnd
            | AstBin::BitOr
            | AstBin::BitXor => {
                // Promote both operands to a common type so the IR op has
                // matching, well-typed operands (LLVM requires it).
                let ct = common_type(lt, rt);
                let l = self.coerce(l, lt, ct);
                let r = self.coerce(r, rt, ct);
                (self.arith(op, l, r, ct), ct)
            }
            // Shifts keep the shifted value's type; only the shift amount is
            // coerced to match it.
            AstBin::Shl | AstBin::Shr => {
                let st = if lt == IrType::Ptr { IrType::I64 } else { lt };
                let l = self.coerce(l, lt, st);
                let r = self.coerce(r, rt, st);
                (self.arith(op, l, r, st), st)
            }
            // `&&`/`||` lowered as bitwise on i1 for the kernel (no
            // short-circuit side effects yet); both sides become `i1`.
            AstBin::And => {
                let l = self.coerce_bool(l, lt);
                let r = self.coerce_bool(r, rt);
                (self.fb.bin(IrBin::And, l, r, IrType::Bool), IrType::Bool)
            }
            AstBin::Or => {
                let l = self.coerce_bool(l, lt);
                let r = self.coerce_bool(r, rt);
                (self.fb.bin(IrBin::Or, l, r, IrType::Bool), IrType::Bool)
            }
            AstBin::Lt | AstBin::Le | AstBin::Gt | AstBin::Ge | AstBin::Eq | AstBin::Ne => {
                let ct = common_type(lt, rt);
                let l = self.coerce(l, lt, ct);
                let r = self.coerce(r, rt, ct);
                (self.fb.cmp(cmp_pred(op, ct), l, r), IrType::Bool)
            }
            // ranges, is/as/case, <=>, ?? — later.
            _ => (undef(lt), lt),
        }
    }

    /// Emit a value-producing arithmetic/bitwise op of result type `ty`.
    fn arith(&mut self, op: AstBin, l: Value, r: Value, ty: IrType) -> Value {
        let f = ty.is_float();
        let s = ty.is_signed();
        let irop = match op {
            AstBin::Add => with_float(f, IrBin::Add, IrBin::FAdd),
            AstBin::Sub => with_float(f, IrBin::Sub, IrBin::FSub),
            AstBin::Mul => with_float(f, IrBin::Mul, IrBin::FMul),
            AstBin::Div => {
                if f {
                    IrBin::FDiv
                } else if s {
                    IrBin::SDiv
                } else {
                    IrBin::UDiv
                }
            }
            AstBin::Mod => {
                if f {
                    IrBin::FRem
                } else if s {
                    IrBin::SRem
                } else {
                    IrBin::URem
                }
            }
            AstBin::BitAnd => IrBin::And,
            AstBin::BitOr => IrBin::Or,
            AstBin::BitXor => IrBin::Xor,
            AstBin::Shl => IrBin::Shl,
            AstBin::Shr => {
                if s {
                    IrBin::AShr
                } else {
                    IrBin::LShr
                }
            }
            _ => IrBin::Add, // unreachable for callers
        };
        self.fb.bin(irop, l, r, ty)
    }

    /// Resolve an lvalue expression to `(pointer, pointee-type)`: a local's
    /// stack slot, or a struct field address via `fieldaddr`. `None` for
    /// anything not (yet) a storable place — callers degrade.
    fn lvalue(&mut self, e: &Expr, src: &str) -> Option<(Value, IrType)> {
        match e {
            Expr::Paren { inner, .. } => self.lvalue(inner, src),
            Expr::Ident(s) => self.lookup(s.text(src)),
            Expr::This(_) => self.this_slot.clone(),
            Expr::Member { base, name, .. } => {
                let (body_ptr, id) = self.struct_base(base, src)?;
                let fname = name.text(src);
                // Copy index + field type out, ending the `defs` borrow before
                // the `&mut self` field-address emit.
                let (idx, fty) = {
                    let def = &self.structs.defs[id.0 as usize];
                    let i = def.fields.iter().position(|f| f.name == fname)?;
                    (i as u32, def.fields[i].ty)
                };
                Some((self.fb.field_addr(body_ptr, id, idx), fty))
            }
            // `p[i]` over a typed pointer: address = base + index·sizeof(elem).
            Expr::Index { base, args, .. } => {
                let elem = self.ptr_elem_of(base, src)?;
                let idx_expr = args.first()?;
                let (basev, _bt) = self.expr(base, src);
                let (iv, it) = self.expr(idx_expr, src);
                let iv = self.coerce(iv, it, IrType::I64);
                Some((self.fb.elem_addr(basev, elem, iv), elem))
            }
            _ => None,
        }
    }

    /// The element type of a typed-pointer base expression (`T* p` → `T`), for
    /// indexing. Resolves pointer locals/params (and through parens) today.
    fn ptr_elem_of(&self, e: &Expr, src: &str) -> Option<IrType> {
        match e {
            Expr::Paren { inner, .. } => self.ptr_elem_of(inner, src),
            Expr::Ident(s) => self.lookup_elem(s.text(src)),
            // A pointer *field*: `obj.buf[i]` / `this.buf[i]`.
            Expr::Member { base, name, .. } => {
                let owner = self.expr_struct_id(base, src)?;
                let fname = name.text(src);
                let fidx = self.structs.defs[owner.0 as usize]
                    .fields
                    .iter()
                    .position(|f| f.name == fname)?;
                self.structs.field_elems[owner.0 as usize][fidx]
            }
            _ => None,
        }
    }

    /// The struct/reference type id of an expression, by *static* type — emits
    /// no code. Resolves `this`, locals/params, and (nested) fields, so member
    /// and index resolution can find the owning layout.
    fn expr_struct_id(&self, e: &Expr, src: &str) -> Option<StructId> {
        let ty = match e {
            Expr::Paren { inner, .. } => return self.expr_struct_id(inner, src),
            Expr::This(_) => self.this_slot.as_ref().map(|(_, t)| *t)?,
            Expr::Ident(s) => self.lookup(s.text(src)).map(|(_, t)| t)?,
            Expr::Member { base, name, .. } => {
                let owner = self.expr_struct_id(base, src)?;
                let fname = name.text(src);
                let fidx = self.structs.defs[owner.0 as usize]
                    .fields
                    .iter()
                    .position(|f| f.name == fname)?;
                self.structs.defs[owner.0 as usize].fields[fidx].ty
            }
            _ => return None,
        };
        match ty {
            IrType::Struct(id) | IrType::Ref(id) => Some(id),
            _ => None,
        }
    }

    /// Resolve the base of a member access to `(body_pointer, struct_id)`.
    /// A value struct's place *is* its body; a class reference's place holds a
    /// pointer, so load it to reach the heap body.
    fn struct_base(&mut self, base: &Expr, src: &str) -> Option<(Value, StructId)> {
        match self.lvalue(base, src) {
            Some((place, IrType::Struct(id))) => Some((place, id)),
            Some((place, IrType::Ref(id))) => {
                let body = self.fb.load(place, IrType::Ptr);
                Some((body, id))
            }
            Some(_) => None,
            None => {
                // Non-lvalue base (e.g. `new C().x`): a reference rvalue is
                // itself the body pointer.
                let (v, t) = self.expr(base, src);
                if let IrType::Ref(id) = t {
                    Some((v, id))
                } else {
                    None
                }
            }
        }
    }

    /// `new C(...)` → `malloc(sizeof C)` + a zeroed object header, yielding a
    /// `Ref(C)`. The constructor call is deferred (a later sprint); fields are
    /// left at their freshly-allocated (indeterminate) values for now.
    fn lower_new(&mut self, operand: &Expr, src: &str) -> (Value, IrType) {
        if let Some(name) = ctor_class_name(operand, src)
            && let Some(IrType::Ref(id)) = self.structs.ty_of(name)
        {
            let size = self.fb.size_of(id);
            let p = self.fb.call("malloc", vec![size], IrType::Ref(id));
            // Object header (ClassVData*) at offset 0 — null until vtables.
            let hdr = self.fb.field_addr(p.clone(), id, 0);
            self.fb.store(hdr, Value::Const(Const::Null));
            // Run the constructor overload matching the argument count; coercion
            // makes each arg its declared param type.
            let args = ctor_args(operand);
            if let Some(ctor) = self.structs.ctor_for(id, args.len()) {
                let mut call_args = vec![p.clone()];
                for (i, a) in args.iter().enumerate() {
                    let (v, t) = self.expr(a, src);
                    let pt = ctor.params.get(i + 1).copied().unwrap_or(t);
                    call_args.push(self.coerce(v, t, pt));
                }
                self.fb.call(ctor.full_name, call_args, IrType::Void);
            }
            return (p, IrType::Ref(id));
        }
        (undef(IrType::Ptr), IrType::Ptr)
    }

    /// Construct a corlib `String` from a C-string pointer (the target-typed
    /// literal path): `malloc` the body, zero the header, run `String(char8*)`.
    fn construct_string(&mut self, cstr: Value) -> Value {
        let Some(id) = self.structs.by_name.get("String").copied() else {
            return cstr;
        };
        let size = self.fb.size_of(id);
        let p = self.fb.call("malloc", vec![size], IrType::Ref(id));
        let hdr = self.fb.field_addr(p.clone(), id, 0);
        self.fb.store(hdr, Value::Const(Const::Null));
        if let Some(ctor) = self.structs.ctor_for(id, 1) {
            self.fb
                .call(ctor.full_name, vec![p.clone(), cstr], IrType::Void);
        }
        p
    }

    /// `delete x` → `free(x)`. The destructor is deferred (a later sprint).
    fn lower_delete(&mut self, operand: &Expr, src: &str) -> (Value, IrType) {
        let (v, t) = self.expr(operand, src);
        // Run the destructor before freeing, if the type has one.
        if let IrType::Ref(id) = t
            && let Some(dtor) = self.structs.dtor_of(id)
        {
            self.fb.call(dtor, vec![v.clone()], IrType::Void);
        }
        self.fb.call("free", vec![v], IrType::Void);
        (Value::Const(Const::Undef(IrType::Void)), IrType::Void)
    }

    /// Lower a method call `receiver.Method(args)`. Resolves the receiver's
    /// type, looks up the method (this-aware), and emits a direct call — passing
    /// the receiver as `this` for an instance method. Degrades (evaluating args
    /// for their effects) when the method can't be resolved.
    fn lower_method_call(
        &mut self,
        base: &Expr,
        mname: &str,
        args: &[Expr],
        src: &str,
    ) -> (Value, IrType) {
        // Evaluate arguments once: their types drive overload selection, their
        // values feed whichever site resolves. (Static and instance sites are
        // mutually exclusive — a type name isn't a receiver and vice versa — so
        // there's no double-emit.)
        let arg_vals: Vec<(Value, IrType)> = args.iter().map(|a| self.expr(a, src)).collect();
        let arg_tys: Vec<IrType> = arg_vals.iter().map(|(_, t)| *t).collect();

        // Qualified static call `Type.Method(args)`: the base names a registered
        // type (not a local). `members: false` keeps only static overloads.
        if let Expr::Ident(s) = base {
            let name = s.text(src);
            if self.lookup(name).is_none()
                && let Some(&id) = self.structs.by_name.get(name)
                && let Some(sig) = self.structs.methods[id.0 as usize]
                    .get(mname)
                    .and_then(|cands| pick_overload(cands, &arg_tys, false))
                    .cloned()
            {
                let call_args: Vec<Value> = arg_vals
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(i, (v, t))| self.coerce(v, t, sig.params[i]))
                    .collect();
                let r = self.fb.call(sig.full_name, call_args, sig.ret);
                return (r, sig.ret);
            }
        }
        // Instance call `obj.Method(args)` / `this.Method(args)`. `members: true`
        // admits instance overloads (matched past `this`) and statics.
        if let Some((body_ptr, owner_id)) = self.struct_base(base, src)
            && let Some(sig) = self.structs.methods[owner_id.0 as usize]
                .get(mname)
                .and_then(|cands| pick_overload(cands, &arg_tys, true))
                .cloned()
        {
            let mut call_args = Vec::new();
            let mut pidx = 0;
            if sig.is_instance {
                call_args.push(body_ptr);
                pidx = 1;
            }
            for (v, t) in arg_vals {
                let pt = sig.params.get(pidx).copied().unwrap_or(t);
                call_args.push(self.coerce(v, t, pt));
                pidx += 1;
            }
            let r = self.fb.call(sig.full_name, call_args, sig.ret);
            return (r, sig.ret);
        }
        // Unresolved — arguments were already evaluated for their effects.
        (undef(IrType::I64), IrType::I64)
    }

    fn assign(&mut self, op: AssignOp, target: &Expr, value: &Expr, src: &str) -> (Value, IrType) {
        let (rhs, rhs_ty) = self.expr(value, src);
        // Resolve the target to a place (local slot or struct field). The
        // stored value takes the place's type so later loads stay consistent.
        if let Some((slot, ty)) = self.lvalue(target, src) {
            let rhs = self.coerce(rhs, rhs_ty, ty);
            let stored = match compound_op(op) {
                Some(astbin) => {
                    let cur = self.fb.load(slot.clone(), ty);
                    self.arith(astbin, cur, rhs, ty)
                }
                None => rhs, // plain `=`
            };
            self.fb.store(slot, stored.clone());
            return (stored, ty);
        }
        // Unsupported lvalue (index/deref/…) — not lowered yet.
        (rhs, rhs_ty)
    }

    /// Lower `++`/`--` against a local lvalue: load, add/sub one, store. `pre`
    /// selects the prefix form (result is the *new* value) over postfix (the
    /// *old* value). On a non-local operand (a field/index — not lowered yet)
    /// it just evaluates the operand for its value, emitting no store.
    fn incdec(&mut self, operand: &Expr, delta: i128, pre: bool, src: &str) -> (Value, IrType) {
        if let Some((slot, ty)) = self.lvalue(operand, src) {
            let cur = self.fb.load(slot.clone(), ty);
            let (op, one) = if ty.is_float() {
                (IrBin::FAdd, Value::float(delta as f64, ty))
            } else {
                (IrBin::Add, Value::int(delta, ty))
            };
            let next = self.fb.bin(op, cur.clone(), one, ty);
            self.fb.store(slot, next.clone());
            return (if pre { next } else { cur }, ty);
        }
        self.expr(operand, src)
    }

    /// Coerce a value to `i1`: comparisons already are; otherwise `!= 0`.
    fn coerce_bool(&mut self, v: Value, ty: IrType) -> Value {
        if ty == IrType::Bool {
            v
        } else {
            self.fb.cmp(CmpPred::Ne, v, zero_of(ty))
        }
    }

    /// Convert `v` (currently of type `from`) to type `to`, emitting the
    /// appropriate IR cast. Keeps the IR well-typed at every use site; when no
    /// single cast bridges the two (e.g. `ptr`↔`float`) it yields a typed
    /// `undef` rather than emitting an ill-typed instruction.
    fn coerce(&mut self, v: Value, from: IrType, to: IrType) -> Value {
        if from == to {
            return v;
        }
        // To bool is a truthiness test, not a bit-level cast.
        if to == IrType::Bool {
            return self.coerce_bool(v, from);
        }
        // Target-typed literal: a C-string (`char8*`/`Ptr`, e.g. a string
        // literal) used where `String` is expected → construct a `String` from
        // it. So `String s = "hi"` and passing a literal to a `String` param /
        // returning one all wrap automatically.
        if from == IrType::Ptr
            && let IrType::Ref(rid) = to
            && self.structs.by_name.get("String") == Some(&rid)
        {
            return self.construct_string(v);
        }
        match (from, to) {
            // Same-width integers share one LLVM type (signedness isn't in the
            // type), so no cast is needed.
            (a, b) if a.is_int() && b.is_int() && a.bit_width() == b.bit_width() => v,
            (a, b) if a.is_int() && b.is_int() => {
                let kind = if b.bit_width() > a.bit_width() {
                    if a.is_signed() {
                        CastKind::SExt
                    } else {
                        CastKind::ZExt
                    }
                } else {
                    CastKind::Trunc
                };
                self.fb.cast(kind, v, to)
            }
            (a, b) if a.is_int() && b.is_float() => {
                let kind = if a.is_signed() {
                    CastKind::SiToFp
                } else {
                    CastKind::UiToFp
                };
                self.fb.cast(kind, v, to)
            }
            (a, b) if a.is_float() && b.is_int() => {
                let kind = if b.is_signed() {
                    CastKind::FpToSi
                } else {
                    CastKind::FpToUi
                };
                self.fb.cast(kind, v, to)
            }
            (a, b) if a.is_float() && b.is_float() => {
                let kind = if b.bit_width() > a.bit_width() {
                    CastKind::FpExt
                } else {
                    CastKind::FpTrunc
                };
                self.fb.cast(kind, v, to)
            }
            // Pointer-like (`Ptr`/`Ref`) ↔ pointer-like: same LLVM `ptr`, so a
            // plain reinterpret (no cast instruction needed).
            (a, b) if a.is_pointer() && b.is_pointer() => v,
            (a, b) if a.is_pointer() && b.is_int() => self.fb.cast(CastKind::PtrToInt, v, to),
            (a, b) if a.is_int() && b.is_pointer() => self.fb.cast(CastKind::IntToPtr, v, to),
            // No single cast bridges the gap — stay well-typed with an undef.
            _ => undef(to),
        }
    }
}

fn with_float(is_float: bool, int_op: IrBin, float_op: IrBin) -> IrBin {
    if is_float { float_op } else { int_op }
}

/// Float bit width, or 0 for non-floats (helper for [`common_type`]).
fn float_bits(t: IrType) -> u16 {
    if t.is_float() { t.bit_width() } else { 0 }
}

/// The common type two operands are promoted to for a binary op. Mirrors a
/// C-like promotion: any float wins; otherwise the wider integer (signed if
/// either is). Pointers have no LLVM arithmetic/compare ops, so a pointer
/// operand drops into the integer domain (address arithmetic).
fn common_type(a: IrType, b: IrType) -> IrType {
    let t = match (a, b) {
        _ if a == b => a,
        (x, y) if x.is_float() || y.is_float() => IrType::Float {
            bits: float_bits(x).max(float_bits(y)).max(32),
        },
        (x, y) if x.is_int() && y.is_int() => {
            let bits = x.bit_width().max(y.bit_width()).max(1);
            if bits <= 1 {
                IrType::Bool
            } else {
                IrType::Int {
                    bits,
                    signed: x.is_signed() || y.is_signed(),
                }
            }
        }
        // ptr/int mix, ptr/float, void, … → integer domain.
        _ => IrType::I64,
    };
    if t == IrType::Ptr { IrType::I64 } else { t }
}

fn cmp_pred(op: AstBin, ty: IrType) -> CmpPred {
    let f = ty.is_float();
    let s = ty.is_signed();
    match op {
        AstBin::Eq => {
            if f {
                CmpPred::FOeq
            } else {
                CmpPred::Eq
            }
        }
        AstBin::Ne => {
            if f {
                CmpPred::FOne
            } else {
                CmpPred::Ne
            }
        }
        AstBin::Lt => float_signed(f, s, CmpPred::FOlt, CmpPred::Slt, CmpPred::Ult),
        AstBin::Le => float_signed(f, s, CmpPred::FOle, CmpPred::Sle, CmpPred::Ule),
        AstBin::Gt => float_signed(f, s, CmpPred::FOgt, CmpPred::Sgt, CmpPred::Ugt),
        AstBin::Ge => float_signed(f, s, CmpPred::FOge, CmpPred::Sge, CmpPred::Uge),
        _ => CmpPred::Eq,
    }
}

fn float_signed(f: bool, s: bool, fp: CmpPred, signed: CmpPred, unsigned: CmpPred) -> CmpPred {
    if f {
        fp
    } else if s {
        signed
    } else {
        unsigned
    }
}

/// The arithmetic op behind a compound assignment (`+=` → `Add`); `None` for
/// plain `=` and not-yet-lowered forms (`??=`).
fn compound_op(op: AssignOp) -> Option<AstBin> {
    Some(match op {
        AssignOp::Assign => return None,
        AssignOp::Add => AstBin::Add,
        AssignOp::Sub => AstBin::Sub,
        AssignOp::Mul => AstBin::Mul,
        AssignOp::Div => AstBin::Div,
        AssignOp::Mod => AstBin::Mod,
        AssignOp::And => AstBin::BitAnd,
        AssignOp::Or => AstBin::BitOr,
        AssignOp::Xor => AstBin::BitXor,
        AssignOp::Shl => AstBin::Shl,
        AssignOp::Shr => AstBin::Shr,
        AssignOp::NullCoalesce => return None,
    })
}

fn zero_of(ty: IrType) -> Value {
    match ty {
        IrType::Float { .. } => Value::float(0.0, ty),
        IrType::Bool => Value::bool(false),
        IrType::Ptr => Value::Const(Const::Null),
        IrType::Void => Value::Const(Const::Undef(ty)),
        IrType::Int { .. } => Value::int(0, ty),
        // A reference's zero is the null pointer.
        IrType::Ref(_) => Value::Const(Const::Null),
        // Aggregates have no scalar zero; an `undef` keeps the IR well-typed.
        IrType::Struct(_) => Value::Const(Const::Undef(ty)),
    }
}

fn undef(ty: IrType) -> Value {
    Value::Const(Const::Undef(ty))
}

/// Decode a char-literal token (`'A'`, `'\n'`, `'\''`, …) to its code value.
/// Common C escapes; otherwise the first character's scalar value.
fn decode_char_literal(raw: &str) -> i128 {
    let body = raw.strip_prefix('\'').unwrap_or(raw);
    let body = body.strip_suffix('\'').unwrap_or(body);
    let mut chars = body.chars();
    match chars.next() {
        Some('\\') => match chars.next() {
            Some('n') => 10,
            Some('t') => 9,
            Some('r') => 13,
            Some('0') => 0,
            Some('\\') => 92,
            Some('\'') => 39,
            Some('"') => 34,
            Some(c) => c as i128,
            None => 0,
        },
        Some(c) => c as i128,
        None => 0,
    }
}

/// Decode a string-literal token (surrounding quotes + escapes) into its
/// runtime bytes. Handles plain `"..."` with the common C escapes; the `@`
/// (verbatim) / `$` (interpolated) prefixes are stripped best-effort —
/// interpolation itself isn't lowered yet, so `$"…"` keeps its literal text.
fn decode_string_literal(raw: &str) -> String {
    let verbatim = raw.starts_with('@') || raw.starts_with("$@") || raw.starts_with("@$");
    let body = raw.trim_start_matches(['@', '$']);
    let body = body.strip_prefix('"').unwrap_or(body);
    let body = body.strip_suffix('"').unwrap_or(body);
    if verbatim {
        return body.replace("\"\"", "\"");
    }
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('\'') => out.push('\''),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Map an AST type to its concrete IR type. Reference/unknown types collapse
/// to an opaque pointer (correct for classes; a kernel approximation for
/// value structs/tuples until the layout sprint).
/// The class name a `new`/`scope` operand constructs: `C`, `C(args)`, and
/// `C<T>(args)` all name `C`.
fn ctor_class_name<'s>(e: &Expr, src: &'s str) -> Option<&'s str> {
    match e {
        Expr::Ident(s) => Some(s.text(src)),
        Expr::Paren { inner, .. } => ctor_class_name(inner, src),
        Expr::Call { callee, .. } => ctor_class_name(callee, src),
        Expr::Generic { base, .. } => ctor_class_name(base, src),
        _ => None,
    }
}

/// The external symbol a body-less `[Intrinsic("sym")]` / `[LinkName("sym")]`
/// method binds to (so `Internal.MemCpy` calls `memcpy`). `None` if the method
/// has no such attribute.
fn extern_symbol(attrs: &[Attribute], src: &str) -> Option<String> {
    for a in attrs {
        let aname = match &a.name {
            AstType::Path { segments, .. } => segments.last().map(|s| s.name.text(src)),
            _ => None,
        };
        if matches!(aname, Some("Intrinsic" | "LinkName"))
            && let Some(Expr::Str(s)) = a.args.first()
        {
            return Some(decode_string_literal(s.text(src)));
        }
    }
    None
}

/// The constructor argument expressions in a `new` operand: `new C(a, b)` →
/// `[a, b]`; empty for `new C` / `new C()`.
fn ctor_args(e: &Expr) -> &[Expr] {
    match e {
        Expr::Call { args, .. } => args,
        Expr::Paren { inner, .. } => ctor_args(inner),
        _ => &[],
    }
}

/// The element type of an AST pointer type (`T*` → `T`), for typed indexing.
/// `None` for non-pointer types.
fn pointer_elem(ty: &AstType, src: &str, structs: &StructTable) -> Option<IrType> {
    match ty {
        AstType::Pointer { inner, .. } => Some(lower_ty(inner, src, structs)),
        AstType::Nullable { inner, .. } => pointer_elem(inner, src, structs),
        _ => None,
    }
}

fn lower_ty(ty: &AstType, src: &str, structs: &StructTable) -> IrType {
    match ty {
        AstType::Path { segments, .. } if segments.len() == 1 && segments[0].args.is_empty() => {
            // A single-segment name that's a registered value `struct` lowers to
            // its aggregate; otherwise a primitive (or `Ptr` for a reference).
            let name = segments[0].name.text(src);
            structs.ty_of(name).unwrap_or_else(|| primitive(name))
        }
        AstType::Pointer { .. } => IrType::Ptr,
        AstType::Nullable { inner, .. } => lower_ty(inner, src, structs),
        _ => IrType::Ptr,
    }
}

fn primitive(name: &str) -> IrType {
    match name {
        "void" => IrType::Void,
        "bool" => IrType::Bool,
        "int" | "int64" | "intptr" => IrType::I64,
        "int8" => IrType::I8,
        "int16" => IrType::I16,
        "int32" => IrType::I32,
        "uint" | "uint64" | "uintptr" => IrType::U64,
        "uint8" | "char8" => IrType::U8,
        "uint16" | "char16" => IrType::Int {
            bits: 16,
            signed: false,
        },
        "uint32" | "char32" => IrType::U32,
        "float" => IrType::F32,
        "double" => IrType::F64,
        // A named non-primitive type is a reference (class) — a pointer.
        _ => IrType::Ptr,
    }
}

fn parse_int(text: &str) -> i128 {
    let cleaned: String = text.chars().filter(|c| *c != '_' && *c != '\'').collect();
    let lower = cleaned.to_ascii_lowercase();
    let (radix, digits) = if let Some(h) = lower.strip_prefix("0x") {
        (16, h)
    } else if let Some(b) = lower.strip_prefix("0b") {
        (2, b)
    } else if let Some(o) = lower.strip_prefix("0o") {
        (8, o)
    } else {
        (10, lower.as_str())
    };
    let valid: String = digits.chars().take_while(|c| c.is_digit(radix)).collect();
    i128::from_str_radix(&valid, radix).unwrap_or(0)
}

fn parse_float(text: &str) -> f64 {
    let cleaned: String = text.chars().filter(|c| *c != '_').collect();
    cleaned
        .trim_end_matches(['f', 'F', 'd', 'D'])
        .parse::<f64>()
        .unwrap_or(0.0)
}
