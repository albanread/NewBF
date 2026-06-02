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

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use newbf_ir::{
    BinOp as IrBin, BlockId, CastKind, CmpPred, Const, FieldDef, Function, FunctionBuilder,
    GlobalDef, IrType, Module, Param as IrParam, StructDef, StructId, Value, VtableDef,
};
use newbf_lexer::{FileId, Span};
use newbf_parser::{
    AccessorKind, AssignOp, Attribute, BinOp as AstBin, CompUnit, Expr, Item, Member, MethodBody,
    Modifier, Param as AstParam, PrefixKw, Stmt, SwitchArm, Type as AstType, TypeDecl, TypeKind,
    UnOp, parse_file,
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
    /// Per-id base class (the first class in a type's base list), for single
    /// inheritance: `apply_inheritance` composes the base's fields/methods into
    /// the derived. `None` for roots and value structs.
    bases: Vec<Option<StructId>>,
    /// Per-id `virtual`/`override` methods declared *in this type*, as
    /// `(name, impl symbol)` in declaration order — the input to vtable layout.
    virtuals: Vec<Vec<(String, String)>>,
    /// Per-id composed vtable: method name → slot index (consistent across a
    /// base and its derived). A virtual call indexes the receiver's vtable here.
    vslots: Vec<HashMap<String, usize>>,
    /// Per-id composed vtable: slot → implementing symbol. Emitted as the
    /// class's vtable global; non-empty only for classes with virtual methods.
    vimpls: Vec<Vec<String>>,
    /// Monomorphized generic instantiations to lower: `(mono id, generic type
    /// name, type-parameter env)`. `lower_program` re-finds each generic decl by
    /// name and lowers its methods at the mono id/prefix with the env.
    monos: Vec<MonoRecord>,
    /// Generic-method monomorphs: mangled symbol -> lowered signature, so a call
    /// site `Identity<int>(x)` resolves to a direct call.
    gen_method_sigs: HashMap<String, MethodSig>,
    /// (mangled symbol, method name, type-param env) per generic-method
    /// instantiation, so lowering re-finds the decl and emits its body.
    gen_method_monos: Vec<GenMethodMono>,
    /// Int-backed enums: enum name -> (case name -> value). An enum type lowers to
    /// `int32`; `Enum.Case` is the constant. Only enums whose cases ALL lack a
    /// payload land here; a payload-bearing enum is a tagged-union struct instead.
    enums: HashMap<String, HashMap<String, i64>>,
    /// Payload (tagged-union) enums — `enum Opt { Some(int32), None }`: the enum
    /// name -> the value-struct id backing it (`{$disc:i32, $p0, $p1, …}`). The
    /// enum type lowers to `Struct(id)`; a case constructs/matches that struct.
    payload_enums: HashMap<String, StructId>,
    /// Per payload-enum struct id: its cases in declaration order, each
    /// `(case name, discriminant, that case's payload field types)`. Drives
    /// construction (store disc + payload) and `match` (test disc, bind payload).
    enum_cases: HashMap<StructId, Vec<(String, i64, Vec<IrType>)>>,
    /// `static` fields → a mutable global. Key is the global symbol
    /// `{prefix}{field}` (e.g. "Counter.Total"); value is its IR type.
    statics: HashMap<String, IrType>,
    /// Anonymous lambdas: the lambda expression's span → the free-function
    /// symbol it was emitted as. `lower_program` collects + emits each
    /// `function R() f = () => …` lambda; `Expr::Lambda` lowers to its address.
    lambda_names: HashMap<Span, String>,
    /// Closure captures per lambda symbol (`$lambdaN`): the outer locals a lambda
    /// body references, as `(name, type)` in environment order. Filled at the
    /// lambda-creation site (where the enclosing scope is live) via interior
    /// mutability, and read back when the lambda body is emitted. Empty (or
    /// absent) ⇒ a non-capturing lambda (a bare function pointer).
    lambda_captures: RefCell<HashMap<String, Vec<(String, IrType)>>>,
}

/// A monomorphization to lower: `(mono id, generic type name, type-param env)`.
type MonoRecord = (StructId, String, Vec<(String, IrType)>);

/// A generic-method monomorph to lower: `(mangled symbol, method name,
/// type-param env)`. `lower_program` re-finds the decl by name and emits it.
type GenMethodMono = (String, String, Vec<(String, IrType)>);

impl StructTable {
    fn build(files: &[SourceFile<'_>]) -> Self {
        let mut t = StructTable::default();
        // 1. Register every non-generic type's name/id, and int-backed enums.
        for f in files {
            register_struct_names(&f.unit.items, "", f.src, &mut t);
            register_enums(&f.unit.items, f.src, &mut t);
        }
        // 1b. Reclassify payload-bearing enums as tagged-union structs *before*
        //     members/signatures fill — so a `Shape`-typed param/field/return
        //     resolves to the struct everywhere (the call site's recorded sig and
        //     the function definition agree). `fill_type_struct` skips enums
        //     (struct_kind = None), so it won't clobber the synthetic layout.
        for f in files {
            register_payload_enums(&f.unit.items, f.src, &mut t);
        }
        // 2. Index generic declarations by name (with their owning file's src),
        //    so an instantiation can find the template and its parameter names.
        let mut generics: GenericDecls = HashMap::new();
        for f in files {
            index_generic_decls(&f.unit.items, f.src, &mut generics);
        }
        let mut gmethods: GenMethodDecls = HashMap::new();
        for f in files {
            index_generic_methods(&f.unit.items, f.src, &mut gmethods);
        }
        // 3. Collect generic instantiations across the program; register one
        //    monomorphized type per distinct (generic, concrete-args) pair.
        let mut monos: MonoList = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for f in files {
            collect_insts_items(
                &f.unit.items,
                f.src,
                &generics,
                &gmethods,
                &mut t,
                &mut seen,
                &mut monos,
            );
        }
        // 4. Fill ordinary types, then each monomorph's members with its env,
        //    and record the monomorphs so lowering can emit their method bodies.
        for f in files {
            fill_struct_fields(&f.unit.items, f.src, &mut t);
        }
        for (id, decl, decl_src, env) in &monos {
            let kind = struct_kind(decl).unwrap_or(StructKind::Value);
            fill_members_at(decl, *id, kind, env, decl_src, &mut t, true);
            t.monos
                .push((*id, decl.name.text(decl_src).to_string(), env.clone()));
        }
        // 4c. Generic payload enums: overwrite each enum monomorph's (empty) layout
        //     with its tagged-union fields, resolving payload types through the
        //     mono env (`Opt<int>` → `{$disc, $p0:i32}`). After the fill above, which
        //     leaves an enum mono field-less.
        for (id, decl, decl_src, env) in &monos {
            if decl.kind == TypeKind::Enum {
                register_payload_enum_mono(*id, decl, decl_src, env, &mut t);
            }
        }
        // 5. Compose single inheritance once every type's own layout is filled,
        //    then lay out vtables (which inherit/override across that hierarchy).
        apply_inheritance(&mut t);
        apply_vtables(&mut t);
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

/// Index top-level/namespace generic type declarations by name, paired with
/// their owning file's `src` (parameter names + member text are read from it).
fn index_generic_decls<'a>(items: &'a [Item], src: &'a str, out: &mut GenericDecls<'a>) {
    for item in items {
        match item {
            Item::Namespace {
                body: Some(body), ..
            } => index_generic_decls(body, src, out),
            // Generic value structs / classes — and generic *simple payload enums*
            // (`Option<T>`), which monomorphize into tagged-union structs the same
            // way (their `struct_kind` is `None`, so name them explicitly).
            Item::Type(td)
                if !td.generic_params.is_empty()
                    && (struct_kind(td).is_some()
                        || (td.kind == TypeKind::Enum
                            && enum_has_payload(td)
                            && enum_is_simple(td))) =>
            {
                out.entry(td.name.text(src).to_string())
                    .or_insert((td, src));
            }
            _ => {}
        }
    }
}

type GenMethodDecls<'a> = HashMap<String, (&'a Member, &'a str)>;

/// Index generic methods (those with type parameters) by name, paired with the
/// owning file's `src`.
fn index_generic_methods<'a>(items: &'a [Item], src: &'a str, out: &mut GenMethodDecls<'a>) {
    for item in items {
        match item {
            Item::Namespace {
                body: Some(body), ..
            } => index_generic_methods(body, src, out),
            Item::Type(td) => {
                for m in &td.members {
                    if let Member::Method {
                        name,
                        generic_params,
                        ..
                    } = m
                        && !generic_params.is_empty()
                    {
                        out.entry(name.text(src).to_string()).or_insert((m, src));
                    }
                }
            }
            _ => {}
        }
    }
}

/// Register a monomorphized instantiation as a fresh concrete type and return
/// its id (fields are filled later via [`fill_fields_at`] with the env).
fn register_mono(t: &mut StructTable, mangled: &str, kind: StructKind) -> StructId {
    let id = StructId(t.defs.len() as u32);
    t.defs.push(StructDef {
        name: mangled.to_string(),
        fields: Vec::new(),
    });
    t.kinds.push(kind);
    t.prefixes.push(format!("{mangled}."));
    t.ctors.push(Vec::new());
    t.dtors.push(None);
    t.methods.push(HashMap::new());
    t.field_elems.push(Vec::new());
    t.bases.push(None);
    t.virtuals.push(Vec::new());
    t.vslots.push(HashMap::new());
    t.vimpls.push(Vec::new());
    t.by_name.insert(mangled.to_string(), id);
    id
}

/// Compose single inheritance across the table: each class with a base gains
/// the base's fields (right after its own `$header`), the matching field-element
/// types, and any base methods/destructor it doesn't itself declare. Recursive +
/// memoized, so a chain (`Cat : Dog : Animal`) composes base-first.
fn apply_inheritance(t: &mut StructTable) {
    let mut composed = vec![false; t.defs.len()];
    for i in 0..t.defs.len() {
        compose_inheritance(StructId(i as u32), t, &mut composed);
    }
}

fn compose_inheritance(id: StructId, t: &mut StructTable, composed: &mut [bool]) {
    let i = id.0 as usize;
    if composed[i] {
        return;
    }
    composed[i] = true;
    let Some(base) = t.bases[i] else {
        return;
    };
    compose_inheritance(base, t, composed);
    let b = base.0 as usize;
    // Layout: own `$header`, then the base's (already-composed) non-header
    // fields, then this type's own fields — so a derived pointer is
    // prefix-compatible with the base (a base method reads inherited fields at
    // the same offsets).
    let base_fields: Vec<FieldDef> = t.defs[b].fields.iter().skip(1).cloned().collect();
    let base_elems: Vec<Option<IrType>> = t.field_elems[b].iter().skip(1).cloned().collect();
    let own_fields = t.defs[i].fields.split_off(1);
    t.defs[i].fields.extend(base_fields);
    t.defs[i].fields.extend(own_fields);
    let own_elems = t.field_elems[i].split_off(1);
    t.field_elems[i].extend(base_elems);
    t.field_elems[i].extend(own_elems);
    // Inherit methods (name-level) the derived doesn't override; an inherited
    // sig keeps the base's symbol + `this` type, called on a prefix-compatible
    // derived pointer.
    for (name, sigs) in t.methods[b].clone() {
        t.methods[i].entry(name).or_insert(sigs);
    }
    if t.dtors[i].is_none() {
        let inherited = t.dtors[b].clone();
        t.dtors[i] = inherited;
    }
}

/// The vtable global's symbol for a class prefix (`"Animal."` → `"Animal.$vtable"`).
fn vtable_name(prefix: &str) -> String {
    format!("{prefix}$vtable")
}

/// Compose vtables across the table (recursive, memoized, base-first): inherit
/// the base's slots, let an `override` replace a slot's implementation, and
/// append a new slot for each newly-introduced `virtual` method. Slot indices
/// stay stable from base to derived, so a call site resolves the slot from the
/// static type and the receiver's vtable supplies the runtime implementation.
fn apply_vtables(t: &mut StructTable) {
    let mut done = vec![false; t.vimpls.len()];
    for i in 0..t.vimpls.len() {
        compose_vtable(StructId(i as u32), t, &mut done);
    }
}

fn compose_vtable(id: StructId, t: &mut StructTable, done: &mut [bool]) {
    let i = id.0 as usize;
    if done[i] {
        return;
    }
    done[i] = true;
    if let Some(base) = t.bases[i] {
        compose_vtable(base, t, done);
        let b = base.0 as usize;
        t.vslots[i] = t.vslots[b].clone();
        t.vimpls[i] = t.vimpls[b].clone();
    }
    for (name, full) in t.virtuals[i].clone() {
        if let Some(&slot) = t.vslots[i].get(&name) {
            t.vimpls[i][slot] = full;
        } else {
            let slot = t.vimpls[i].len();
            t.vslots[i].insert(name, slot);
            t.vimpls[i].push(full);
        }
    }
}

type MonoList<'a> = Vec<(StructId, &'a TypeDecl, &'a str, Vec<(String, IrType)>)>;

/// Generic type declarations indexed by name, each with its owning file's src.
type GenericDecls<'a> = HashMap<String, (&'a TypeDecl, &'a str)>;

/// Record the monomorphization a generic type reference demands (`Box<int>` →
/// register `Box$i64`), then recurse into its arguments and any wrapped type.
fn use_in_type<'a>(
    ty: &AstType,
    src: &'a str,
    generics: &GenericDecls<'a>,
    t: &mut StructTable,
    seen: &mut Vec<String>,
    monos: &mut MonoList<'a>,
) {
    if let AstType::Path { segments, .. } = ty
        && segments.len() == 1
        && !segments[0].args.is_empty()
    {
        record_inst(
            segments[0].name.text(src),
            &segments[0].args,
            src,
            generics,
            t,
            seen,
            monos,
        );
    }
    if let AstType::Pointer { inner, .. } | AstType::Nullable { inner, .. } = ty {
        use_in_type(inner, src, generics, t, seen, monos);
    }
}

/// Register the monomorph a `Name<Args>` reference demands (`Box<int>` →
/// `Box$i64`) when `Name` is a known generic and it isn't already recorded,
/// then recurse into the type arguments for nested instantiations. Shared by
/// type-position (`use_in_type`) and expression-position (`collect_insts_expr`)
/// collection.
fn record_inst<'a>(
    name: &str,
    args: &[AstType],
    src: &'a str,
    generics: &GenericDecls<'a>,
    t: &mut StructTable,
    seen: &mut Vec<String>,
    monos: &mut MonoList<'a>,
) {
    let Some(&(decl, decl_src)) = generics.get(name) else {
        return;
    };
    let argtys: Vec<IrType> = args.iter().map(|a| lower_ty_env(a, src, t, &[])).collect();
    let mangled = mangle_generic(name, &argtys);
    if !seen.iter().any(|s| s == &mangled) {
        seen.push(mangled.clone());
        let kind = struct_kind(decl).unwrap_or(StructKind::Value);
        let id = register_mono(t, &mangled, kind);
        let env: Vec<(String, IrType)> = decl
            .generic_params
            .iter()
            .zip(&argtys)
            .map(|(gp, ty)| (gp.name.text(decl_src).to_string(), *ty))
            .collect();
        monos.push((id, decl, decl_src, env));
    }
    for a in args {
        use_in_type(a, src, generics, t, seen, monos);
    }
}

/// Record the monomorph a generic-method call `Name<Args>(...)` demands. Dedup
/// is by presence in `gen_method_sigs` (no separate seen-set needed).
fn record_method_inst(
    name: &str,
    targs: &[AstType],
    src: &str,
    gmethods: &GenMethodDecls,
    t: &mut StructTable,
) {
    let Some(&(member, mdecl_src)) = gmethods.get(name) else {
        return;
    };
    let Member::Method {
        generic_params,
        params,
        return_ty,
        ..
    } = member
    else {
        return;
    };
    let argtys: Vec<IrType> = targs.iter().map(|a| lower_ty_env(a, src, t, &[])).collect();
    if argtys.len() != generic_params.len() {
        return;
    }
    let mangled = mangle_generic(name, &argtys);
    if t.gen_method_sigs.contains_key(&mangled) {
        return;
    }
    let env: Vec<(String, IrType)> = generic_params
        .iter()
        .zip(&argtys)
        .map(|(gp, ty)| (gp.name.text(mdecl_src).to_string(), *ty))
        .collect();
    let psig: Vec<IrType> = params
        .iter()
        .map(|p| lower_ty_env(&p.ty, mdecl_src, t, &env))
        .collect();
    let ret = lower_ty_env(return_ty, mdecl_src, t, &env);
    t.gen_method_sigs.insert(
        mangled.clone(),
        MethodSig {
            full_name: mangled.clone(),
            ret,
            params: psig,
            is_instance: false,
        },
    );
    t.gen_method_monos.push((mangled, name.to_string(), env));
}

fn collect_insts_items<'a>(
    items: &'a [Item],
    src: &'a str,
    generics: &GenericDecls<'a>,
    gmethods: &GenMethodDecls<'a>,
    t: &mut StructTable,
    seen: &mut Vec<String>,
    monos: &mut MonoList<'a>,
) {
    for item in items {
        match item {
            Item::Namespace {
                body: Some(body), ..
            } => collect_insts_items(body, src, generics, gmethods, t, seen, monos),
            Item::Type(td) => collect_insts_type(td, src, generics, gmethods, t, seen, monos),
            _ => {}
        }
    }
}

fn collect_insts_type<'a>(
    td: &'a TypeDecl,
    src: &'a str,
    generics: &GenericDecls<'a>,
    gmethods: &GenMethodDecls<'a>,
    t: &mut StructTable,
    seen: &mut Vec<String>,
    monos: &mut MonoList<'a>,
) {
    for m in &td.members {
        match m {
            Member::Field { ty, .. } => use_in_type(ty, src, generics, t, seen, monos),
            Member::Method {
                params,
                return_ty,
                body,
                ..
            } => {
                use_in_type(return_ty, src, generics, t, seen, monos);
                for p in params {
                    use_in_type(&p.ty, src, generics, t, seen, monos);
                }
                if let MethodBody::Block(s) = body {
                    collect_insts_stmt(s, src, generics, gmethods, t, seen, monos);
                }
            }
            Member::Constructor { params, body, .. } => {
                for p in params {
                    use_in_type(&p.ty, src, generics, t, seen, monos);
                }
                if let MethodBody::Block(s) = body {
                    collect_insts_stmt(s, src, generics, gmethods, t, seen, monos);
                }
            }
            Member::Nested(n) => collect_insts_type(n, src, generics, gmethods, t, seen, monos),
            _ => {}
        }
    }
}

/// Walk statement bodies for generic instantiations in local-declaration types
/// (`Box<int> b;`). Expression-position instantiations (`new Box<int>()`) arrive
/// with the generic *class* slice.
fn collect_insts_stmt<'a>(
    stmt: &Stmt,
    src: &'a str,
    generics: &GenericDecls<'a>,
    gmethods: &GenMethodDecls<'a>,
    t: &mut StructTable,
    seen: &mut Vec<String>,
    monos: &mut MonoList<'a>,
) {
    match stmt {
        Stmt::Block { stmts, .. } => {
            for s in stmts {
                collect_insts_stmt(s, src, generics, gmethods, t, seen, monos);
            }
        }
        Stmt::Local { ty, init, .. } => {
            if let Some(ty) = ty {
                use_in_type(ty, src, generics, t, seen, monos);
            }
            if let Some(e) = init {
                collect_insts_expr(e, src, generics, gmethods, t, seen, monos);
            }
        }
        Stmt::Expr { expr, .. } => {
            collect_insts_expr(expr, src, generics, gmethods, t, seen, monos)
        }
        Stmt::Return { value: Some(e), .. } => {
            collect_insts_expr(e, src, generics, gmethods, t, seen, monos)
        }
        Stmt::If {
            cond, then, els, ..
        } => {
            collect_insts_expr(cond, src, generics, gmethods, t, seen, monos);
            collect_insts_stmt(then, src, generics, gmethods, t, seen, monos);
            if let Some(e) = els {
                collect_insts_stmt(e, src, generics, gmethods, t, seen, monos);
            }
        }
        Stmt::While { cond, body, .. } | Stmt::DoWhile { body, cond, .. } => {
            collect_insts_expr(cond, src, generics, gmethods, t, seen, monos);
            collect_insts_stmt(body, src, generics, gmethods, t, seen, monos);
        }
        Stmt::ForEach { iter, body, .. } => {
            collect_insts_expr(iter, src, generics, gmethods, t, seen, monos);
            collect_insts_stmt(body, src, generics, gmethods, t, seen, monos);
        }
        Stmt::Defer { body, .. } => {
            collect_insts_stmt(body, src, generics, gmethods, t, seen, monos)
        }
        Stmt::For {
            init,
            cond,
            update,
            body,
            ..
        } => {
            if let Some(i) = init {
                collect_insts_stmt(i, src, generics, gmethods, t, seen, monos);
            }
            if let Some(c) = cond {
                collect_insts_expr(c, src, generics, gmethods, t, seen, monos);
            }
            if let Some(u) = update {
                collect_insts_expr(u, src, generics, gmethods, t, seen, monos);
            }
            collect_insts_stmt(body, src, generics, gmethods, t, seen, monos);
        }
        _ => {}
    }
}

/// Walk an expression for generic instantiations in expression position —
/// chiefly `new Name<Args>(…)` (where the `Name<Args>` is an `Expr::Generic`),
/// so an instantiation reaches monomorphization even without a typed local.
fn collect_insts_expr<'a>(
    e: &Expr,
    src: &'a str,
    generics: &GenericDecls<'a>,
    gmethods: &GenMethodDecls<'a>,
    t: &mut StructTable,
    seen: &mut Vec<String>,
    monos: &mut MonoList<'a>,
) {
    match e {
        Expr::Generic { base, args, .. } => {
            if let Expr::Ident(s) = &**base {
                record_inst(s.text(src), args, src, generics, t, seen, monos);
                record_method_inst(s.text(src), args, src, gmethods, t);
            }
        }
        Expr::Paren { inner, .. } => {
            collect_insts_expr(inner, src, generics, gmethods, t, seen, monos)
        }
        // `sizeof(List<int>)` instantiates the type it names.
        Expr::SizeOf { ty, .. } => use_in_type(ty, src, generics, t, seen, monos),
        Expr::Unary { operand, .. }
        | Expr::PostInc { operand, .. }
        | Expr::PostDec { operand, .. }
        | Expr::Prefix { operand, .. } => {
            collect_insts_expr(operand, src, generics, gmethods, t, seen, monos)
        }
        Expr::Member { base, .. } => {
            collect_insts_expr(base, src, generics, gmethods, t, seen, monos)
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_insts_expr(lhs, src, generics, gmethods, t, seen, monos);
            collect_insts_expr(rhs, src, generics, gmethods, t, seen, monos);
        }
        Expr::Assign { target, value, .. } => {
            collect_insts_expr(target, src, generics, gmethods, t, seen, monos);
            collect_insts_expr(value, src, generics, gmethods, t, seen, monos);
        }
        Expr::Ternary {
            cond, then, els, ..
        } => {
            collect_insts_expr(cond, src, generics, gmethods, t, seen, monos);
            collect_insts_expr(then, src, generics, gmethods, t, seen, monos);
            collect_insts_expr(els, src, generics, gmethods, t, seen, monos);
        }
        Expr::Call { callee, args, .. }
        | Expr::Index {
            base: callee, args, ..
        } => {
            collect_insts_expr(callee, src, generics, gmethods, t, seen, monos);
            for a in args {
                collect_insts_expr(a, src, generics, gmethods, t, seen, monos);
            }
        }
        _ => {}
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
            t.bases.push(None);
            t.virtuals.push(Vec::new());
            t.vslots.push(HashMap::new());
            t.vimpls.push(Vec::new());
            t.by_name.insert(name, id);
        }
    }
    for m in &td.members {
        if let Member::Nested(n) = m {
            register_type_struct(n, &new_prefix, src, t);
        }
    }
}

/// Record every int-backed `enum` and the integer value of each of its cases.
/// Cases number sequentially from `0`; an explicit `= N` (integer literal) sets
/// the running counter, so the next unannotated case is `N + 1`. A case whose
/// `value` is a non-integer-literal expression (can't be evaluated here) falls
/// back to the sequential counter rather than failing. Recurses into namespaces
/// and nested types so enums declared anywhere are registered.
fn register_enums(items: &[Item], src: &str, t: &mut StructTable) {
    for item in items {
        match item {
            Item::Namespace {
                body: Some(body), ..
            } => register_enums(body, src, t),
            Item::Type(td) => register_enum_type(td, src, t),
            _ => {}
        }
    }
}

/// Whether any case of an `enum` carries a payload (`Some(int32)`), making it a
/// tagged union rather than a plain int-backed enum.
fn enum_has_payload(td: &TypeDecl) -> bool {
    td.members
        .iter()
        .any(|m| matches!(m, Member::EnumCase { payload, .. } if !payload.is_empty()))
}

/// Extract `(case name, binding-name spans)` from a `match`/`case` pattern:
/// `.Some(let v)` / `Enum.Some(let v)` → `("Some", [v])`; `.None` / `Enum.None`
/// → `("None", [])`. `None` if the pattern isn't an enum-case shape. (The parser
/// keeps the bound name's span in a `let v` binding, so `args` are `Ident`s.)
fn enum_pattern(pat: &Expr, src: &str) -> Option<(String, Vec<Span>)> {
    match pat {
        Expr::Call { callee, args, .. } => {
            let case = match &**callee {
                Expr::DotIdent { name, .. } | Expr::Member { name, .. } => {
                    name.text(src).to_string()
                }
                _ => return None,
            };
            let binds = args
                .iter()
                .filter_map(|a| match a {
                    Expr::Ident(s) => Some(*s),
                    _ => None,
                })
                .collect();
            Some((case, binds))
        }
        Expr::DotIdent { name, .. } | Expr::Member { name, .. } => {
            Some((name.text(src).to_string(), Vec::new()))
        }
        _ => None,
    }
}

fn register_enum_type(td: &TypeDecl, src: &str, t: &mut StructTable) {
    // Record EVERY enum's case→value table (the int-backed view). A payload-bearing
    // enum may *also* be reclassified as a tagged-union struct by
    // `register_payload_enums` (which takes precedence in type resolution and
    // construction); this int-backed entry is the fallback for enums whose payload
    // we can't yet lay out — e.g. heterogeneous cases like `A(int), B(float)`.
    if td.kind == TypeKind::Enum {
        let enum_name = td.name.text(src).to_string();
        let mut next: i64 = 0;
        for m in &td.members {
            if let Member::EnumCase { name, value, .. } = m {
                // An explicit `= <int literal>` sets the value and reseeds the
                // counter; anything else (or no value) takes the sequential one.
                // `wrapping_add` so a case pinned at `i64::MAX` (real-beef
                // flag/sentinel enums do this) doesn't overflow the counter.
                let val = match value {
                    Some(Expr::Int(s)) => {
                        let v = parse_int(s.text(src)) as i64;
                        next = v.wrapping_add(1);
                        v
                    }
                    _ => {
                        let v = next;
                        next = next.wrapping_add(1);
                        v
                    }
                };
                t.enums
                    .entry(enum_name.clone())
                    .or_default()
                    .insert(name.text(src).to_string(), val);
            }
        }
    }
    // Enums can be declared as nested types too.
    for m in &td.members {
        if let Member::Nested(n) = m {
            register_enum_type(n, src, t);
        }
    }
}

/// Register payload-bearing `enum`s as tagged-union value structs. Each becomes a
/// struct `{$disc:i32, $p0, $p1, …}`: field 0 is the discriminant, and the payload
/// slots are sized to the widest case (slot `i`'s type comes from the first case
/// with an `i`-th field — a homogeneous-position union; heterogeneous payload
/// types per slot are a follow-on). Runs *after* `fill_struct_fields` so that pass
/// can't overwrite the synthetic field list. Records `payload_enums` (name → id)
/// for type resolution + construction, and `enum_cases` (id → cases) for both.
fn register_payload_enums(items: &[Item], src: &str, t: &mut StructTable) {
    for item in items {
        match item {
            Item::Namespace {
                body: Some(body), ..
            } => register_payload_enums(body, src, t),
            Item::Type(td) => register_payload_enum_type(td, src, t),
            _ => {}
        }
    }
}

/// Whether an enum is a *simple* tagged union we can lay out: every member is a
/// case (no methods/properties/ctors on the enum) and it has no base/interface.
/// Method-bearing enums (e.g. corlib `Result<T> : IDisposable`) stay int-backed —
/// reclassifying them would strand their methods, which we don't lower yet.
fn enum_is_simple(td: &TypeDecl) -> bool {
    td.bases.is_empty()
        && td
            .members
            .iter()
            .all(|m| matches!(m, Member::EnumCase { .. }))
}

/// Whether a *non-generic* payload enum can be laid out as a tagged-union struct
/// that also carries methods: no base/interface, and every member is a case or a
/// method (so we can register and emit those methods, e.g. `Option`'s
/// `GetValueOrDefault`). A base-bearing enum (corlib `Result<T> : IDisposable`)
/// stays int-backed, exactly as before. Looser than [`enum_is_simple`], which
/// still gates the *generic* monomorphization path (cases only).
fn enum_is_layoutable(td: &TypeDecl) -> bool {
    td.bases.is_empty()
        && td.members.iter().all(|m| match m {
            Member::EnumCase { .. } | Member::Method { .. } => true,
            // Computed (body-having) properties only: an *auto* property needs a
            // synthesized backing field, but the enum's layout is fixed
            // (`$disc, $p0…`) and the `fill_layout = false` member fill won't add
            // one — so an auto-property-bearing enum stays int-backed.
            Member::Property { accessors, .. } => accessors
                .iter()
                .all(|a| !matches!(a.body, MethodBody::None)),
            _ => false,
        })
}

fn register_payload_enum_type(td: &TypeDecl, src: &str, t: &mut StructTable) {
    if td.kind == TypeKind::Enum
        && enum_has_payload(td)
        && enum_is_layoutable(td)
        && td.generic_params.is_empty()
        && !t.by_name.contains_key(td.name.text(src))
    {
        let enum_name = td.name.text(src).to_string();
        // Collect cases as (name, discriminant, payload field IR types). Discriminants
        // number sequentially from 0; an explicit `= <int>` reseeds the counter
        // (mirroring the int-backed path).
        let mut cases: Vec<(String, i64, Vec<IrType>)> = Vec::new();
        let mut next: i64 = 0;
        for m in &td.members {
            if let Member::EnumCase {
                name,
                value,
                payload,
                ..
            } = m
            {
                let disc = match value {
                    Some(Expr::Int(s)) => {
                        let v = parse_int(s.text(src)) as i64;
                        next = v.wrapping_add(1);
                        v
                    }
                    _ => {
                        let v = next;
                        next = next.wrapping_add(1);
                        v
                    }
                };
                let ptys: Vec<IrType> = payload
                    .iter()
                    .map(|p| lower_ty_env(&p.ty, src, t, &[]))
                    .collect();
                cases.push((name.text(src).to_string(), disc, ptys));
            }
        }
        // Payload slot layout: max arity across cases; slot `i`'s type must be
        // agreed by every case that fills it. A *heterogeneous* position (e.g.
        // `A(int), B(float)`) can't share one typed slot, so such an enum isn't
        // reclassified — it stays int-backed (registered in `enums`). A real
        // size-based union (reinterpreting a byte blob per case) is a follow-on.
        let maxf = cases.iter().map(|(_, _, p)| p.len()).max().unwrap_or(0);
        let mut slots: Vec<IrType> = Vec::with_capacity(maxf);
        let mut homogeneous = true;
        for i in 0..maxf {
            let mut slot: Option<IrType> = None;
            for (_, _, p) in &cases {
                if let Some(&ft) = p.get(i) {
                    match slot {
                        None => slot = Some(ft),
                        Some(prev) if prev == ft => {}
                        Some(_) => homogeneous = false,
                    }
                }
            }
            slots.push(slot.unwrap_or(IrType::I64));
        }
        if homogeneous {
            let mut fields = vec![FieldDef {
                name: "$disc".to_string(),
                ty: IrType::Int {
                    bits: 32,
                    signed: true,
                },
            }];
            for (i, slot_ty) in slots.iter().enumerate() {
                fields.push(FieldDef {
                    name: format!("$p{i}"),
                    ty: *slot_ty,
                });
            }
            let nfields = fields.len();
            let id = StructId(t.defs.len() as u32);
            t.defs.push(StructDef {
                name: enum_name.clone(),
                fields,
            });
            t.kinds.push(StructKind::Value);
            t.prefixes.push(format!("{enum_name}."));
            t.ctors.push(Vec::new());
            t.dtors.push(None);
            t.methods.push(HashMap::new());
            t.field_elems.push(vec![None; nfields]);
            t.bases.push(None);
            t.virtuals.push(Vec::new());
            t.vslots.push(HashMap::new());
            t.vimpls.push(Vec::new());
            t.by_name.insert(enum_name.clone(), id);
            t.payload_enums.insert(enum_name, id);
            t.enum_cases.insert(id, cases);
        }
    }
    for m in &td.members {
        if let Member::Nested(n) = m {
            register_payload_enum_type(n, src, t);
        }
    }
}

/// Register a *generic* payload enum's monomorph (`Option<int>`) as a tagged-union
/// struct, reusing the mono's already-allocated `id`. Runs after the member-fill
/// pass (which leaves an enum mono with empty fields), so it OVERWRITES that id's
/// layout. Payload types resolve through the mono's `env` (so `T` → concrete), and
/// the mangled name (`Option$i32`) maps to `id` for type resolution + construction.
fn register_payload_enum_mono(
    id: StructId,
    td: &TypeDecl,
    src: &str,
    env: TyEnv,
    t: &mut StructTable,
) {
    if !(enum_has_payload(td) && enum_is_simple(td)) {
        return;
    }
    // Collect cases (payload types resolved through the mono env).
    let mut cases: Vec<(String, i64, Vec<IrType>)> = Vec::new();
    let mut next: i64 = 0;
    for m in &td.members {
        if let Member::EnumCase {
            name,
            value,
            payload,
            ..
        } = m
        {
            let disc = match value {
                Some(Expr::Int(s)) => {
                    let v = parse_int(s.text(src)) as i64;
                    next = v.wrapping_add(1);
                    v
                }
                _ => {
                    let v = next;
                    next = next.wrapping_add(1);
                    v
                }
            };
            let ptys: Vec<IrType> = payload
                .iter()
                .map(|p| lower_ty_env(&p.ty, src, t, env))
                .collect();
            cases.push((name.text(src).to_string(), disc, ptys));
        }
    }
    // Homogeneity check (slot `i` agreed by every case that fills it).
    let maxf = cases.iter().map(|(_, _, p)| p.len()).max().unwrap_or(0);
    let mut slots: Vec<IrType> = Vec::with_capacity(maxf);
    let mut homogeneous = true;
    for i in 0..maxf {
        let mut slot: Option<IrType> = None;
        for (_, _, p) in &cases {
            if let Some(&ft) = p.get(i) {
                match slot {
                    None => slot = Some(ft),
                    Some(prev) if prev == ft => {}
                    Some(_) => homogeneous = false,
                }
            }
        }
        slots.push(slot.unwrap_or(IrType::I64));
    }
    if !homogeneous {
        return;
    }
    let mut fields = vec![FieldDef {
        name: "$disc".to_string(),
        ty: IrType::Int {
            bits: 32,
            signed: true,
        },
    }];
    for (i, slot_ty) in slots.iter().enumerate() {
        fields.push(FieldDef {
            name: format!("$p{i}"),
            ty: *slot_ty,
        });
    }
    let nfields = fields.len();
    // Overwrite the mono struct's (empty) layout in place.
    t.defs[id.0 as usize].fields = fields;
    t.field_elems[id.0 as usize] = vec![None; nfields];
    let argtys: Vec<IrType> = env.iter().map(|(_, ty)| *ty).collect();
    let mangled = mangle_generic(td.name.text(src), &argtys);
    t.payload_enums.insert(mangled, id);
    t.enum_cases.insert(id, cases);
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

/// Fill `id`'s field layout (and per-field pointer-element types) from `td`'s
/// instance fields, resolving any generic type-parameters through `env`. Shared
/// by ordinary types (`env = &[]`) and monomorphized generic instantiations
/// (where `td` is the generic declaration and `env` maps its params to concrete
/// types). A class carries a `$header` (ClassVData*) at offset 0; a value struct
/// has none.
fn fill_fields_at(
    td: &TypeDecl,
    id: StructId,
    kind: StructKind,
    env: TyEnv,
    src: &str,
    t: &mut StructTable,
) {
    let mut fields = Vec::new();
    let mut elems: Vec<Option<IrType>> = Vec::new();
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
                // A `static` field becomes a mutable module global keyed by its
                // `{prefix}{field}` symbol (e.g. "Counter.Total"). Register it
                // here, then fall through to skip the instance layout. (Pure
                // non-static `Const` is unchanged: registered nowhere.)
                //
                // Only *scalar* statics (int/float/bool/ptr/ref) are registered:
                // an aggregate (`struct`) static can't be zero-initialized as a
                // single global cleanly, and — because the backend skips emitting
                // such a global — a member access through it (`Type.Field.x`)
                // would address a missing global and drop the receiver argument,
                // breaking call arity. Skipping aggregate statics keeps sema and
                // the backend in lock-step (every registered static has a real
                // global) and leaves the prior (unsupported) behavior intact.
                if modifiers
                    .iter()
                    .any(|(mo, _)| matches!(mo, Modifier::Static))
                {
                    let fty = lower_ty_env(ty, src, t, env);
                    if matches!(
                        fty,
                        IrType::Bool
                            | IrType::Int { .. }
                            | IrType::Float { .. }
                            | IrType::Ptr
                            | IrType::Ref(_)
                    ) {
                        let sym = format!("{}{}", t.prefixes[id.0 as usize], name.text(src));
                        t.statics.insert(sym, fty);
                    }
                }
                continue;
            }
            let fty = lower_ty_env(ty, src, t, env);
            elems.push(pointer_elem_env(ty, src, t, env));
            fields.push(FieldDef {
                name: name.text(src).to_string(),
                ty: fty,
            });
        }
    }
    // An instance auto-property (at least one body-less accessor) gets a
    // compiler-synthesized backing field `{Name}$prop`; the `$` keeps it out
    // of the user namespace. An all-computed property (every accessor has a
    // body) needs no storage and is skipped. Statics aren't in the layout.
    for m in &td.members {
        if let Member::Property {
            ty,
            name,
            accessors,
            modifiers,
            ..
        } = m
        {
            if modifiers
                .iter()
                .any(|(mo, _)| matches!(mo, Modifier::Static))
            {
                continue;
            }
            if !accessors.iter().any(|a| matches!(a.body, MethodBody::None)) {
                continue;
            }
            let pty = lower_ty_env(ty, src, t, env);
            elems.push(pointer_elem_env(ty, src, t, env));
            fields.push(FieldDef {
                name: format!("{}$prop", name.text(src)),
                ty: pty,
            });
        }
    }
    t.defs[id.0 as usize].fields = fields;
    t.field_elems[id.0 as usize] = elems;
}

/// Register `id`'s layout, constructors, destructor, and method table from
/// `td`'s members, resolving generic type-parameters through `env`. Shared by
/// ordinary types (`env = &[]`, `id` from the name table) and monomorphized
/// instantiations (`td` is the generic decl, `id`/prefix the monomorph's,
/// `env` its parameter substitutions). `t.prefixes[id]` supplies the mangled
/// symbol prefix in both cases.
/// Register `td`'s constructors, destructor, and method signatures at `id`. When
/// `fill_layout` is set (the usual case) it first lays out `id`'s fields and
/// records its base; a reclassified payload enum passes `false`, since its
/// tagged-union layout was already built by `register_payload_enums` and an
/// (instance-field-less) refill would clobber it.
fn fill_members_at(
    td: &TypeDecl,
    id: StructId,
    kind: StructKind,
    env: TyEnv,
    src: &str,
    t: &mut StructTable,
    fill_layout: bool,
) {
    if fill_layout {
        fill_fields_at(td, id, kind, env, src, t);

        // Single inheritance: record the first base that resolves to a class.
        // `apply_inheritance` later composes its fields/methods into this type.
        if matches!(kind, StructKind::Ref) {
            for b in &td.bases {
                if let IrType::Ref(bid) = lower_ty_env(b, src, t, env)
                    && bid != id
                {
                    t.bases[id.0 as usize] = Some(bid);
                    break;
                }
            }
        }
    }

    // Constructors (one per distinct arity → `$ctorN`), a destructor, and the
    // this-aware method table for call resolution. The implicit `this` is a
    // reference to the instance body.
    for m in &td.members {
        match m {
            Member::Constructor { params, .. } => {
                let arity = params.len();
                if t.ctors[id.0 as usize]
                    .iter()
                    .all(|c| c.params.len() != arity + 1)
                {
                    let mut ps = vec![IrType::Ref(id)];
                    for p in params {
                        ps.push(lower_ty_env(&p.ty, src, t, env));
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
                generic_params,
                ..
            } => {
                // A generic method is emitted only as monomorphs (its `T` is
                // unresolved here); skip it in the ordinary method table.
                if !generic_params.is_empty() {
                    continue;
                }
                let nm = name.text(src).to_string();
                let explicit: Vec<IrType> = params
                    .iter()
                    .map(|p| lower_ty_env(&p.ty, src, t, env))
                    .collect();
                // An `abstract` instance method is body-less but reserves a
                // vtable slot a derived `override` fills; it mangles like a real
                // method (below) so the base vtable entry references a symbol
                // that's never defined → emitted as a null slot.
                let is_abstract = matches!(body, MethodBody::None)
                    && modifiers
                        .iter()
                        .any(|(mo, _)| matches!(mo, Modifier::Abstract));
                // A body-having (or `abstract`) method emits `{prefix}{name}` —
                // suffixed by parameter types when it's a *later* overload of
                // that name (the first keeps the plain symbol). A body-less
                // `[Intrinsic]`/`[LinkName]` extern resolves to its symbol; any
                // other body-less member (interface signature) isn't callable
                // and is skipped.
                let full_name = if matches!(body, MethodBody::None) && !is_abstract {
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
                    ret: lower_ty_env(return_ty, src, t, env),
                    params: ps,
                    is_instance,
                };
                // A `virtual`/`override`/`abstract` instance method occupies a
                // vtable slot; record it (in declaration order) for layout. A
                // body-having one supplies the impl; an `abstract` one reserves
                // the slot with a null impl for a derived `override` to fill.
                let is_virtual = modifiers.iter().any(|(mo, _)| {
                    matches!(
                        mo,
                        Modifier::Virtual | Modifier::Override | Modifier::Abstract
                    )
                });
                if is_virtual && is_instance && (is_abstract || !matches!(body, MethodBody::None)) {
                    t.virtuals[id.0 as usize].push((nm.clone(), sig.full_name.clone()));
                }
                let bucket = t.methods[id.0 as usize].entry(nm).or_default();
                if !bucket.iter().any(|s| s.params == sig.params) {
                    bucket.push(sig);
                }
            }
            Member::Property {
                ty,
                name,
                accessors,
                modifiers,
                ..
            } => {
                // A `get` accessor registers as a `get_{Name}` instance method;
                // reading `obj.Name` calls it. Both computed (body-having) and
                // auto (body-less, backed by the synthesized `{Name}$prop`
                // field) accessors register here — lowering picks the body.
                let nm = name.text(src).to_string();
                let is_instance = !modifiers
                    .iter()
                    .any(|(mo, _)| matches!(mo, Modifier::Static));
                let pty = lower_ty_env(ty, src, t, env);
                for acc in accessors {
                    if matches!(acc.kind, AccessorKind::Get) {
                        let mut ps = Vec::new();
                        if is_instance {
                            ps.push(IrType::Ref(id));
                        }
                        let sig = MethodSig {
                            full_name: format!("{}get_{}", t.prefixes[id.0 as usize], nm),
                            ret: pty,
                            params: ps,
                            is_instance,
                        };
                        let bucket = t.methods[id.0 as usize]
                            .entry(format!("get_{nm}"))
                            .or_default();
                        if !bucket.iter().any(|s| s.params == sig.params) {
                            bucket.push(sig);
                        }
                    }
                    // A `set` accessor registers as a `set_{Name}` instance
                    // method returning void, taking the property type as its
                    // (implicit `value`) parameter. Computed and auto alike.
                    if matches!(acc.kind, AccessorKind::Set) {
                        let mut ps = Vec::new();
                        if is_instance {
                            ps.push(IrType::Ref(id));
                        }
                        ps.push(pty);
                        let sig = MethodSig {
                            full_name: format!("{}set_{}", t.prefixes[id.0 as usize], nm),
                            ret: IrType::Void,
                            params: ps,
                            is_instance,
                        };
                        let bucket = t.methods[id.0 as usize]
                            .entry(format!("set_{nm}"))
                            .or_default();
                        if !bucket.iter().any(|s| s.params == sig.params) {
                            bucket.push(sig);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn fill_type_struct(td: &TypeDecl, src: &str, t: &mut StructTable) {
    let kind = struct_kind(td).filter(|_| td.generic_params.is_empty());
    let id = kind.and_then(|_| t.by_name.get(td.name.text(src)).copied());
    if let (Some(kind), Some(id)) = (kind, id) {
        fill_members_at(td, id, kind, &[], src, t, true);
    } else if td.kind == TypeKind::Enum
        && td.generic_params.is_empty()
        && let Some(&id) = t.payload_enums.get(td.name.text(src))
    {
        // A reclassified payload enum: register its methods so `obj.Method()`
        // resolves. Its tagged-union field layout already exists (built in
        // `register_payload_enums`), so don't refill it (`fill_layout = false`).
        fill_members_at(td, id, StructKind::Value, &[], src, t, false);
    }
    for m in &td.members {
        if let Member::Nested(n) = m {
            fill_type_struct(n, src, t);
        }
    }
}

/// A lambda queued for emission: `(symbol, return type, (param name, param
/// type) pairs, body, source)`.
type LambdaEmit<'a> = (String, IrType, Vec<(String, IrType)>, &'a Stmt, &'a str);

/// Collect anonymous lambdas to emit as free functions. Minimal slice:
/// paramless lambdas assigned to a `function R()` local (`function R() f =
/// () => …;`) — the target type gives the signature (no inference/capture).
/// Each gets a `$lambdaN` symbol recorded by span; its body is queued to emit.
fn collect_lambdas<'a>(
    items: &'a [Item],
    src: &'a str,
    structs: &mut StructTable,
    emits: &mut Vec<LambdaEmit<'a>>,
) {
    for item in items {
        match item {
            Item::Namespace {
                body: Some(body), ..
            } => collect_lambdas(body, src, structs, emits),
            Item::Type(td) => {
                for m in &td.members {
                    let body = match m {
                        Member::Method {
                            body: MethodBody::Block(s),
                            ..
                        }
                        | Member::Constructor {
                            body: MethodBody::Block(s),
                            ..
                        }
                        | Member::Destructor {
                            body: MethodBody::Block(s),
                            ..
                        } => Some(s),
                        _ => None,
                    };
                    if let Some(s) = body {
                        collect_lambdas_stmt(s, src, structs, emits);
                    }
                }
            }
            _ => {}
        }
    }
}

fn collect_lambdas_stmt<'a>(
    stmt: &'a Stmt,
    src: &'a str,
    structs: &mut StructTable,
    emits: &mut Vec<LambdaEmit<'a>>,
) {
    match stmt {
        Stmt::Block { stmts, .. } => {
            for s in stmts {
                collect_lambdas_stmt(s, src, structs, emits);
            }
        }
        Stmt::Local {
            ty:
                Some(AstType::Function {
                    return_ty,
                    params: tparams,
                    ..
                }),
            init:
                Some(Expr::Lambda {
                    span,
                    params: lparams,
                    body,
                }),
            ..
        } if lparams.len() == tparams.len() => {
            let name = format!("$lambda{}", structs.lambda_names.len());
            let ret = lower_ty_env(return_ty, src, structs, &[]);
            // Lambda params are untyped; the target `function R(P)` supplies the
            // types. Pair each captured param name with its target type.
            let param_pairs: Vec<(String, IrType)> = lparams
                .iter()
                .zip(tparams.iter())
                .map(|(nspan, t)| {
                    (
                        nspan.text(src).to_string(),
                        lower_ty_env(t, src, structs, &[]),
                    )
                })
                .collect();
            structs.lambda_names.insert(*span, name.clone());
            emits.push((name, ret, param_pairs, &**body, src));
        }
        Stmt::If { then, els, .. } => {
            collect_lambdas_stmt(then, src, structs, emits);
            if let Some(e) = els {
                collect_lambdas_stmt(e, src, structs, emits);
            }
        }
        Stmt::While { body, .. }
        | Stmt::DoWhile { body, .. }
        | Stmt::For { body, .. }
        | Stmt::ForEach { body, .. }
        | Stmt::Defer { body, .. } => collect_lambdas_stmt(body, src, structs, emits),
        _ => {}
    }
}

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
    let mut structs = StructTable::build(&all);
    // Collect anonymous lambdas (paramless, target-typed) before lowering: each
    // gets a `$lambdaN` symbol recorded by span (so `Expr::Lambda` lowers to its
    // address) and its body is queued to emit as a free function below.
    let mut lambda_emits: Vec<LambdaEmit> = Vec::new();
    for f in &all {
        collect_lambdas(&f.unit.items, f.src, &mut structs, &mut lambda_emits);
    }
    m.structs = structs.defs.clone();
    // Emit a vtable global for each class that has virtual methods.
    for i in 0..structs.vimpls.len() {
        if !structs.vimpls[i].is_empty() {
            m.add_vtable(VtableDef {
                name: vtable_name(&structs.prefixes[i]),
                entries: structs.vimpls[i].clone(),
            });
        }
    }
    // Emit a mutable module global for each `static` field.
    for (sym, ty) in &structs.statics {
        m.add_global(GlobalDef {
            name: sym.clone(),
            ty: *ty,
        });
    }
    for f in &all {
        lower_items(&f.unit.items, "", f.src, &structs, &mut m);
    }
    // Emit each collected lambda as a free function `$lambdaN() -> R { body }`.
    // An expression body (`=> e`) returns `e`; a block body lowers as-is.
    for (name, ret, params, body, lsrc) in &lambda_emits {
        let mb = match *body {
            Stmt::Expr { expr, .. } => MethodBody::Expr(expr.clone()),
            other => MethodBody::Block(other.clone()),
        };
        let caps = structs
            .lambda_captures
            .borrow()
            .get(name)
            .cloned()
            .unwrap_or_default();
        let func = if caps.is_empty() {
            // Non-capturing → a plain free function; params bind as `extra`.
            let empty: HashMap<String, Vec<MethodSig>> = HashMap::new();
            let extra: Vec<(&str, IrType)> = params.iter().map(|(n, t)| (n.as_str(), *t)).collect();
            lower_method(
                name.clone(),
                *ret,
                &[],
                &mb,
                lsrc,
                &empty,
                &structs,
                None,
                &[],
                &extra,
            )
        } else {
            // Capturing → a closure function taking the env as `$self`.
            emit_closure(name, *ret, params, &caps, &mb, lsrc, &structs)
        };
        if let Some(func) = func {
            m.add_function(func);
        }
    }
    // Lower each monomorphized instantiation's methods/ctors at its mono id and
    // mangled prefix, with its type-parameter env (so a `T` resolves concretely).
    let mut generics: GenericDecls = HashMap::new();
    for f in &all {
        index_generic_decls(&f.unit.items, f.src, &mut generics);
    }
    for (id, name, env) in &structs.monos {
        if let Some(&(decl, decl_src)) = generics.get(name) {
            let prefix = structs.prefixes[id.0 as usize].clone();
            lower_type_at(decl, Some(*id), &prefix, env, decl_src, &structs, &mut m);
        }
    }
    // Emit each generic-*method* monomorph as a static free function: re-find the
    // decl by name and lower its body with the instantiation's type-param env, so
    // a `T` resolves concretely. (`None` this_ty = static; `&[]` extra params.)
    let mut gmethods: GenMethodDecls = HashMap::new();
    for f in &all {
        index_generic_methods(&f.unit.items, f.src, &mut gmethods);
    }
    for (mangled, name, env) in &structs.gen_method_monos {
        if let Some(&(member, mdecl_src)) = gmethods.get(name)
            && let Member::Method {
                return_ty,
                params,
                body,
                ..
            } = member
        {
            let ret = lower_ty_env(return_ty, mdecl_src, &structs, env);
            let empty: HashMap<String, Vec<MethodSig>> = HashMap::new();
            if let Some(func) = lower_method(
                mangled.clone(),
                ret,
                params,
                body,
                mdecl_src,
                &empty,
                &structs,
                None,
                env,
                &[],
            ) {
                m.add_function(func);
            }
        }
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
    let mut best: Option<(&MethodSig, u32)> = None;
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
        let score: u32 = formal
            .iter()
            .zip(arg_tys)
            .map(|(f, a)| type_affinity(*f, *a))
            .sum();
        if best.is_none_or(|(_, bs)| score > bs) {
            best = Some((c, score));
        }
    }
    best.map(|(c, _)| c)
}

/// How well an argument type fits a parameter type for overload ranking: an
/// exact match beats a same-category match (int↔int of any width, float↔float,
/// pointer↔pointer) beats no relation. So an `int` argument prefers an `int`
/// parameter over a `String` one even when neither is the exact width.
fn type_affinity(f: IrType, a: IrType) -> u32 {
    if f == a {
        2
    } else if (f.is_int() && a.is_int())
        || (f.is_float() && a.is_float())
        || (f.is_pointer() && a.is_pointer())
    {
        1
    } else {
        0
    }
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
    // A generic *template* is never lowered directly — only its monomorphs
    // (driven from `structs.monos` in `lower_program`) are.
    if !td.generic_params.is_empty() {
        return;
    }
    let new_prefix = format!("{prefix}{}.", td.name.text(src));
    let owner_id = structs.by_name.get(td.name.text(src)).copied();
    lower_type_at(td, owner_id, &new_prefix, &[], src, structs, m);
}

/// Lower `td`'s methods/ctors/dtor at `owner_id` under `prefix`, resolving
/// generic type-parameters through `env`. Ordinary types pass `env = &[]` and
/// their own id; monomorphs pass the instantiation's id/prefix/env.
fn lower_type_at(
    td: &TypeDecl,
    owner_id: Option<StructId>,
    prefix: &str,
    env: TyEnv,
    src: &str,
    structs: &StructTable,
    m: &mut Module,
) {
    let new_prefix = prefix;
    // The type's own method table (this-aware, built in the pre-pass) resolves
    // same-type bare calls; an empty map covers unregistered types (interfaces).
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
                generic_params,
                ..
            } => {
                // A generic method is emitted only as monomorphs (step 7); its
                // `T` is unresolved in the ordinary lowering env, so skip it.
                if !generic_params.is_empty() {
                    continue;
                }
                // Reuse the table's mangled symbol (it disambiguates overloads)
                // by matching this overload's explicit parameter types; fall back
                // to the plain name for unregistered types (generics/interfaces).
                let explicit: Vec<IrType> = params
                    .iter()
                    .map(|p| lower_ty_env(&p.ty, src, structs, env))
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
                let ret = lower_ty_env(return_ty, src, structs, env);
                // Instance methods take a leading `this`; static ones don't.
                let is_static = modifiers
                    .iter()
                    .any(|(mo, _)| matches!(mo, Modifier::Static));
                let this_ty = match owner_id {
                    Some(id) if !is_static => Some(IrType::Ref(id)),
                    _ => None,
                };
                if let Some(func) = lower_method(
                    full_name,
                    ret,
                    params,
                    body,
                    src,
                    sigs,
                    structs,
                    this_ty,
                    env,
                    &[],
                ) {
                    m.add_function(func);
                }
            }
            // Constructors/destructors lower like instance methods with a
            // leading `this` (a reference to the body); `new`/`delete` call
            // them by the `$ctor`/`$dtor` mangled names recorded in the table.
            Member::Constructor { params, body, .. } => {
                if let Some(id) = owner_id {
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
                        env,
                        &[],
                    ) {
                        m.add_function(func);
                    }
                }
            }
            Member::Destructor { body, .. } => {
                if let Some(id) = owner_id {
                    let full_name = format!("{new_prefix}$dtor");
                    let this = Some(IrType::Ref(id));
                    if let Some(func) = lower_method(
                        full_name,
                        IrType::Void,
                        &[],
                        body,
                        src,
                        sigs,
                        structs,
                        this,
                        env,
                        &[],
                    ) {
                        m.add_function(func);
                    }
                }
            }
            Member::Nested(nested) => lower_type(nested, new_prefix, src, structs, m),
            Member::Property {
                ty,
                name,
                accessors,
                modifiers,
                ..
            } => {
                // Lower each `get`/`set` accessor as the `get_{Name}`/`set_{Name}`
                // method the pre-pass registered. A computed accessor lowers its
                // AST body via `lower_method` (sees `this` like any instance
                // method); an auto accessor has no body, so we synthesize a
                // trivial read/write of the backing field `{Name}$prop`.
                let nm = name.text(src);
                let is_static = modifiers
                    .iter()
                    .any(|(mo, _)| matches!(mo, Modifier::Static));
                let this_ty = match owner_id {
                    Some(id) if !is_static => Some(IrType::Ref(id)),
                    _ => None,
                };
                let ret = lower_ty_env(ty, src, structs, env);
                // Index of the synthesized backing field (instance auto-props
                // only); `None` for computed or static properties.
                let backing = format!("{}$prop", nm);
                let bidx = owner_id
                    .and_then(|oid| {
                        structs.defs[oid.0 as usize]
                            .fields
                            .iter()
                            .position(|f| f.name == backing)
                    })
                    .map(|p| p as u32);
                for acc in accessors {
                    if matches!(acc.kind, AccessorKind::Get)
                        && !matches!(acc.body, MethodBody::None)
                    {
                        let full_name = format!("{new_prefix}get_{nm}");
                        if let Some(func) = lower_method(
                            full_name,
                            ret,
                            &[],
                            &acc.body,
                            src,
                            sigs,
                            structs,
                            this_ty,
                            env,
                            &[],
                        ) {
                            m.add_function(func);
                        }
                    }
                    // Auto getter: synthesize `get_{Name}(this) = this.{Name}$prop`.
                    if matches!(acc.kind, AccessorKind::Get)
                        && matches!(acc.body, MethodBody::None)
                        && let (Some(oid), Some(idx)) = (owner_id, bidx)
                    {
                        let pty = lower_ty_env(ty, src, structs, env);
                        let mut fb = FunctionBuilder::new(
                            format!("{new_prefix}get_{nm}"),
                            vec![IrParam {
                                name: Some("this".to_string()),
                                ty: IrType::Ref(oid),
                            }],
                            pty,
                        );
                        let p = fb.field_addr(Value::Param(0), oid, idx);
                        let v = fb.load(p, pty);
                        fb.ret(Some(v));
                        m.add_function(fb.finish());
                    }
                    // A computed `set` accessor lowers as `set_{Name}`, whose
                    // body sees an implicit `value` param of the property type
                    // (plus `this` like any instance method).
                    if matches!(acc.kind, AccessorKind::Set)
                        && !matches!(acc.body, MethodBody::None)
                    {
                        let pty = lower_ty_env(ty, src, structs, env);
                        let full_name = format!("{new_prefix}set_{nm}");
                        if let Some(func) = lower_method(
                            full_name,
                            IrType::Void,
                            &[],
                            &acc.body,
                            src,
                            sigs,
                            structs,
                            this_ty,
                            env,
                            &[("value", pty)],
                        ) {
                            m.add_function(func);
                        }
                    }
                    // Auto setter: synthesize `set_{Name}(this, value)` writing
                    // `value` into the backing field. `this` is Param(0), the
                    // implicit `value` is Param(1).
                    if matches!(acc.kind, AccessorKind::Set)
                        && matches!(acc.body, MethodBody::None)
                        && let (Some(oid), Some(idx)) = (owner_id, bidx)
                    {
                        let pty = lower_ty_env(ty, src, structs, env);
                        let mut fb = FunctionBuilder::new(
                            format!("{new_prefix}set_{nm}"),
                            vec![
                                IrParam {
                                    name: Some("this".to_string()),
                                    ty: IrType::Ref(oid),
                                },
                                IrParam {
                                    name: Some("value".to_string()),
                                    ty: pty,
                                },
                            ],
                            IrType::Void,
                        );
                        let p = fb.field_addr(Value::Param(0), oid, idx);
                        fb.store(p, Value::Param(1));
                        fb.ret(None);
                        m.add_function(fb.finish());
                    }
                }
            }
            _ => {} // fields / enum-cases — later
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
    env: TyEnv,
    extra: &[(&str, IrType)],
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
        ty: lower_ty_env(&p.ty, src, structs, env),
    }));
    // Pre-named params with no source span (e.g. a setter's implicit `value`).
    ir_params.extend(extra.iter().map(|(n, t)| IrParam {
        name: Some(n.to_string()),
        ty: *t,
    }));

    let fb = FunctionBuilder::new(full_name, ir_params, ret);
    let mut lw = Lowerer::new(fb, ret, methods, structs, env);

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
            let ty = lower_ty_env(&p.ty, src, structs, env);
            let elem = pointer_elem_env(&p.ty, src, structs, env);
            let slot = lw.fb.alloca(ty);
            lw.fb.store(slot.clone(), Value::Param((i + base) as u32));
            lw.bind(nm.text(src), slot, ty, elem);
            // A `function R(P)`-typed *parameter* is callable: record its
            // signature (under the monomorph env) so `name(args)` in the body
            // lowers to an indirect call. This is what lets a higher-order
            // method like `Map(self, f)` actually call its `f`. Bare code
            // pointer — fine for non-capturing-lambda / method-ref arguments;
            // passing a *closure* needs the uniform function-value repr (a
            // follow-on), so closures aren't recorded here.
            if let AstType::Function {
                return_ty,
                params: fps,
                ..
            } = &p.ty
            {
                let fret = lower_ty_env(return_ty, src, structs, env);
                let fptys: Vec<IrType> = fps
                    .iter()
                    .map(|t| lower_ty_env(t, src, structs, env))
                    .collect();
                lw.fn_sigs.insert(nm.text(src).to_string(), (fret, fptys));
            }
        }
    }
    // Bind pre-named extra params (no source span): their `Param` index follows
    // `this` and the explicit AstParams.
    for (j, (name, ty)) in extra.iter().enumerate() {
        let slot = lw.fb.alloca(*ty);
        lw.fb
            .store(slot.clone(), Value::Param((base + params.len() + j) as u32));
        lw.bind(name, slot, *ty, None);
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

/// Emit a *capturing* lambda as a closure function `$lambdaN($self, params…) ->
/// ret`. `$self` (param 0) is the env pointer `[code_ptr | cap0 | cap1 …]`; each
/// capture binds to its env slot `$self[i+1]` (reads/writes flow through that
/// address); the lambda's own params follow and spill to slots as usual.
#[allow(clippy::too_many_arguments)]
fn emit_closure(
    name: &str,
    ret: IrType,
    params: &[(String, IrType)],
    caps: &[(String, IrType)],
    body: &MethodBody,
    src: &str,
    structs: &StructTable,
) -> Option<Function> {
    let body_stmt = match body {
        MethodBody::None => return None,
        other => other,
    };
    let mut ir_params: Vec<IrParam> = vec![IrParam {
        name: Some("$self".to_string()),
        ty: IrType::Ptr,
    }];
    ir_params.extend(params.iter().map(|(n, t)| IrParam {
        name: Some(n.clone()),
        ty: *t,
    }));
    let fb = FunctionBuilder::new(name.to_string(), ir_params, ret);
    let empty: HashMap<String, Vec<MethodSig>> = HashMap::new();
    let mut lw = Lowerer::new(fb, ret, &empty, structs, &[]);

    // Captures: bind each name to its env address `$self[i+1]`.
    let self_env = Value::Param(0);
    for (i, (n, t)) in caps.iter().enumerate() {
        let addr = lw.fb.elem_addr(
            self_env.clone(),
            IrType::Ptr,
            Value::int((i + 1) as i128, IrType::I64),
        );
        lw.bind(n, addr, *t, None);
    }
    // The lambda's params follow `$self` (param indices offset by 1).
    for (i, (n, t)) in params.iter().enumerate() {
        let slot = lw.fb.alloca(*t);
        lw.fb.store(slot.clone(), Value::Param((i + 1) as u32));
        lw.bind(n, slot, *t, None);
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
    /// Generic type-parameter env when lowering a monomorph's body (so a `T`
    /// local declaration resolves to its concrete type). Empty otherwise.
    env: TyEnv<'a>,
    /// Function-pointer locals: name → (return type, parameter types). A
    /// `function R(P)` local holds a code address; a call `f(args)` through it
    /// lowers to an indirect call with this signature.
    fn_sigs: HashMap<String, (IrType, Vec<IrType>)>,
    /// Names of function-pointer locals that hold a *closure* (an env pointer
    /// whose slot 0 is the code pointer), not a bare code pointer. A call
    /// through one passes the env as a hidden first argument.
    closures: std::collections::HashSet<String>,
}

impl<'a> Lowerer<'a> {
    fn new(
        fb: FunctionBuilder,
        ret_ty: IrType,
        methods: &'a HashMap<String, Vec<MethodSig>>,
        structs: &'a StructTable,
        env: TyEnv<'a>,
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
            env,
            fn_sigs: HashMap::new(),
            closures: std::collections::HashSet::new(),
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
                    Some(t) => lower_ty_env(t, src, self.structs, self.env),
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
                // Record the element type for a typed-pointer local (`T* p`),
                // resolving `T` through the monomorph env.
                let elem = ty
                    .as_ref()
                    .and_then(|t| pointer_elem_env(t, src, self.structs, self.env));
                self.bind(name.text(src), slot, slot_ty, elem);
                // A `function R(P)` local is a code pointer (slot type `Ptr`);
                // record its signature so a later `name(args)` can lower to an
                // indirect call with the right return type + arg coercions.
                if let Some(AstType::Function {
                    return_ty, params, ..
                }) = ty
                {
                    let ret = lower_ty_env(return_ty, src, self.structs, self.env);
                    let ptys: Vec<IrType> = params
                        .iter()
                        .map(|p| lower_ty_env(p, src, self.structs, self.env))
                        .collect();
                    self.fn_sigs.insert(name.text(src).to_string(), (ret, ptys));
                    // If the initializer is a *capturing* lambda, this local
                    // holds a closure (env pointer) — a call through it passes
                    // the env as a hidden first arg. (The lambda's captures were
                    // recorded by name when its `Expr::Lambda` was lowered above.)
                    if let Some(Expr::Lambda { span, .. }) = init
                        && let Some(lname) = self.structs.lambda_names.get(span)
                        && self
                            .structs
                            .lambda_captures
                            .borrow()
                            .get(lname)
                            .is_some_and(|c| !c.is_empty())
                    {
                        self.closures.insert(name.text(src).to_string());
                    }
                }
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
            Stmt::ForEach {
                name, iter, body, ..
            } => {
                // Two iterable shapes lower to counting loops (both with
                // `break`/`continue` wired to the loop stack): a numeric range
                // `lo..<hi` / `lo...hi`, or a List-like receiver with
                // `Count() -> int` + `Get(int) -> T`. Anything else degrades to a
                // skipped body.
                if let Expr::Binary {
                    op: rop @ (AstBin::Range | AstBin::ClosedRange),
                    lhs,
                    rhs,
                    ..
                } = iter
                {
                    // `for (var i in lo..<hi)` → `for (i = lo; i </<= hi; i += 1)`.
                    let (lo, lot) = self.expr(lhs, src);
                    let (hi, hit) = self.expr(rhs, src);
                    let ity = common_type(lot, hit);
                    let lo = self.coerce(lo, lot, ity);
                    let hi = self.coerce(hi, hit, ity);
                    self.scopes.push(HashMap::new());
                    let hi_slot = self.fb.alloca(ity);
                    self.fb.store(hi_slot.clone(), hi);
                    let var_slot = self.fb.alloca(ity);
                    self.fb.store(var_slot.clone(), lo);
                    self.bind(name.text(src), var_slot.clone(), ity, None);
                    let head = self.fb.create_block("foreach.head");
                    let body_b = self.fb.create_block("foreach.body");
                    let cont = self.fb.create_block("foreach.cont");
                    let exit = self.fb.create_block("foreach.exit");
                    self.fb.br(head);
                    self.switch(head);
                    let iv = self.fb.load(var_slot.clone(), ity);
                    let hv = self.fb.load(hi_slot.clone(), ity);
                    let pred = if matches!(rop, AstBin::ClosedRange) {
                        CmpPred::Sle
                    } else {
                        CmpPred::Slt
                    };
                    let test = self.fb.cmp(pred, iv, hv);
                    self.fb.cond_br(test, body_b, exit);
                    self.terminated = true;
                    self.switch(body_b);
                    self.loops.push((cont, exit));
                    self.stmt(body, src);
                    self.loops.pop();
                    if !self.terminated {
                        self.fb.br(cont);
                    }
                    self.switch(cont);
                    let iv = self.fb.load(var_slot.clone(), ity);
                    let inc = self.fb.bin(IrBin::Add, iv, Value::int(1, ity), ity);
                    self.fb.store(var_slot, inc);
                    self.fb.br(head);
                    self.switch(exit);
                    self.scopes.pop();
                    return;
                }
                // `for (name in coll)` over a List-like receiver — one with
                // `Count() -> int` and `Get(int) -> T` (e.g. corlib `List<T>`).
                // Lowered as an indexed loop: `for (i = 0; i < coll.Count();
                // i += 1) { name = coll.Get(i); body }`. A non-iterable
                // collection (no Count/Get) degrades to a skipped body.
                let (coll, coll_ty) = self.expr(iter, src);
                let sigs = if let IrType::Ref(id) = coll_ty {
                    let count = self.structs.methods[id.0 as usize]
                        .get("Count")
                        .and_then(|c| pick_overload(c, &[], true))
                        .cloned();
                    let get = self.structs.methods[id.0 as usize]
                        .get("Get")
                        .and_then(|c| pick_overload(c, &[IrType::I64], true))
                        .cloned();
                    count.zip(get)
                } else {
                    None
                };
                if let Some((count_sig, get_sig)) = sigs {
                    let idx_ty = get_sig.params[1];
                    let elem_ty = get_sig.ret;
                    self.scopes.push(HashMap::new());
                    // Evaluate the collection once; index + loop-var slots.
                    let coll_slot = self.fb.alloca(coll_ty);
                    self.fb.store(coll_slot.clone(), coll);
                    let idx_slot = self.fb.alloca(idx_ty);
                    self.fb.store(idx_slot.clone(), Value::int(0, idx_ty));
                    let var_slot = self.fb.alloca(elem_ty);
                    self.bind(name.text(src), var_slot.clone(), elem_ty, None);
                    let head = self.fb.create_block("foreach.head");
                    let body_b = self.fb.create_block("foreach.body");
                    let cont = self.fb.create_block("foreach.cont");
                    let exit = self.fb.create_block("foreach.exit");
                    self.fb.br(head);
                    // head: i < coll.Count()
                    self.switch(head);
                    let cv = self.fb.load(coll_slot.clone(), coll_ty);
                    let cnt = self
                        .fb
                        .call(count_sig.full_name.clone(), vec![cv], count_sig.ret);
                    let cnt = self.coerce(cnt, count_sig.ret, idx_ty);
                    let iv = self.fb.load(idx_slot.clone(), idx_ty);
                    let test = self.fb.cmp(CmpPred::Slt, iv, cnt);
                    self.fb.cond_br(test, body_b, exit);
                    self.terminated = true;
                    // body: name = coll.Get(i); <body>
                    self.switch(body_b);
                    let cv = self.fb.load(coll_slot.clone(), coll_ty);
                    let iv = self.fb.load(idx_slot.clone(), idx_ty);
                    let elem = self
                        .fb
                        .call(get_sig.full_name.clone(), vec![cv, iv], elem_ty);
                    self.fb.store(var_slot.clone(), elem);
                    self.loops.push((cont, exit));
                    self.stmt(body, src);
                    self.loops.pop();
                    if !self.terminated {
                        self.fb.br(cont);
                    }
                    // cont: i += 1; back to head
                    self.switch(cont);
                    let iv = self.fb.load(idx_slot.clone(), idx_ty);
                    let inc = self.fb.bin(IrBin::Add, iv, Value::int(1, idx_ty), idx_ty);
                    self.fb.store(idx_slot, inc);
                    self.fb.br(head);
                    self.switch(exit);
                    self.scopes.pop();
                }
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
                // Payload-enum `match`: a discriminant test per arm + payload
                // binding, instead of the scalar value-equality chain below.
                if let IrType::Struct(eid) = st
                    && self.structs.enum_cases.contains_key(&eid)
                {
                    self.lower_enum_match(sv, eid, arms, src);
                    return;
                }
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

    // ── closure capture detection ─────────────────────────────────────────

    /// Find the outer locals a lambda body references (its free variables) — the
    /// closure captures — as `(name, slot, type)` read from the live enclosing
    /// scope. Excludes the lambda's own params and any locals the body declares.
    fn detect_captures(
        &self,
        body: &Stmt,
        params: &[Span],
        src: &str,
    ) -> Vec<(String, Value, IrType)> {
        let mut bound: HashSet<String> = params.iter().map(|p| p.text(src).to_string()).collect();
        let mut seen: HashSet<String> = HashSet::new();
        let mut caps: Vec<(String, Value, IrType)> = Vec::new();
        self.caps_stmt(body, src, &mut bound, &mut seen, &mut caps);
        caps
    }

    fn caps_stmt(
        &self,
        s: &Stmt,
        src: &str,
        bound: &mut HashSet<String>,
        seen: &mut HashSet<String>,
        caps: &mut Vec<(String, Value, IrType)>,
    ) {
        match s {
            Stmt::Block { stmts, .. } => {
                for st in stmts {
                    self.caps_stmt(st, src, bound, seen, caps);
                }
            }
            Stmt::Local { name, init, .. } => {
                if let Some(e) = init {
                    self.caps_expr(e, src, bound, seen, caps);
                }
                bound.insert(name.text(src).to_string());
            }
            Stmt::Expr { expr, .. } => self.caps_expr(expr, src, bound, seen, caps),
            Stmt::Return { value: Some(e), .. } => self.caps_expr(e, src, bound, seen, caps),
            Stmt::If {
                cond, then, els, ..
            } => {
                self.caps_expr(cond, src, bound, seen, caps);
                self.caps_stmt(then, src, bound, seen, caps);
                if let Some(e) = els {
                    self.caps_stmt(e, src, bound, seen, caps);
                }
            }
            Stmt::While { cond, body, .. } | Stmt::DoWhile { cond, body, .. } => {
                self.caps_expr(cond, src, bound, seen, caps);
                self.caps_stmt(body, src, bound, seen, caps);
            }
            Stmt::For {
                init,
                cond,
                update,
                body,
                ..
            } => {
                if let Some(i) = init {
                    self.caps_stmt(i, src, bound, seen, caps);
                }
                if let Some(c) = cond {
                    self.caps_expr(c, src, bound, seen, caps);
                }
                if let Some(u) = update {
                    self.caps_expr(u, src, bound, seen, caps);
                }
                self.caps_stmt(body, src, bound, seen, caps);
            }
            Stmt::ForEach {
                name, iter, body, ..
            } => {
                self.caps_expr(iter, src, bound, seen, caps);
                bound.insert(name.text(src).to_string());
                self.caps_stmt(body, src, bound, seen, caps);
            }
            _ => {}
        }
    }

    fn caps_expr(
        &self,
        e: &Expr,
        src: &str,
        bound: &mut HashSet<String>,
        seen: &mut HashSet<String>,
        caps: &mut Vec<(String, Value, IrType)>,
    ) {
        match e {
            Expr::Ident(s) => {
                let n = s.text(src);
                if !bound.contains(n)
                    && !seen.contains(n)
                    && let Some((slot, ty)) = self.lookup(n)
                {
                    seen.insert(n.to_string());
                    caps.push((n.to_string(), slot, ty));
                }
            }
            Expr::Paren { inner, .. } => self.caps_expr(inner, src, bound, seen, caps),
            Expr::Unary { operand, .. }
            | Expr::Prefix { operand, .. }
            | Expr::PostInc { operand, .. }
            | Expr::PostDec { operand, .. } => self.caps_expr(operand, src, bound, seen, caps),
            Expr::Member { base, .. } => self.caps_expr(base, src, bound, seen, caps),
            Expr::Binary { lhs, rhs, .. } => {
                self.caps_expr(lhs, src, bound, seen, caps);
                self.caps_expr(rhs, src, bound, seen, caps);
            }
            Expr::Assign { target, value, .. } => {
                self.caps_expr(target, src, bound, seen, caps);
                self.caps_expr(value, src, bound, seen, caps);
            }
            Expr::Ternary {
                cond, then, els, ..
            } => {
                self.caps_expr(cond, src, bound, seen, caps);
                self.caps_expr(then, src, bound, seen, caps);
                self.caps_expr(els, src, bound, seen, caps);
            }
            Expr::Cast { operand, .. } => self.caps_expr(operand, src, bound, seen, caps),
            Expr::Call { callee, args, .. }
            | Expr::Index {
                base: callee, args, ..
            } => {
                self.caps_expr(callee, src, bound, seen, caps);
                for a in args {
                    self.caps_expr(a, src, bound, seen, caps);
                }
            }
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
            // `sizeof(T)` → the type's byte size, an `int` (I64). A value struct
            // defers to the IR `SizeOf` (LLVM's DataLayout — the same size `new`
            // allocates); scalars and references are constant-sized (a class
            // reference is pointer-sized).
            Expr::SizeOf { ty, .. } => {
                let it = lower_ty_env(ty, src, self.structs, self.env);
                let sz = match it {
                    IrType::Struct(id) => self.fb.size_of(id),
                    IrType::Bool => Value::int(1, IrType::I64),
                    IrType::Int { bits, .. } => Value::int((bits / 8) as i128, IrType::I64),
                    IrType::Float { bits } => Value::int((bits / 8) as i128, IrType::I64),
                    IrType::Ptr | IrType::Ref(_) => Value::int(8, IrType::I64),
                    IrType::Void => Value::int(0, IrType::I64),
                };
                (sz, IrType::I64)
            }
            // An anonymous lambda lowers to the address of the free function it
            // was emitted as. If it captures outer locals it becomes a *closure*:
            // allocate a heap env `[code_ptr | cap0 | cap1 …]` (8-byte slots),
            // store the code pointer + each captured value, and the lambda value
            // is the env pointer. Non-capturing ⇒ the bare code pointer. The
            // captures (name, type) are recorded for the emit pass.
            Expr::Lambda {
                span, params, body, ..
            } => {
                let Some(name) = self.structs.lambda_names.get(span).cloned() else {
                    return (undef(IrType::I64), IrType::I64);
                };
                let caps = self.detect_captures(body, params, src);
                self.structs.lambda_captures.borrow_mut().insert(
                    name.clone(),
                    caps.iter().map(|(n, _, t)| (n.clone(), *t)).collect(),
                );
                let code = self.fb.global_addr(name);
                if caps.is_empty() {
                    return (code, IrType::Ptr);
                }
                let words = (1 + caps.len()) as i128;
                let env = self.fb.call(
                    "malloc",
                    vec![Value::int(words * 8, IrType::I64)],
                    IrType::Ptr,
                );
                let slot0 = self
                    .fb
                    .elem_addr(env.clone(), IrType::Ptr, Value::int(0, IrType::I64));
                self.fb.store(slot0, code);
                for (i, (_n, slot, ty)) in caps.iter().enumerate() {
                    let dst = self.fb.elem_addr(
                        env.clone(),
                        IrType::Ptr,
                        Value::int((i + 1) as i128, IrType::I64),
                    );
                    let val = self.fb.load(slot.clone(), *ty);
                    self.fb.store(dst, val);
                }
                (env, IrType::Ptr)
            }
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
            Expr::Binary {
                op: AstBin::Case,
                lhs,
                rhs,
                ..
            } => self.case_test(lhs, rhs, src),
            Expr::Binary { op, lhs, rhs, .. } => self.binary(*op, lhs, rhs, src),
            Expr::Ternary {
                cond, then, els, ..
            } => self.ternary(cond, then, els, src),
            Expr::Assign {
                op, target, value, ..
            } => self.assign(*op, target, value, src),
            Expr::Call { callee, args, .. } => {
                // Generic-method call `Name<Args>(args)`: resolve to the mangled
                // monomorph emitted during lowering and call it directly.
                if let Expr::Generic {
                    base, args: targs, ..
                } = &**callee
                    && let Expr::Ident(s) = &**base
                {
                    let argtys: Vec<IrType> = targs
                        .iter()
                        .map(|a| lower_ty_env(a, src, self.structs, self.env))
                        .collect();
                    let mangled = mangle_generic(s.text(src), &argtys);
                    if let Some(sig) = self.structs.gen_method_sigs.get(&mangled).cloned()
                        && sig.params.len() == args.len()
                    {
                        let call_args: Vec<Value> = args
                            .iter()
                            .enumerate()
                            .map(|(i, a)| {
                                let (v, ty) = self.expr(a, src);
                                self.coerce(v, ty, sig.params[i])
                            })
                            .collect();
                        let r = self.fb.call(sig.full_name.clone(), call_args, sig.ret);
                        return (r, sig.ret);
                    }
                }
                // Method call on a receiver: `obj.Method(args)` / `this.M(args)`.
                if let Expr::Member { base, name, .. } = &**callee {
                    // `Enum.Case(payload)` for a payload enum constructs its struct.
                    if let Some(r) = self.try_enum_construct(base, name.text(src), args, src) {
                        return r;
                    }
                    return self.lower_method_call(base, name.text(src), args, src);
                }
                // Target-typed `.Case(payload)` — a payload-enum case shorthand.
                if let Expr::DotIdent { name, .. } = &**callee
                    && let Some(r) = self.try_enum_construct_dot(name.text(src), args, src)
                {
                    return r;
                }
                let arg_vals: Vec<(Value, IrType)> =
                    args.iter().map(|a| self.expr(a, src)).collect();
                if let Expr::Ident(s) = &**callee {
                    let name = s.text(src);
                    // A function-pointer local (`function R(P) f`): `f(args)`
                    // loads the code pointer and calls it indirectly.
                    if let Some((ret, ptys)) = self.fn_sigs.get(name).cloned()
                        && let Some((slot, _)) = self.lookup(name)
                    {
                        let is_closure = self.closures.contains(name);
                        // `f`'s value is the env pointer for a closure, or the
                        // code pointer for a bare function pointer.
                        let f = self.fb.load(slot, IrType::Ptr);
                        let (fptr, env) = if is_closure {
                            // Code pointer is env slot 0; the env is the hidden
                            // first argument (`$self`).
                            let code_slot = self.fb.elem_addr(
                                f.clone(),
                                IrType::Ptr,
                                Value::int(0, IrType::I64),
                            );
                            (self.fb.load(code_slot, IrType::Ptr), Some(f))
                        } else {
                            (f, None)
                        };
                        let mut call_args: Vec<Value> = Vec::new();
                        if let Some(env) = env {
                            call_args.push(env);
                        }
                        for (i, (v, t)) in arg_vals.into_iter().enumerate() {
                            let pt = ptys.get(i).copied().unwrap_or(t);
                            call_args.push(self.coerce(v, t, pt));
                        }
                        return (self.fb.call_indirect(fptr, call_args, ret), ret);
                    }
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
            Expr::Member { base, name, .. } => {
                // A payloadless payload-enum case (`IntOpt.None`) constructs its
                // tagged-union struct; a plain int-backed `Enum.Case` is a constant.
                if let Some(r) = self.try_enum_construct(base, name.text(src), &[], src) {
                    r
                } else if let Some(r) = self.try_enum_const(base, name.text(src), src) {
                    r
                } else {
                    match self.lvalue(e, src) {
                        Some((ptr, ty)) => (self.fb.load(ptr, ty), ty),
                        // Not a storable field — try a computed property getter,
                        // then a `Type.StaticMethod` reference (a function pointer).
                        None => {
                            if let Some(r) = self.try_property_get(base, name.text(src), src) {
                                r
                            } else if let Some(r) = self.try_method_ref(base, name.text(src), src) {
                                r
                            } else {
                                (undef(IrType::I64), IrType::I64)
                            }
                        }
                    }
                }
            }
            // Index read (`p[i]`): load the element at the computed address.
            Expr::Index { .. } => match self.lvalue(e, src) {
                Some((ptr, ty)) => (self.fb.load(ptr, ty), ty),
                None => (undef(IrType::I64), IrType::I64),
            },
            // Bare `.Case` (a `DotIdent`) — a payloadless payload-enum case
            // shorthand (`IntOpt x = .None`). Constructs the unique owning enum.
            Expr::DotIdent { name, .. } => self
                .try_enum_construct_dot(name.text(src), &[], src)
                .unwrap_or((undef(IrType::I64), IrType::I64)),
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

    /// `cond ? then : els` — short-circuiting (only the taken branch runs).
    /// Lowers like an `if`/`else` joined by a phi of the two results, each
    /// coerced (in its own predecessor block) to the common result type.
    fn ternary(&mut self, cond: &Expr, then: &Expr, els: &Expr, src: &str) -> (Value, IrType) {
        let (cv, cvt) = self.expr(cond, src);
        let cond_v = self.coerce_bool(cv, cvt);
        let then_b = self.fb.create_block("tern.then");
        let else_b = self.fb.create_block("tern.else");
        let join_b = self.fb.create_block("tern.join");
        self.fb.cond_br(cond_v, then_b, else_b);

        // Evaluate each arm in its block (an arm may itself branch, so the phi
        // predecessor is the block the arm *ends* in).
        self.switch(then_b);
        let (av, at) = self.expr(then, src);
        let then_end = self.fb.current_block();
        self.switch(else_b);
        let (bv, bt) = self.expr(els, src);
        let else_end = self.fb.current_block();

        // Coerce each result to the common type in its own block, then branch.
        let ty = common_type(at, bt);
        self.fb.switch_to(then_end);
        let a2 = self.coerce(av, at, ty);
        self.fb.br(join_b);
        self.fb.switch_to(else_end);
        let b2 = self.coerce(bv, bt, ty);
        self.fb.br(join_b);

        self.switch(join_b);
        let r = self.fb.phi(vec![(then_end, a2), (else_end, b2)], ty);
        (r, ty)
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
            AstBin::Eq | AstBin::Ne
                if matches!(lt, IrType::Ref(_)) && matches!(rt, IrType::Ref(_)) =>
            {
                // Value equality for class references that define `Equals(Self)`
                // (e.g. String): `a == b` → `a.Equals(b)`, `a != b` → its
                // negation. Reference identity (and null comparison, where one
                // side isn't a `Ref`) falls through to the scalar compare below.
                let IrType::Ref(id) = lt else { unreachable!() };
                let eq = self.structs.methods[id.0 as usize]
                    .get("Equals")
                    .and_then(|c| pick_overload(c, &[rt], true))
                    .cloned();
                if let Some(eq) = eq {
                    let other = self.coerce(r, rt, eq.params[1]);
                    let res = self.fb.call(eq.full_name, vec![l, other], IrType::Bool);
                    let res = if matches!(op, AstBin::Ne) {
                        self.fb.cmp(CmpPred::Eq, res, Value::bool(false))
                    } else {
                        res
                    };
                    (res, IrType::Bool)
                } else {
                    let ct = common_type(lt, rt);
                    let l = self.coerce(l, lt, ct);
                    let r = self.coerce(r, rt, ct);
                    (self.fb.cmp(cmp_pred(op, ct), l, r), IrType::Bool)
                }
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
                // A `static` field is a mutable global addressed as `Type.Field`.
                // Resolving it here — before the instance-field path — makes
                // reads, plain assignment, and compound assignment all work,
                // since `lvalue` powers all three. Falls through to the
                // instance path when the symbol isn't a registered static.
                if let Expr::Ident(s) = &**base {
                    let sym = format!("{}.{}", s.text(src), name.text(src));
                    if let Some(&ty) = self.structs.statics.get(&sym) {
                        return Some((self.fb.global_addr(sym), ty));
                    }
                }
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

    /// `Enum.Case` where `Enum` is a registered enum and `Case` is one of its
    /// members → the constant int32 value.
    fn try_enum_const(&self, base: &Expr, name: &str, src: &str) -> Option<(Value, IrType)> {
        if let Expr::Ident(s) = base
            && let Some(cases) = self.structs.enums.get(s.text(src))
            && let Some(&v) = cases.get(name)
        {
            return Some((
                Value::int(
                    v as i128,
                    IrType::Int {
                        bits: 32,
                        signed: true,
                    },
                ),
                IrType::Int {
                    bits: 32,
                    signed: true,
                },
            ));
        }
        None
    }

    /// `Enum.Case(args)` (or payloadless `Enum.Case`) for a *payload* enum →
    /// construct its tagged-union struct: store the case discriminant in `$disc`
    /// and each argument in `$p{i}` (coerced to that case's declared payload
    /// type), then load the struct value. `None` if it isn't a payload-enum case.
    /// The unique payload enum with a case named `case` → `(struct id,
    /// discriminant, payload field types)`. `None` if no payload enum — or more
    /// than one — has that case, so a target-typed `.Case(args)` is constructed
    /// only when unambiguous. (A qualified `Enum.Case` always resolves by name.)
    fn payload_enum_for_case(&self, case: &str) -> Option<(StructId, i64, Vec<IrType>)> {
        let mut found = None;
        for (id, cases) in &self.structs.enum_cases {
            if let Some((_, disc, ptys)) = cases.iter().find(|(n, _, _)| n == case) {
                if found.is_some() {
                    return None; // ambiguous across enums
                }
                found = Some((*id, *disc, ptys.clone()));
            }
        }
        found
    }

    /// Build a payload-enum value: alloca its struct, store the discriminant in
    /// `$disc` and each argument in `$p{i}` (coerced to that case's payload type),
    /// then load the aggregate.
    fn build_enum_value(
        &mut self,
        id: StructId,
        disc: i64,
        ptys: &[IrType],
        args: &[Expr],
        src: &str,
    ) -> (Value, IrType) {
        let sty = IrType::Struct(id);
        let slot = self.fb.alloca(sty);
        let disc_addr = self.fb.field_addr(slot.clone(), id, 0);
        self.fb.store(
            disc_addr,
            Value::int(
                disc as i128,
                IrType::Int {
                    bits: 32,
                    signed: true,
                },
            ),
        );
        for (i, a) in args.iter().enumerate() {
            let (v, t) = self.expr(a, src);
            let want = ptys.get(i).copied().unwrap_or(t);
            let cv = self.coerce(v, t, want);
            let fa = self.fb.field_addr(slot.clone(), id, (1 + i) as u32);
            self.fb.store(fa, cv);
        }
        let val = self.fb.load(slot, sty);
        (val, sty)
    }

    /// Qualified `Enum.Case(args)` (base is the enum's name) → construct it.
    /// `None` if `base.case` isn't a payload-enum case.
    fn try_enum_construct(
        &mut self,
        base: &Expr,
        case: &str,
        args: &[Expr],
        src: &str,
    ) -> Option<(Value, IrType)> {
        // `Enum.Case` (base = the enum's name) or `Enum<T>.Case` (base = a generic
        // application) → the key into `payload_enums` is the (possibly mangled) name.
        let key = match base {
            Expr::Ident(s) => s.text(src).to_string(),
            Expr::Generic {
                base: gbase, args, ..
            } => {
                let Expr::Ident(s) = &**gbase else {
                    return None;
                };
                let argtys: Vec<IrType> = args
                    .iter()
                    .map(|a| lower_ty_env(a, src, self.structs, self.env))
                    .collect();
                mangle_generic(s.text(src), &argtys)
            }
            _ => return None,
        };
        let id = *self.structs.payload_enums.get(&key)?;
        let (disc, ptys) = {
            let cases = self.structs.enum_cases.get(&id)?;
            let (_, disc, ptys) = cases.iter().find(|(n, _, _)| n == case)?;
            (*disc, ptys.clone())
        };
        Some(self.build_enum_value(id, disc, &ptys, args, src))
    }

    /// Target-typed `.Case(args)` / bare `.Case` (a `DotIdent`, no enum-name
    /// prefix) → construct the unique payload enum owning that case. `None` if the
    /// case is unknown or ambiguous.
    fn try_enum_construct_dot(
        &mut self,
        case: &str,
        args: &[Expr],
        src: &str,
    ) -> Option<(Value, IrType)> {
        let (id, disc, ptys) = self.payload_enum_for_case(case)?;
        Some(self.build_enum_value(id, disc, &ptys, args, src))
    }

    /// Lower a `match`/`switch` whose scrutinee is a payload-enum struct: spill the
    /// value so its fields are addressable, load its `$disc`, then test each arm's
    /// case discriminant; in a matched arm, bind its payload fields (`$p{j}`) to the
    /// pattern's binding names before running the body. Mirrors the value-switch's
    /// block + `break` structure (a patternless arm is the default; no fallthrough).
    fn lower_enum_match(&mut self, sv: Value, id: StructId, arms: &[SwitchArm], src: &str) {
        let i32t = IrType::Int {
            bits: 32,
            signed: true,
        };
        let sty = IrType::Struct(id);
        let slot = self.fb.alloca(sty);
        self.fb.store(slot.clone(), sv);
        let disc_addr = self.fb.field_addr(slot.clone(), id, 0);
        let disc = self.fb.load(disc_addr, i32t);

        let exit = self.fb.create_block("match.exit");
        let body_blocks: Vec<BlockId> = (0..arms.len())
            .map(|i| self.fb.create_block(format!("match.case{i}")))
            .collect();
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
            // The discriminant this arm matches — `None` ⇒ not a known case ⇒ never
            // matches (branch straight to the next test).
            let want = enum_pattern(pat, src).and_then(|(case, _)| {
                self.structs
                    .enum_cases
                    .get(&id)
                    .and_then(|cs| cs.iter().find(|(n, _, _)| *n == case).map(|(_, d, _)| *d))
            });
            let last = chain_i + 1 == case_idxs.len();
            let next = if last {
                default_target
            } else {
                self.fb.create_block("match.test")
            };
            if let Some(d) = want {
                let eq = self
                    .fb
                    .cmp(CmpPred::Eq, disc.clone(), Value::int(d as i128, i32t));
                self.fb.cond_br(eq, body_blocks[arm_i], next);
            } else {
                self.fb.br(next);
            }
            self.terminated = true;
            if !last {
                self.switch(next);
            }
        }

        self.loops.push((cont, exit));
        for i in 0..arms.len() {
            self.switch(body_blocks[i]);
            // A fresh scope so a payload binding doesn't leak across arms.
            self.scopes.push(HashMap::new());
            if let Some(pat) = arms[i].pattern.as_ref()
                && let Some((case, binds)) = enum_pattern(pat, src)
            {
                let ptys: Vec<IrType> = self
                    .structs
                    .enum_cases
                    .get(&id)
                    .and_then(|cs| {
                        cs.iter()
                            .find(|(n, _, _)| *n == case)
                            .map(|(_, _, p)| p.clone())
                    })
                    .unwrap_or_default();
                for (j, bspan) in binds.iter().enumerate() {
                    if let Some(&fty) = ptys.get(j) {
                        let fa = self.fb.field_addr(slot.clone(), id, (1 + j) as u32);
                        self.bind(bspan.text(src), fa, fty, None);
                    }
                }
            }
            for s in &arms[i].body {
                self.stmt(s, src);
                if self.terminated {
                    break;
                }
            }
            if !self.terminated {
                self.fb.br(exit);
            }
            self.scopes.pop();
        }
        self.loops.pop();
        self.switch(exit);
    }

    /// `x case .Some(let v)` — a boolean case-test that *also* binds any payload
    /// names into the current scope, so the guarded branch can read them
    /// (`if (x case .Some(let v)) { use v; }`). It's one arm of
    /// [`Self::lower_enum_match`] turned into an expression: store the scrutinee,
    /// compare its discriminant against the named case, and bind each payload
    /// field to its `let`-name. A non-enum scrutinee or an unknown case evaluates
    /// to `false` and binds nothing.
    fn case_test(&mut self, lhs: &Expr, pat: &Expr, src: &str) -> (Value, IrType) {
        let (sv, st) = self.expr(lhs, src);
        // The scrutinee may be the enum *value* (a local/field, `Struct(id)`) or a
        // *pointer* to it (`this` inside an enum method, `Ref(id)`). Resolve both
        // to (enum id, address-of-body): spill a value into a slot; a pointer is
        // already the body address.
        let (id, addr) = match st {
            IrType::Struct(id) => {
                let slot = self.fb.alloca(st);
                self.fb.store(slot.clone(), sv);
                (id, slot)
            }
            IrType::Ref(id) => (id, sv),
            _ => return (Value::bool(false), IrType::Bool),
        };
        if !self.structs.enum_cases.contains_key(&id) {
            return (Value::bool(false), IrType::Bool);
        }
        let Some((case, binds)) = enum_pattern(pat, src) else {
            return (Value::bool(false), IrType::Bool);
        };
        let Some((disc, ptys)) = self
            .structs
            .enum_cases
            .get(&id)
            .and_then(|cs| cs.iter().find(|(n, _, _)| *n == case))
            .map(|(_, d, p)| (*d, p.clone()))
        else {
            return (Value::bool(false), IrType::Bool);
        };
        let i32t = IrType::Int {
            bits: 32,
            signed: true,
        };
        let disc_addr = self.fb.field_addr(addr.clone(), id, 0);
        let cur = self.fb.load(disc_addr, i32t);
        let eq = self
            .fb
            .cmp(CmpPred::Eq, cur, Value::int(disc as i128, i32t));
        // Bind each payload field to its `let`-name. The address is a stable slot,
        // so the binding is valid even when the case doesn't match — reading it is
        // only meaningful in the matched branch (Beef's contract).
        for (j, bspan) in binds.iter().enumerate() {
            if let Some(&fty) = ptys.get(j) {
                let fa = self.fb.field_addr(addr.clone(), id, (1 + j) as u32);
                self.bind(bspan.text(src), fa, fty, None);
            }
        }
        (eq, IrType::Bool)
    }

    /// `Type.StaticMethod` used as a *value* (not called) → a function pointer:
    /// the method's code address as a `Ptr`. Backs `function R(P) f = Type.M;`.
    /// Only a non-instance (static) method qualifies, and `Type` must be a type
    /// name (not a local).
    fn try_method_ref(&mut self, base: &Expr, name: &str, src: &str) -> Option<(Value, IrType)> {
        if let Expr::Ident(s) = base {
            let tyname = s.text(src);
            if self.lookup(tyname).is_none()
                && let Some(&id) = self.structs.by_name.get(tyname)
                && let Some(sig) = self.structs.methods[id.0 as usize]
                    .get(name)
                    .and_then(|c| c.iter().find(|s| !s.is_instance))
            {
                return Some((self.fb.global_addr(sig.full_name.clone()), IrType::Ptr));
            }
        }
        None
    }

    /// `obj.Name` where `Name` is not a field but the receiver's type defines a
    /// `get_Name` instance method (a computed property): emit the getter call.
    /// Uses `struct_base` for the receiver (a *pointer* to the body) — exactly
    /// as `lower_method_call` does — so value-struct receivers pass an address,
    /// not an aggregate value.
    fn try_property_get(&mut self, base: &Expr, name: &str, src: &str) -> Option<(Value, IrType)> {
        let (body_ptr, owner) = self.struct_base(base, src)?;
        let getter = self.structs.methods[owner.0 as usize]
            .get(&format!("get_{name}"))
            .and_then(|c| pick_overload(c, &[], true))?
            .clone();
        let mut call_args = Vec::new();
        if getter.is_instance {
            call_args.push(body_ptr);
        }
        Some((
            self.fb.call(getter.full_name, call_args, getter.ret),
            getter.ret,
        ))
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
    /// The class id a `new` operand constructs: a generic instantiation
    /// (`new Box<int>()`) resolves to its monomorph, otherwise the plain class.
    fn new_class_id(&self, operand: &Expr, src: &str) -> Option<StructId> {
        if let Some((name, args)) = generic_new_parts(operand, src) {
            let argtys: Vec<IrType> = args
                .iter()
                .map(|a| lower_ty_env(a, src, self.structs, self.env))
                .collect();
            let mangled = mangle_generic(name, &argtys);
            if let Some(IrType::Ref(id)) = self.structs.ty_of(&mangled) {
                return Some(id);
            }
        }
        let name = ctor_class_name(operand, src)?;
        match self.structs.ty_of(name) {
            Some(IrType::Ref(id)) => Some(id),
            _ => None,
        }
    }

    fn lower_new(&mut self, operand: &Expr, src: &str) -> (Value, IrType) {
        if let Some(id) = self.new_class_id(operand, src) {
            let size = self.fb.size_of(id);
            let p = self.fb.call("malloc", vec![size], IrType::Ref(id));
            // Object header (ClassVData*) at offset 0: the class vtable when it
            // has virtual methods, else null.
            let hdr = self.fb.field_addr(p.clone(), id, 0);
            let header = if self.structs.vimpls[id.0 as usize].is_empty() {
                Value::Const(Const::Null)
            } else {
                self.fb
                    .global_addr(vtable_name(&self.structs.prefixes[id.0 as usize]))
            };
            self.fb.store(hdr, header);
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
                call_args.push(body_ptr.clone());
                pidx = 1;
            }
            for (v, t) in arg_vals {
                let pt = sig.params.get(pidx).copied().unwrap_or(t);
                call_args.push(self.coerce(v, t, pt));
                pidx += 1;
            }
            // Virtual dispatch: if the method occupies a vtable slot on the
            // receiver's static type, call through the object's `$header` vtable
            // (the runtime type) so an override runs; else a direct call.
            if sig.is_instance
                && let Some(&slot) = self.structs.vslots[owner_id.0 as usize].get(mname)
            {
                let hdr = self.fb.field_addr(body_ptr, owner_id, 0);
                let vtbl = self.fb.load(hdr, IrType::Ptr);
                let slotp =
                    self.fb
                        .elem_addr(vtbl, IrType::Ptr, Value::int(slot as i128, IrType::I64));
                let fnptr = self.fb.load(slotp, IrType::Ptr);
                let r = self.fb.call_indirect(fnptr, call_args, sig.ret);
                return (r, sig.ret);
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
        // Plain `obj.X = v` where `X` is not a field but a computed property
        // with a `set_X` accessor: lower to `set_X(receiver, v)`. Compound
        // assignments (`+=` etc.) don't take this path (no read-back yet).
        if matches!(op, AssignOp::Assign)
            && let Expr::Member { base, name, .. } = target
            && let Some((body_ptr, owner)) = self.struct_base(base, src)
            && let Some(setter) = self.structs.methods[owner.0 as usize]
                .get(&format!("set_{}", name.text(src)))
                .and_then(|cands| pick_overload(cands, &[rhs_ty], true))
                .cloned()
        {
            let pty = *setter.params.last().unwrap();
            let val = self.coerce(rhs, rhs_ty, pty);
            self.fb
                .call(setter.full_name, vec![body_ptr, val.clone()], IrType::Void);
            return (val, pty);
        }
        // Compound assignment on a property: `obj.X op= v` lowers to
        // `set_X(obj, get_X(obj) op v)` (reusing the receiver pointer once). Plain `=`
        // is handled by the setter block above; this fires only for `+=`, `-=`, etc.
        if let Some(astbin) = compound_op(op)
            && let Expr::Member { base, name, .. } = target
            && let Some((body_ptr, owner)) = self.struct_base(base, src)
        {
            let prop = name.text(src);
            let getter = self.structs.methods[owner.0 as usize]
                .get(&format!("get_{prop}"))
                .and_then(|c| pick_overload(c, &[], true))
                .cloned();
            if let Some(getter) = getter {
                let pty = getter.ret;
                let setter = self.structs.methods[owner.0 as usize]
                    .get(&format!("set_{prop}"))
                    .and_then(|c| pick_overload(c, &[pty], true))
                    .cloned();
                if let Some(setter) = setter {
                    let cur = self.fb.call(getter.full_name, vec![body_ptr.clone()], pty);
                    let v = self.coerce(rhs, rhs_ty, pty);
                    let combined = self.arith(astbin, cur, v, pty);
                    self.fb.call(
                        setter.full_name,
                        vec![body_ptr, combined.clone()],
                        IrType::Void,
                    );
                    return (combined, pty);
                }
            }
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
/// For a `new Name<Args>(…)` operand, the generic name and its type arguments
/// (digging through the wrapping `Call`/`Paren`). `None` for a non-generic
/// `new`. Lets `lower_new` resolve the monomorphized class.
fn generic_new_parts<'a>(e: &'a Expr, src: &'a str) -> Option<(&'a str, &'a [AstType])> {
    match e {
        Expr::Paren { inner, .. } => generic_new_parts(inner, src),
        Expr::Call { callee, .. } => generic_new_parts(callee, src),
        Expr::Generic { base, args, .. } => match &**base {
            Expr::Ident(s) => Some((s.text(src), args.as_slice())),
            _ => None,
        },
        _ => None,
    }
}

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

/// The element type of an AST pointer type (`T*` → `T`) for typed indexing,
/// resolving generic type-parameters through `env` so a `T*` field/local in a
/// monomorph strides by `T`'s concrete size (`List<int32>`'s buffer steps by 4,
/// not 8). `None` for non-pointer types.
fn pointer_elem_env(ty: &AstType, src: &str, structs: &StructTable, env: TyEnv) -> Option<IrType> {
    match ty {
        AstType::Pointer { inner, .. } => Some(lower_ty_env(inner, src, structs, env)),
        AstType::Nullable { inner, .. } => pointer_elem_env(inner, src, structs, env),
        _ => None,
    }
}

/// A generic type-parameter environment: param name → the concrete IR type it
/// was monomorphized to. Empty for ordinary (non-generic) lowering.
type TyEnv<'a> = &'a [(String, IrType)];

/// The monomorphized symbol name of a generic instantiation: `Box<int>` →
/// `Box$i64`, `Pair<int32>` → `Pair$i32` (reusing the overload type codes).
fn mangle_generic(name: &str, args: &[IrType]) -> String {
    format!("{name}${}", type_codes(args))
}

/// Lower a type, resolving generic type-parameters through `env` (so a `T`
/// field of a monomorphized `Box<int>` becomes `i64`) and generic
/// instantiations through the monomorphized symbol table (`Box<int>` → the
/// registered `Box$i64`).
fn lower_ty_env(ty: &AstType, src: &str, structs: &StructTable, env: TyEnv) -> IrType {
    match ty {
        AstType::Path { segments, .. } if segments.len() == 1 && segments[0].args.is_empty() => {
            let name = segments[0].name.text(src);
            // A bare type-parameter resolves through the monomorphization env.
            if let Some((_, t)) = env.iter().find(|(n, _)| n.as_str() == name) {
                return *t;
            }
            // An int-backed enum type is just `int32`.
            // A payload-bearing enum lowers to its tagged-union struct; a plain
            // int-backed enum is `int32`.
            if let Some(&id) = structs.payload_enums.get(name) {
                return IrType::Struct(id);
            }
            if structs.enums.contains_key(name) {
                return IrType::Int {
                    bits: 32,
                    signed: true,
                };
            }
            structs.ty_of(name).unwrap_or_else(|| primitive(name))
        }
        // A generic instantiation `Name<Args>` → its monomorphized type.
        AstType::Path { segments, .. } if segments.len() == 1 && !segments[0].args.is_empty() => {
            let name = segments[0].name.text(src);
            let args: Vec<IrType> = segments[0]
                .args
                .iter()
                .map(|a| lower_ty_env(a, src, structs, env))
                .collect();
            let mangled = mangle_generic(name, &args);
            structs.ty_of(&mangled).unwrap_or(IrType::Ptr)
        }
        AstType::Pointer { .. } => IrType::Ptr,
        AstType::Nullable { inner, .. } => lower_ty_env(inner, src, structs, env),
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
