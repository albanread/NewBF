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
    AccessorKind, AssignOp, Attribute, BinOp as AstBin, CompUnit, Expr, InterpPart, Item, Member,
    MethodBody, Modifier, Param as AstParam, ParamModifier, PrefixKw, Stmt, SwitchArm,
    Type as AstType, TypeDecl, TypeKind, UnOp, parse_file,
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
    /// Anonymous tuple types → the synthetic value-struct id backing each
    /// distinct shape, keyed by element `type_codes` (so `(int32, int32)`
    /// everywhere is one struct, fields named "0", "1", …). A pre-pass over
    /// type positions registers them; `lower_ty_env` resolves a `Tuple` here.
    tuples: HashMap<String, StructId>,
    /// Local (nested) functions: the declaration's name span → its emitted free-
    /// function symbol. A pre-pass assigns each `$localfn{N}`; the body lowers it
    /// and a same-method call resolves the name to a direct call.
    local_fn_syms: HashMap<Span, String>,
    /// Per-type field default initializers (`int32 v = 9;`) that are *constant*
    /// literals, keyed by struct id → list of (field name, constant). Applied at
    /// construction (before the constructor body) by name, so they survive
    /// inheritance's field reindexing. Non-constant inits (calls/`new`) aren't
    /// captured yet.
    field_inits: HashMap<StructId, Vec<(String, Const)>>,
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
                &[],
            );
        }
        // 3b. Register a synthetic value struct per distinct tuple shape used in a
        //     type position. Must precede the field/member fill below: a method's
        //     `(int32,int32)` return/param and a tuple field have to resolve to the
        //     struct when their signatures are built, or they'd default to a
        //     pointer. Element types need only names/kinds (step 1) and monos
        //     (step 3), not filled fields, so here is early enough.
        for f in files {
            register_tuples(&f.unit.items, f.src, &mut t);
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

/// Pre-pass: register a synthetic value struct for each distinct tuple shape
/// that appears in a type position, so every `(int32, int32)` resolves to one
/// `Struct(id)` whose fields are named "0", "1", … . Generic *templates* are
/// skipped (their tuples carry unresolved `T`s; monomorphs would need their own
/// registration — a follow-on).
fn register_tuples(items: &[Item], src: &str, t: &mut StructTable) {
    for item in items {
        match item {
            Item::Namespace {
                body: Some(body), ..
            } => register_tuples(body, src, t),
            Item::Type(td) if td.generic_params.is_empty() => register_tuples_in_type(td, src, t),
            _ => {}
        }
    }
}

fn register_tuples_in_type(td: &TypeDecl, src: &str, t: &mut StructTable) {
    for m in &td.members {
        match m {
            Member::Field { ty, .. } => register_tuple_type(ty, src, t),
            Member::Method {
                params,
                return_ty,
                body,
                generic_params,
                ..
            } if generic_params.is_empty() => {
                register_tuple_type(return_ty, src, t);
                for p in params {
                    register_tuple_type(&p.ty, src, t);
                }
                if let MethodBody::Block(s) = body {
                    register_tuples_in_stmt(s, src, t);
                }
            }
            Member::Constructor { params, body, .. } => {
                for p in params {
                    register_tuple_type(&p.ty, src, t);
                }
                if let MethodBody::Block(s) = body {
                    register_tuples_in_stmt(s, src, t);
                }
            }
            Member::Property {
                ty, index_params, ..
            } => {
                register_tuple_type(ty, src, t);
                for p in index_params {
                    register_tuple_type(&p.ty, src, t);
                }
            }
            Member::Nested(n) if n.generic_params.is_empty() => register_tuples_in_type(n, src, t),
            _ => {}
        }
    }
}

/// Walk a statement body for tuple types in local declarations (`(int,int) t;`).
fn register_tuples_in_stmt(stmt: &Stmt, src: &str, t: &mut StructTable) {
    match stmt {
        Stmt::Block { stmts, .. } => {
            for s in stmts {
                register_tuples_in_stmt(s, src, t);
            }
        }
        Stmt::Local { ty: Some(ty), .. } => register_tuple_type(ty, src, t),
        Stmt::Locals { decls, .. } => {
            for d in decls {
                register_tuples_in_stmt(d, src, t);
            }
        }
        Stmt::If { then, els, .. } => {
            register_tuples_in_stmt(then, src, t);
            if let Some(e) = els {
                register_tuples_in_stmt(e, src, t);
            }
        }
        Stmt::While { body, .. } | Stmt::DoWhile { body, .. } => {
            register_tuples_in_stmt(body, src, t)
        }
        Stmt::ForEach { body, .. } => register_tuples_in_stmt(body, src, t),
        Stmt::For {
            init,
            init_extra,
            body,
            ..
        } => {
            if let Some(i) = init {
                register_tuples_in_stmt(i, src, t);
            }
            for s in init_extra {
                register_tuples_in_stmt(s, src, t);
            }
            register_tuples_in_stmt(body, src, t);
        }
        Stmt::Defer { body, .. } => register_tuples_in_stmt(body, src, t),
        _ => {}
    }
}

/// Register the tuple shapes inside `ty` (inner tuples first, so an outer
/// tuple's element type resolves to a concrete `Struct(id)`). A non-tuple
/// composite (`(int,int)*`, `(int,int)[]`) is followed to its element.
fn register_tuple_type(ty: &AstType, src: &str, t: &mut StructTable) {
    match ty {
        AstType::Tuple { elems, .. } => {
            for e in elems {
                register_tuple_type(e, src, t);
            }
            let etys: Vec<IrType> = elems.iter().map(|e| lower_ty_env(e, src, t, &[])).collect();
            let key = type_codes(&etys);
            if t.tuples.contains_key(&key) {
                return;
            }
            let id = register_mono(t, &format!("$tuple${key}"), StructKind::Value);
            for (i, ety) in etys.iter().enumerate() {
                t.defs[id.0 as usize].fields.push(FieldDef {
                    name: i.to_string(),
                    ty: *ety,
                });
                t.field_elems[id.0 as usize].push(None);
            }
            t.tuples.insert(key, id);
        }
        AstType::Pointer { inner, .. }
        | AstType::Nullable { inner, .. }
        | AstType::Array { inner, .. }
        | AstType::Sized { inner, .. } => register_tuple_type(inner, src, t),
        _ => {}
    }
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
#[allow(clippy::too_many_arguments)] // recursive collector: threaded visitor state
fn use_in_type<'a>(
    ty: &AstType,
    src: &'a str,
    generics: &GenericDecls<'a>,
    gmethods: &GenMethodDecls<'a>,
    t: &mut StructTable,
    seen: &mut Vec<String>,
    monos: &mut MonoList<'a>,
    env: &[(String, IrType)],
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
            gmethods,
            t,
            seen,
            monos,
            env,
        );
    }
    if let AstType::Pointer { inner, .. } | AstType::Nullable { inner, .. } = ty {
        use_in_type(inner, src, generics, gmethods, t, seen, monos, env);
    }
}

/// Register the monomorph a `Name<Args>` reference demands (`Box<int>` →
/// `Box$i64`) when `Name` is a known generic and it isn't already recorded,
/// then recurse into the type arguments for nested instantiations. Shared by
/// type-position (`use_in_type`) and expression-position (`collect_insts_expr`)
/// collection.
#[allow(clippy::too_many_arguments)] // recursive collector: threaded visitor state
fn record_inst<'a>(
    name: &str,
    args: &[AstType],
    src: &'a str,
    generics: &GenericDecls<'a>,
    gmethods: &GenMethodDecls<'a>,
    t: &mut StructTable,
    seen: &mut Vec<String>,
    monos: &mut MonoList<'a>,
    env: &[(String, IrType)],
) {
    let Some(&(decl, decl_src)) = generics.get(name) else {
        return;
    };
    // Resolve the type args through the *caller's* env, so a nested `List<T>`
    // inside a `Stack<int32>` body resolves `T → int32` (→ `List$i32`), not the
    // `Ptr` fallback. This is what makes transitive monomorphization correct.
    let argtys: Vec<IrType> = args.iter().map(|a| lower_ty_env(a, src, t, env)).collect();
    let mangled = mangle_generic(name, &argtys);
    if !seen.iter().any(|s| s == &mangled) {
        seen.push(mangled.clone());
        let kind = struct_kind(decl).unwrap_or(StructKind::Value);
        let id = register_mono(t, &mangled, kind);
        let inst_env: Vec<(String, IrType)> = decl
            .generic_params
            .iter()
            .zip(&argtys)
            .map(|(gp, ty)| (gp.name.text(decl_src).to_string(), *ty))
            .collect();
        // Transitively collect the instantiations this mono's *own body* needs,
        // with its concrete env (so `Stack<int32>` drags in `List<int32>`).
        collect_insts_type(
            decl, decl_src, generics, gmethods, t, seen, monos, &inst_env,
        );
        monos.push((id, decl, decl_src, inst_env));
    }
    for a in args {
        use_in_type(a, src, generics, gmethods, t, seen, monos, env);
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
    env: &[(String, IrType)],
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
    let argtys: Vec<IrType> = targs.iter().map(|a| lower_ty_env(a, src, t, env)).collect();
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
            variadic: None,
        },
    );
    t.gen_method_monos.push((mangled, name.to_string(), env));
}

#[allow(clippy::too_many_arguments)] // recursive collector: threaded visitor state
fn collect_insts_items<'a>(
    items: &'a [Item],
    src: &'a str,
    generics: &GenericDecls<'a>,
    gmethods: &GenMethodDecls<'a>,
    t: &mut StructTable,
    seen: &mut Vec<String>,
    monos: &mut MonoList<'a>,
    env: &[(String, IrType)],
) {
    for item in items {
        match item {
            Item::Namespace {
                body: Some(body), ..
            } => collect_insts_items(body, src, generics, gmethods, t, seen, monos, env),
            Item::Type(td) => collect_insts_type(td, src, generics, gmethods, t, seen, monos, env),
            _ => {}
        }
    }
}

#[allow(clippy::too_many_arguments)] // recursive collector: threaded visitor state
fn collect_insts_type<'a>(
    td: &'a TypeDecl,
    src: &'a str,
    generics: &GenericDecls<'a>,
    gmethods: &GenMethodDecls<'a>,
    t: &mut StructTable,
    seen: &mut Vec<String>,
    monos: &mut MonoList<'a>,
    env: &[(String, IrType)],
) {
    for m in &td.members {
        match m {
            Member::Field { ty, .. } => {
                use_in_type(ty, src, generics, gmethods, t, seen, monos, env)
            }
            Member::Method {
                params,
                return_ty,
                body,
                ..
            } => {
                use_in_type(return_ty, src, generics, gmethods, t, seen, monos, env);
                for p in params {
                    use_in_type(&p.ty, src, generics, gmethods, t, seen, monos, env);
                }
                if let MethodBody::Block(s) = body {
                    collect_insts_stmt(s, src, generics, gmethods, t, seen, monos, env);
                }
            }
            Member::Constructor { params, body, .. } => {
                for p in params {
                    use_in_type(&p.ty, src, generics, gmethods, t, seen, monos, env);
                }
                if let MethodBody::Block(s) = body {
                    collect_insts_stmt(s, src, generics, gmethods, t, seen, monos, env);
                }
            }
            Member::Nested(n) => {
                collect_insts_type(n, src, generics, gmethods, t, seen, monos, env)
            }
            _ => {}
        }
    }
}

/// Walk statement bodies for generic instantiations in local-declaration types
/// (`Box<int> b;`). Expression-position instantiations (`new Box<int>()`) arrive
/// with the generic *class* slice.
#[allow(clippy::too_many_arguments)] // recursive collector: threaded visitor state
fn collect_insts_stmt<'a>(
    stmt: &Stmt,
    src: &'a str,
    generics: &GenericDecls<'a>,
    gmethods: &GenMethodDecls<'a>,
    t: &mut StructTable,
    seen: &mut Vec<String>,
    monos: &mut MonoList<'a>,
    env: &[(String, IrType)],
) {
    match stmt {
        Stmt::Block { stmts, .. } => {
            for s in stmts {
                collect_insts_stmt(s, src, generics, gmethods, t, seen, monos, env);
            }
        }
        Stmt::Local { ty, init, .. } => {
            if let Some(ty) = ty {
                use_in_type(ty, src, generics, gmethods, t, seen, monos, env);
            }
            if let Some(e) = init {
                collect_insts_expr(e, src, generics, gmethods, t, seen, monos, env);
            }
        }
        Stmt::Locals { decls, .. } => {
            for d in decls {
                collect_insts_stmt(d, src, generics, gmethods, t, seen, monos, env);
            }
        }
        Stmt::Expr { expr, .. } => {
            collect_insts_expr(expr, src, generics, gmethods, t, seen, monos, env)
        }
        Stmt::Return { value: Some(e), .. } => {
            collect_insts_expr(e, src, generics, gmethods, t, seen, monos, env)
        }
        Stmt::If {
            cond, then, els, ..
        } => {
            collect_insts_expr(cond, src, generics, gmethods, t, seen, monos, env);
            collect_insts_stmt(then, src, generics, gmethods, t, seen, monos, env);
            if let Some(e) = els {
                collect_insts_stmt(e, src, generics, gmethods, t, seen, monos, env);
            }
        }
        Stmt::While { cond, body, .. } | Stmt::DoWhile { body, cond, .. } => {
            collect_insts_expr(cond, src, generics, gmethods, t, seen, monos, env);
            collect_insts_stmt(body, src, generics, gmethods, t, seen, monos, env);
        }
        Stmt::ForEach { iter, body, .. } => {
            collect_insts_expr(iter, src, generics, gmethods, t, seen, monos, env);
            collect_insts_stmt(body, src, generics, gmethods, t, seen, monos, env);
        }
        Stmt::Defer { body, .. } => {
            collect_insts_stmt(body, src, generics, gmethods, t, seen, monos, env)
        }
        Stmt::For {
            init,
            init_extra,
            cond,
            update,
            update_extra,
            body,
            ..
        } => {
            if let Some(i) = init {
                collect_insts_stmt(i, src, generics, gmethods, t, seen, monos, env);
            }
            for s in init_extra {
                collect_insts_stmt(s, src, generics, gmethods, t, seen, monos, env);
            }
            if let Some(c) = cond {
                collect_insts_expr(c, src, generics, gmethods, t, seen, monos, env);
            }
            if let Some(u) = update {
                collect_insts_expr(u, src, generics, gmethods, t, seen, monos, env);
            }
            for u in update_extra {
                collect_insts_expr(u, src, generics, gmethods, t, seen, monos, env);
            }
            collect_insts_stmt(body, src, generics, gmethods, t, seen, monos, env);
        }
        _ => {}
    }
}

/// Walk an expression for generic instantiations in expression position —
/// chiefly `new Name<Args>(…)` (where the `Name<Args>` is an `Expr::Generic`),
/// so an instantiation reaches monomorphization even without a typed local.
#[allow(clippy::too_many_arguments)] // recursive collector: threaded visitor state
fn collect_insts_expr<'a>(
    e: &Expr,
    src: &'a str,
    generics: &GenericDecls<'a>,
    gmethods: &GenMethodDecls<'a>,
    t: &mut StructTable,
    seen: &mut Vec<String>,
    monos: &mut MonoList<'a>,
    env: &[(String, IrType)],
) {
    match e {
        Expr::Generic { base, args, .. } => {
            if let Expr::Ident(s) = &**base {
                record_inst(
                    s.text(src),
                    args,
                    src,
                    generics,
                    gmethods,
                    t,
                    seen,
                    monos,
                    env,
                );
                record_method_inst(s.text(src), args, src, gmethods, t, env);
            }
        }
        Expr::Paren { inner, .. } => {
            collect_insts_expr(inner, src, generics, gmethods, t, seen, monos, env)
        }
        // `sizeof(List<int>)` instantiates the type it names.
        Expr::SizeOf { ty, .. } => use_in_type(ty, src, generics, gmethods, t, seen, monos, env),
        Expr::Unary { operand, .. }
        | Expr::PostInc { operand, .. }
        | Expr::PostDec { operand, .. }
        | Expr::Prefix { operand, .. } => {
            collect_insts_expr(operand, src, generics, gmethods, t, seen, monos, env)
        }
        Expr::Member { base, .. } => {
            collect_insts_expr(base, src, generics, gmethods, t, seen, monos, env)
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_insts_expr(lhs, src, generics, gmethods, t, seen, monos, env);
            collect_insts_expr(rhs, src, generics, gmethods, t, seen, monos, env);
        }
        Expr::Assign { target, value, .. } => {
            collect_insts_expr(target, src, generics, gmethods, t, seen, monos, env);
            collect_insts_expr(value, src, generics, gmethods, t, seen, monos, env);
        }
        Expr::Ternary {
            cond, then, els, ..
        } => {
            collect_insts_expr(cond, src, generics, gmethods, t, seen, monos, env);
            collect_insts_expr(then, src, generics, gmethods, t, seen, monos, env);
            collect_insts_expr(els, src, generics, gmethods, t, seen, monos, env);
        }
        Expr::Call { callee, args, .. }
        | Expr::Index {
            base: callee, args, ..
        } => {
            collect_insts_expr(callee, src, generics, gmethods, t, seen, monos, env);
            for a in args {
                collect_insts_expr(a, src, generics, gmethods, t, seen, monos, env);
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

/// Static byte size of a scalar / pointer / reference IR type, for sizing a
/// heterogeneous payload-enum union slot. `None` for aggregates (`struct`) and
/// `void`, whose size needs the target DataLayout.
fn scalar_size(t: IrType) -> Option<u32> {
    match t {
        IrType::Bool => Some(1),
        IrType::Int { bits, .. } => Some(u32::from(bits) / 8),
        IrType::Float { bits } => Some(u32::from(bits) / 8),
        IrType::Ptr | IrType::Ref(_) => Some(8),
        IrType::Struct(_) | IrType::Void => None,
    }
}

/// The union slot types for a payload enum's cases: max arity across cases, each
/// slot the agreed type (homogeneous position) or the widest scalar member (a
/// heterogeneous position — each case stores/loads its own type, the slot just
/// reserves bytes). `None` if a heterogeneous position holds a `struct` payload,
/// which can't be sized without the target DataLayout (such enums stay int-backed).
fn payload_enum_slots(cases: &[(String, i64, Vec<IrType>)]) -> Option<Vec<IrType>> {
    let maxf = cases.iter().map(|(_, _, p)| p.len()).max().unwrap_or(0);
    let mut slots = Vec::with_capacity(maxf);
    for i in 0..maxf {
        let tys: Vec<IrType> = cases
            .iter()
            .filter_map(|(_, _, p)| p.get(i).copied())
            .collect();
        let Some(&first) = tys.first() else {
            slots.push(IrType::I64);
            continue;
        };
        if tys.iter().all(|&t| t == first) {
            slots.push(first);
        } else if tys.iter().all(|&t| scalar_size(t).is_some()) {
            slots.push(
                tys.iter()
                    .copied()
                    .max_by_key(|&t| scalar_size(t).unwrap())
                    .unwrap(),
            );
        } else {
            return None;
        }
    }
    Some(slots)
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
        // Payload slot layout (`payload_enum_slots`): each position is the agreed
        // type, or the widest scalar for a heterogeneous one. `None` ⇒ a
        // heterogeneous *struct* payload we can't size — keep the enum int-backed.
        if let Some(slots) = payload_enum_slots(&cases) {
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
    // Union slot layout — each position the agreed type or the widest scalar (see
    // `payload_enum_slots`); `None` ⇒ a heterogeneous struct payload (kept int-backed).
    let Some(slots) = payload_enum_slots(&cases) else {
        return;
    };
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
            init,
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
            // A constant field default (`int32 v = 9;`) is recorded by name so it
            // can be applied at construction (after inheritance reindexes fields).
            if let Some(e) = init
                && let Some(c) = const_field_init(e, fty, src)
            {
                t.field_inits
                    .entry(id)
                    .or_default()
                    .push((name.text(src).to_string(), c));
            }
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
                        variadic: None,
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
                    .map(|p| param_ir_ty(p, src, t, env))
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
                // A trailing `params T[]` makes the method variadic: record the
                // element type `T` so a call can pack its overflow args into a `T[]`.
                let variadic = params
                    .last()
                    .filter(|p| matches!(p.modifier, Some((ParamModifier::Params, _))))
                    .and_then(|p| pointer_elem_env(&p.ty, src, t, env));
                let sig = MethodSig {
                    full_name,
                    ret: lower_ty_env(return_ty, src, t, env),
                    params: ps,
                    is_instance,
                    variadic,
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
                index_params,
                ..
            } => {
                // A `get` accessor registers as a `get_{Name}` instance method;
                // reading `obj.Name` calls it. Both computed (body-having) and
                // auto (body-less, backed by the synthesized `{Name}$prop`
                // field) accessors register here — lowering picks the body.
                // An indexer (`this[i]`) registers as `get_this`/`set_this` with
                // its bracket params threaded between `this` and `value`.
                let nm = name.text(src).to_string();
                let is_instance = !modifiers
                    .iter()
                    .any(|(mo, _)| matches!(mo, Modifier::Static));
                let pty = lower_ty_env(ty, src, t, env);
                let idx_tys: Vec<IrType> = index_params
                    .iter()
                    .map(|p| lower_ty_env(&p.ty, src, t, env))
                    .collect();
                for acc in accessors {
                    if matches!(acc.kind, AccessorKind::Get) {
                        let mut ps = Vec::new();
                        if is_instance {
                            ps.push(IrType::Ref(id));
                        }
                        ps.extend(idx_tys.iter().copied());
                        let sig = MethodSig {
                            full_name: format!("{}get_{}", t.prefixes[id.0 as usize], nm),
                            ret: pty,
                            params: ps,
                            is_instance,
                            variadic: None,
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
                        ps.extend(idx_tys.iter().copied());
                        ps.push(pty);
                        let sig = MethodSig {
                            full_name: format!("{}set_{}", t.prefixes[id.0 as usize], nm),
                            ret: IrType::Void,
                            params: ps,
                            is_instance,
                            variadic: None,
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
        Stmt::Locals { decls, .. } => {
            for d in decls {
                collect_lambdas_stmt(d, src, structs, emits);
            }
        }
        _ => {}
    }
}

/// A local (nested) function queued for emission as a free function:
/// `(symbol, return type, params, body, src)`.
type LocalFnEmit<'a> = (String, IrType, &'a [AstParam], &'a Stmt, &'a str);

/// Collect non-generic local functions across all method bodies, assigning each
/// a unique `$localfn{N}` symbol (recorded by name span so the call site and the
/// emit pass agree) and queuing it for emission. Mirrors `collect_lambdas`.
fn collect_local_fns<'a>(
    items: &'a [Item],
    src: &'a str,
    structs: &mut StructTable,
    emits: &mut Vec<LocalFnEmit<'a>>,
) {
    for item in items {
        match item {
            Item::Namespace {
                body: Some(body), ..
            } => collect_local_fns(body, src, structs, emits),
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
                        collect_local_fns_stmt(s, src, structs, emits);
                    }
                }
            }
            _ => {}
        }
    }
}

fn collect_local_fns_stmt<'a>(
    stmt: &'a Stmt,
    src: &'a str,
    structs: &mut StructTable,
    emits: &mut Vec<LocalFnEmit<'a>>,
) {
    match stmt {
        Stmt::Block { stmts, .. } => {
            for s in stmts {
                collect_local_fns_stmt(s, src, structs, emits);
            }
        }
        Stmt::LocalFunction {
            return_ty,
            name,
            generic_params,
            params,
            body,
            ..
        } if generic_params.is_empty() => {
            let sym = format!("$localfn{}", structs.local_fn_syms.len());
            let ret = lower_ty_env(return_ty, src, structs, &[]);
            structs.local_fn_syms.insert(*name, sym.clone());
            emits.push((sym, ret, params, &**body, src));
            // A local function's own body may contain nested locals.
            collect_local_fns_stmt(body, src, structs, emits);
        }
        Stmt::If { then, els, .. } => {
            collect_local_fns_stmt(then, src, structs, emits);
            if let Some(e) = els {
                collect_local_fns_stmt(e, src, structs, emits);
            }
        }
        Stmt::While { body, .. }
        | Stmt::DoWhile { body, .. }
        | Stmt::For { body, .. }
        | Stmt::ForEach { body, .. }
        | Stmt::Defer { body, .. } => collect_local_fns_stmt(body, src, structs, emits),
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
    let mut local_fn_emits: Vec<LocalFnEmit> = Vec::new();
    for f in &all {
        collect_local_fns(&f.unit.items, f.src, &mut structs, &mut local_fn_emits);
    }
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
    // Emit each local (nested) function as a plain free function under its
    // `$localfn{N}` symbol. Non-capturing: the body lowers like a static method
    // with its own params (no access to the enclosing method's locals).
    for (sym, ret, params, body, lsrc) in &local_fn_emits {
        let mb = MethodBody::Block((*body).clone());
        let empty: HashMap<String, Vec<MethodSig>> = HashMap::new();
        if let Some(func) = lower_method(
            sym.clone(),
            *ret,
            params,
            &mb,
            lsrc,
            &empty,
            &structs,
            None,
            &[],
            &[],
        ) {
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
    /// `Some(element type)` if the last explicit parameter is `params T[]`: the
    /// call site packs the trailing arguments into a fresh `T[]` for it.
    variadic: Option<IrType>,
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
        // A `params T[]` method matches any arg count at or above its fixed params
        // (everything past them packs into the `T[]`); a normal one needs an exact
        // count. Only the fixed leading params are scored, and a variadic match
        // takes a flat penalty so an exact non-variadic overload wins a tie.
        let (fixed, penalty) = match c.variadic {
            Some(_) if arg_tys.len() + 1 >= formal.len() => (formal.len() - 1, 1),
            Some(_) => continue,
            None if formal.len() == arg_tys.len() => (formal.len(), 0),
            None => continue,
        };
        let raw: u32 = formal[..fixed]
            .iter()
            .zip(arg_tys)
            .map(|(f, a)| type_affinity(*f, *a))
            .sum();
        let score = raw.saturating_sub(penalty);
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

/// The source symbol of a binary operator, for matching a user-defined
/// `operator <sym>` overload. `None` for operators that don't overload this way
/// in the kernel (`&&`/`||`, ranges, `case`, `<=>`, `??`).
fn operator_symbol(op: AstBin) -> Option<&'static str> {
    Some(match op {
        AstBin::Add => "+",
        AstBin::Sub => "-",
        AstBin::Mul => "*",
        AstBin::Div => "/",
        AstBin::Mod => "%",
        AstBin::BitAnd => "&",
        AstBin::BitOr => "|",
        AstBin::BitXor => "^",
        AstBin::Shl => "<<",
        AstBin::Shr => ">>",
        AstBin::Eq => "==",
        AstBin::Ne => "!=",
        AstBin::Lt => "<",
        AstBin::Le => "<=",
        AstBin::Gt => ">",
        AstBin::Ge => ">=",
        _ => return None,
    })
}

/// The source symbol of a prefix unary operator, for matching a user-defined
/// one-arg `operator <sym>`. `None` for prefixes that don't overload this way.
fn unary_operator_symbol(op: UnOp) -> Option<&'static str> {
    Some(match op {
        UnOp::Neg => "-",
        UnOp::Not => "!",
        UnOp::BitNot => "~",
        _ => return None,
    })
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
                attributes,
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
                    .map(|p| param_ir_ty(p, src, structs, env))
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
                // A `[Comptime]` method is compile-time-only: record its symbol so
                // the comptime fold pass JIT-evaluates it and folds its call sites
                // into literals (then drops the function from the final program).
                if has_comptime_attr(attributes, src) {
                    m.comptime.push(full_name.clone());
                }
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
                index_params,
                ..
            } => {
                // Lower each `get`/`set` accessor as the `get_{Name}`/`set_{Name}`
                // method the pre-pass registered. A computed accessor lowers its
                // AST body via `lower_method` (sees `this` like any instance
                // method); an auto accessor has no body, so we synthesize a
                // trivial read/write of the backing field `{Name}$prop`. An
                // indexer's bracket params (`this[i]`) are the accessor's explicit
                // params — bound in the body just like a method's.
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
                            index_params,
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
                            index_params,
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
        ty: param_ir_ty(p, src, structs, env),
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
            // A `ref`/`out` parameter arrives as a pointer to the caller's
            // storage (`Param` is already `Ptr`). Bind the name straight to that
            // pointer — no entry spill — so reads `load` and writes `store` go
            // *through* it, mutating the caller's variable. The bound value type
            // is the pointee, so an ordinary read/assign sees the value.
            if is_by_ref(p) {
                let pointee = lower_ty_env(&p.ty, src, structs, env);
                lw.bind(
                    nm.text(src),
                    Value::Param((i + base) as u32),
                    pointee,
                    None,
                );
                continue;
            }
            let ty = lower_ty_env(&p.ty, src, structs, env);
            let elem = pointer_elem_env(&p.ty, src, structs, env);
            let slot = lw.fb.alloca(ty);
            lw.fb.store(slot.clone(), Value::Param((i + base) as u32));
            lw.bind(nm.text(src), slot, ty, elem);
            lw.note_enum_local(nm.text(src), &p.ty, src);
            // A `T[]` parameter is an array: mark it so `a.Count`/`foreach`/`delete`
            // work on it just like an array local (the value is the elements
            // pointer; the length header rides 8 bytes behind it).
            if matches!(p.ty, AstType::Array { .. }) {
                lw.array_locals.insert(nm.text(src).to_string());
            }
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
    /// Names of heap-array locals (`T[] a = new T[n]`). The value is a pointer to
    /// the elements; the length is stored in the 8 bytes just *before* it, so
    /// `a[i]` reuses the typed-pointer index path and `a.Count` loads `ptr[-1]`.
    array_locals: std::collections::HashSet<String>,
    /// Per-block stacks of `defer`red statement bodies (cloned). A block runs its
    /// own in reverse on normal exit; a `return` runs every pending scope's, all
    /// in reverse (LIFO), before the `ret`. Parallel to `scopes`.
    defers: Vec<Vec<Stmt>>,
    /// In-scope local (nested) functions: name → (emitted symbol, return type,
    /// parameter types). A bare call to one lowers to a direct call to its symbol.
    local_fns: HashMap<String, (String, IrType, Vec<IrType>)>,
    /// Per-block stacks of `scope`-allocated class instances: each frame is
    /// `(entry block, [(pointer, class id), …])`. Heap allocations with the
    /// lifetime of the enclosing block. Parallel to `defers`; each block frees
    /// its own (dtor + free) on normal exit, and a `return` frees every open
    /// frame. Only allocations made in the frame's *entry* block are tracked, so
    /// the freed value provably dominates the exit (SSA-safe); a `scope` in a
    /// conditional sub-expression isn't tracked and leaks (a follow-on). (Like
    /// `defer`, `break`/`continue` out of a block doesn't yet run cleanup.)
    scope_allocs: Vec<(BlockId, Vec<(Value, StructId)>)>,
    /// Locals/params whose declared type is an int-backed `enum`: name → enum
    /// name. Int-backed enums lower to `int32`, losing their identity, so this
    /// recovers it — letting a bare `.Case` pattern in `switch (x)` resolve
    /// against `x`'s enum (the scrutinee determines the enum, as Beef requires).
    enum_locals: HashMap<String, String>,
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
            array_locals: std::collections::HashSet::new(),
            defers: vec![Vec::new()],
            local_fns: HashMap::new(),
            scope_allocs: vec![(BlockId(0), Vec::new())],
            enum_locals: HashMap::new(),
        }
    }

    /// Free the current block's `scope`-allocated instances (dtor + free), in
    /// reverse allocation order. Called on a block's normal fall-through exit.
    fn free_scope_top(&mut self) {
        if let Some((_, frame)) = self.scope_allocs.last() {
            let allocs: Vec<(Value, StructId)> = frame.iter().rev().cloned().collect();
            for (v, id) in allocs {
                self.emit_destroy(v, id);
            }
        }
    }

    /// Free every open frame's `scope`-allocated instances — innermost first,
    /// reverse within each — before a `return` unwinds the function.
    fn free_all_scopes(&mut self) {
        let allocs: Vec<(Value, StructId)> = self
            .scope_allocs
            .iter()
            .rev()
            .flat_map(|(_, frame)| frame.iter().rev().cloned())
            .collect();
        for (v, id) in allocs {
            self.emit_destroy(v, id);
        }
    }

    fn bind(&mut self, name: &str, slot: Value, ty: IrType, elem: Option<IrType>) {
        self.scopes
            .last_mut()
            .unwrap()
            .insert(name.to_string(), (slot, ty, elem));
    }

    /// If `ty` names an int-backed `enum`, remember that `name` has that enum
    /// type — so a bare `.Case` pattern in `switch (name)` can resolve against it.
    fn note_enum_local(&mut self, name: &str, ty: &AstType, src: &str) {
        if let AstType::Path { segments, .. } = ty
            && let Some(seg) = segments.last()
        {
            let en = seg.name.text(src);
            if self.structs.enums.contains_key(en) {
                self.enum_locals.insert(name.to_string(), en.to_string());
            }
        }
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
                self.defers.push(Vec::new());
                self.scope_allocs.push((self.fb.current_block(), Vec::new()));
                for st in stmts {
                    self.stmt(st, src);
                    if self.terminated {
                        break;
                    }
                }
                // Normal exit (fall-through): run this block's `defer`s in reverse,
                // then free its `scope` allocations. If a `return`/`break` already
                // terminated the block, it ran its own cleanup, so skip here.
                if !self.terminated {
                    self.run_block_defers(src);
                    self.free_scope_top();
                }
                self.scope_allocs.pop();
                self.defers.pop();
                self.scopes.pop();
            }
            Stmt::Expr { expr, .. } => {
                self.expr(expr, src);
            }
            Stmt::Empty(_) => {}
            // A multi-declarator group `int a = 1, b = 2;` — lower each in the
            // *current* scope (it's scope-transparent, unlike a block).
            Stmt::Locals { decls, .. } => {
                for d in decls {
                    self.stmt(d, src);
                    if self.terminated {
                        break;
                    }
                }
            }
            Stmt::Local { ty, name, init, .. } => {
                // Resolve the declared slot type up front so a target-typed enum
                // initializer (`Option<int32> a = .Some(40);`) can pick the right
                // monomorph — `.Some` alone is ambiguous across `Option`'s monos.
                let declared = ty
                    .as_ref()
                    .map(|t| lower_ty_env(t, src, self.structs, self.env));
                let (init_val, init_ty) = match init {
                    Some(e) => {
                        let (v, t) = declared
                            .and_then(|target| {
                                self.try_target_typed_enum(target, e, src)
                                    .or_else(|| self.try_target_typed_tuple(target, e, src))
                                    .or_else(|| self.try_target_typed_ctor(target, e, src))
                                    .or_else(|| self.try_target_typed_initializer(target, e, src))
                            })
                            .unwrap_or_else(|| self.expr(e, src));
                        (Some(v), Some(t))
                    }
                    None => (None, None),
                };
                let slot_ty = declared.unwrap_or_else(|| init_ty.unwrap_or(IrType::I64));
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
                if let Some(t) = ty {
                    self.note_enum_local(name.text(src), t, src);
                }
                // A heap-array local (`T[] a`): remember it's an array so `a.Count`
                // reads the length header and `delete a` frees the real block base.
                if matches!(ty, Some(AstType::Array { .. })) {
                    self.array_locals.insert(name.text(src).to_string());
                }
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
                        // Target-type a `.Some(x)` / `.(args)` return against the
                        // function's return type before falling back to a plain eval.
                        let (v, t) = self
                            .try_target_typed_enum(self.ret_ty, e, src)
                            .or_else(|| self.try_target_typed_ctor(self.ret_ty, e, src))
                            .unwrap_or_else(|| self.expr(e, src));
                        self.coerce(v, t, self.ret_ty)
                    })
                };
                // The return value is captured (above) *before* `defer`s run, so a
                // deferred mutation can't change it — then unwind every pending
                // defer (LIFO) and free every open scope allocation before `ret`.
                self.run_all_defers(src);
                self.free_all_scopes();
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
                init_extra,
                cond,
                update,
                update_extra,
                body,
                ..
            } => {
                // C-style `for (init; cond; update) body`. The loop variable
                // lives in its own scope; `continue` runs `update` then re-tests.
                self.scopes.push(HashMap::new());
                if let Some(init) = init {
                    self.stmt(init, src);
                }
                for s in init_extra {
                    self.stmt(s, src);
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
                // cont: run the update(s), then back to the head.
                self.switch(cont);
                if let Some(u) = update {
                    self.expr(u, src);
                }
                for u in update_extra {
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
                // `for (x in arr)` over a heap-array local → an indexed loop
                // `for (i = 0; i < arr.Count; i += 1) { x = arr[i]; body }`. The
                // element address is the same typed-pointer index `a[i]` uses.
                if let Expr::Ident(s) = iter
                    && self.array_locals.contains(s.text(src))
                    && let Some(elem_ty) = self.lookup_elem(s.text(src))
                {
                    let (arr, _) = self.expr(iter, src);
                    self.scopes.push(HashMap::new());
                    let hdr =
                        self.fb
                            .elem_addr(arr.clone(), IrType::U8, Value::int(-8, IrType::I64));
                    let count = self.fb.load(hdr, IrType::I64);
                    let arr_slot = self.fb.alloca(IrType::Ptr);
                    self.fb.store(arr_slot.clone(), arr);
                    let cnt_slot = self.fb.alloca(IrType::I64);
                    self.fb.store(cnt_slot.clone(), count);
                    let idx_slot = self.fb.alloca(IrType::I64);
                    self.fb.store(idx_slot.clone(), Value::int(0, IrType::I64));
                    let var_slot = self.fb.alloca(elem_ty);
                    self.bind(name.text(src), var_slot.clone(), elem_ty, None);
                    let head = self.fb.create_block("foreach.head");
                    let body_b = self.fb.create_block("foreach.body");
                    let cont = self.fb.create_block("foreach.cont");
                    let exit = self.fb.create_block("foreach.exit");
                    self.fb.br(head);
                    self.switch(head);
                    let iv = self.fb.load(idx_slot.clone(), IrType::I64);
                    let cv = self.fb.load(cnt_slot.clone(), IrType::I64);
                    let test = self.fb.cmp(CmpPred::Slt, iv, cv);
                    self.fb.cond_br(test, body_b, exit);
                    self.terminated = true;
                    self.switch(body_b);
                    let base = self.fb.load(arr_slot.clone(), IrType::Ptr);
                    let iv = self.fb.load(idx_slot.clone(), IrType::I64);
                    let ep = self.fb.elem_addr(base, elem_ty, iv);
                    let ev = self.fb.load(ep, elem_ty);
                    self.fb.store(var_slot.clone(), ev);
                    self.loops.push((cont, exit));
                    self.stmt(body, src);
                    self.loops.pop();
                    if !self.terminated {
                        self.fb.br(cont);
                    }
                    self.switch(cont);
                    let iv = self.fb.load(idx_slot.clone(), IrType::I64);
                    let inc = self.fb.bin(IrBin::Add, iv, Value::int(1, IrType::I64), IrType::I64);
                    self.fb.store(idx_slot, inc);
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

                // An int-backed enum scrutinee (e.g. a `Color` local) lowers to
                // `int32`, so a bare `.Case` pattern needs the enum name to
                // resolve. Recover it from the scrutinee when it's a tracked
                // local/param; the scrutinee determines the enum, per Beef.
                let scrut_enum: Option<String> = match scrutinee {
                    Expr::Ident(s) => self.enum_locals.get(s.text(src)).cloned(),
                    _ => None,
                };
                let case_idxs: Vec<usize> = (0..arms.len())
                    .filter(|&i| arms[i].pattern.is_some())
                    .collect();
                if case_idxs.is_empty() {
                    self.fb.br(default_target);
                    self.terminated = true;
                }
                for (chain_i, &arm_i) in case_idxs.iter().enumerate() {
                    // A multi-value case `case a, b, c:` matches if the scrutinee
                    // equals any listed value — fold the per-value `==` with `or`.
                    // Label expressions are constants/side-effect-free, so testing
                    // them all unconditionally is fine.
                    let pat = arms[arm_i].pattern.as_ref().unwrap();
                    let mut eq: Option<Value> = None;
                    for p in std::iter::once(pat).chain(arms[arm_i].extra.iter()) {
                        let (pv, pt) = self.lower_case_value(p, scrut_enum.as_deref(), src);
                        let ct = common_type(st, pt);
                        let l = self.coerce(sv.clone(), st, ct);
                        let r = self.coerce(pv, pt, ct);
                        let cmp = self.fb.cmp(CmpPred::Eq, l, r);
                        eq = Some(match eq {
                            None => cmp,
                            Some(prev) => self.fb.bin(IrBin::Or, prev, cmp, IrType::Bool),
                        });
                    }
                    let eq = eq.unwrap();
                    let last = chain_i + 1 == case_idxs.len();
                    let next = if last {
                        default_target
                    } else {
                        self.fb.create_block("switch.test")
                    };
                    if let Some(g) = arms[arm_i].guard.as_ref() {
                        // `case v when guard:` — the value must match *and* the
                        // guard hold. Evaluate the guard only on a value match.
                        let guard_b = self.fb.create_block("switch.guard");
                        self.fb.cond_br(eq, guard_b, next);
                        self.switch(guard_b);
                        let (gv, gt) = self.expr(g, src);
                        let gb = self.coerce_bool(gv, gt);
                        self.fb.cond_br(gb, body_blocks[arm_i], next);
                    } else {
                        self.fb.cond_br(eq, body_blocks[arm_i], next);
                    }
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
            // `defer stmt;` — queue the statement to run at the enclosing block's
            // exit (in reverse declaration order). The body is cloned so it can be
            // lowered later when the scope closes (or a `return` unwinds it).
            Stmt::Defer { body, .. } => {
                if let Some(scope) = self.defers.last_mut() {
                    scope.push((**body).clone());
                }
            }
            // A local (nested) function: its body is emitted separately (the
            // `$localfn{N}` symbol assigned in the pre-pass); here we just bring
            // the name into scope so a bare call resolves to a direct call.
            Stmt::LocalFunction {
                return_ty,
                name,
                params,
                generic_params,
                ..
            } if generic_params.is_empty() => {
                if let Some(sym) = self.structs.local_fn_syms.get(name).cloned() {
                    let ret = lower_ty_env(return_ty, src, self.structs, self.env);
                    let ptys: Vec<IrType> = params
                        .iter()
                        .map(|p| param_ir_ty(p, src, self.structs, self.env))
                        .collect();
                    self.local_fns
                        .insert(name.text(src).to_string(), (sym, ret, ptys));
                }
            }
            // local-function, mixin — not in the kernel yet. Skipped (no IR
            // emitted), never panicking.
            _ => {}
        }
    }

    /// Run the current block's `defer`s in reverse declaration order (LIFO). The
    /// block's variable scope is still live, so the deferred code can still see
    /// its locals.
    fn run_block_defers(&mut self, src: &str) {
        let pending: Vec<Stmt> = match self.defers.last() {
            Some(scope) => scope.iter().rev().cloned().collect(),
            None => return,
        };
        for s in &pending {
            self.stmt(s, src);
            if self.terminated {
                break;
            }
        }
    }

    /// Run *every* pending `defer` across all open scopes before a `return` —
    /// innermost scope first, and within each scope reverse declaration order.
    fn run_all_defers(&mut self, src: &str) {
        let pending: Vec<Stmt> = self
            .defers
            .iter()
            .rev()
            .flat_map(|scope| scope.iter().rev().cloned())
            .collect();
        for s in &pending {
            self.stmt(s, src);
            if self.terminated {
                return;
            }
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
            Stmt::Locals { decls, .. } => {
                for d in decls {
                    self.caps_stmt(d, src, bound, seen, caps);
                }
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
                init_extra,
                cond,
                update,
                update_extra,
                body,
                ..
            } => {
                if let Some(i) = init {
                    self.caps_stmt(i, src, bound, seen, caps);
                }
                for s in init_extra {
                    self.caps_stmt(s, src, bound, seen, caps);
                }
                if let Some(c) = cond {
                    self.caps_expr(c, src, bound, seen, caps);
                }
                if let Some(u) = update {
                    self.caps_expr(u, src, bound, seen, caps);
                }
                for u in update_extra {
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
            // `$"…{expr}…"` → a freshly-`new`'d `String` with each literal run and
            // each hole's value appended (via the type-matched `String.Append`).
            Expr::Interp { parts, .. } => self.lower_interp(parts, src),
            // `sizeof(T)` → the type's byte size, an `int` (I64). A value struct
            // defers to the IR `SizeOf` (LLVM's DataLayout — the same size `new`
            // allocates); scalars and references are constant-sized (a class
            // reference is pointer-sized).
            Expr::SizeOf { ty, .. } => {
                let it = lower_ty_env(ty, src, self.structs, self.env);
                (self.size_of_ty(it), IrType::I64)
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
            // A tuple literal `(a, b, …)` builds its synthetic value struct. With
            // no target the shape is inferred from the element types; a tuple-typed
            // local/return target-types it (so `(int32,int32) t = (3,4)` coerces the
            // i64 literals to the i32 fields) via the `Stmt::Local`/`Return` paths.
            Expr::Tuple { elems, .. } => self
                .build_tuple(None, elems, src)
                .unwrap_or((undef(IrType::I64), IrType::I64)),
            // Object/collection initializer with no target (`new T() { … }` or
            // `Type { … }`): the base supplies the object; `.{ … }` needs a target
            // (handled in `Stmt::Local`), so without one it degrades to the base.
            Expr::Initializer { base, entries, .. } => self.lower_initializer(base, entries, None, src),
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
            Expr::Binary {
                op: AstBin::NullCoalesce,
                lhs,
                rhs,
                ..
            } => self.null_coalesce(lhs, rhs, src),
            // `obj is T` / `obj as T`: the RHS names a *type*, not a value, so they
            // must be handled before the generic `binary` (which would evaluate it).
            Expr::Binary {
                op: AstBin::Is,
                lhs,
                rhs,
                ..
            } => self.lower_is(lhs, rhs, src),
            Expr::Binary {
                op: AstBin::As,
                lhs,
                rhs,
                ..
            } => self.lower_as(lhs, rhs, src),
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
                // Null-conditional call `a?.M(args)`: null-guard the whole call
                // (the result is the method's default when `a` is null).
                if let Expr::Member {
                    base,
                    name,
                    conditional: true,
                    ..
                } = &**callee
                {
                    return self.lower_conditional_call(base, name.text(src), args, src);
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
                    args.iter().map(|a| self.arg_value(a, src)).collect();
                if let Expr::Ident(s) = &**callee {
                    let name = s.text(src);
                    // A local (nested) function in scope → a direct call to its
                    // emitted `$localfn{N}` symbol, args coerced to its params.
                    if let Some((sym, ret, ptys)) = self.local_fns.get(name).cloned() {
                        let call_args: Vec<Value> = arg_vals
                            .into_iter()
                            .enumerate()
                            .map(|(i, (v, t))| self.coerce(v, t, ptys.get(i).copied().unwrap_or(t)))
                            .collect();
                        return (self.fb.call(sym, call_args, ret), ret);
                    }
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
                        // Same-type call (incl. recursion). A `params T[]` callee
                        // packs the overflow args into a `T[]`; otherwise each arg
                        // coerces to its param type positionally.
                        let coerced: Vec<Value> = if let Some(elem) = sig.variadic {
                            self.pack_variadic_args(&sig.params, elem, arg_vals)
                        } else {
                            arg_vals
                                .into_iter()
                                .enumerate()
                                .map(|(i, (v, t))| self.coerce(v, t, sig.params[i]))
                                .collect()
                        };
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
            // `scope T(args)` — allocate with the enclosing block's lifetime:
            // heap-allocate like `new` (ctor + field defaults run), then register
            // the instance for an automatic dtor+free at scope exit, so it's
            // freed without a manual `delete`.
            Expr::Prefix {
                kw: PrefixKw::Scope,
                operand,
                ..
            } => {
                let (v, t) = self.lower_new(operand, src);
                // Auto-free at scope exit only when the allocation is at statement
                // level (its def block dominates the block's exit). A `scope` in a
                // conditional sub-expression (ternary/short-circuit branch) is
                // *not* tracked — freeing it at block exit would reference a value
                // that doesn't dominate the exit (an SSA violation) — so it leaks
                // for now (a documented follow-on needing real lifetime analysis).
                let cur = self.fb.current_block();
                if let IrType::Ref(id) = t
                    && let Some((entry, frame)) = self.scope_allocs.last_mut()
                    && cur == *entry
                {
                    frame.push((v.clone(), id));
                }
                (v, t)
            }
            Expr::Prefix {
                kw: PrefixKw::Delete,
                operand,
                ..
            } => self.lower_delete(operand, src),
            // Null-conditional member read `a?.field`: evaluate `a` once and
            // null-guard the field load (yields the field's default when null).
            Expr::Member {
                base,
                name,
                conditional: true,
                ..
            } => self.lower_conditional_member(base, name.text(src), src),
            // Member read (`obj.field` / `ref.field`): load the resolved field;
            // degrade if the base isn't a known struct/reference place.
            Expr::Member { base, name, .. } => {
                // An array's `Count`/`Length`: the length sits in the 8 bytes just
                // before the elements pointer, so load `ptr[-1]` (an `int`).
                if let Some(r) = self.try_array_count(base, name.text(src), src) {
                    r
                }
                // A payloadless payload-enum case (`IntOpt.None`) constructs its
                // tagged-union struct; a plain int-backed `Enum.Case` is a constant.
                else if let Some(r) = self.try_enum_construct(base, name.text(src), &[], src) {
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
            // Index read (`p[i]`): load the element at the computed address for a
            // pointer/array; otherwise a user indexer (`obj[i]` → `get_this`).
            Expr::Index { base, args, .. } => match self.lvalue(e, src) {
                Some((ptr, ty)) => (self.fb.load(ptr, ty), ty),
                None => self
                    .try_indexer_get(base, args, src)
                    .unwrap_or((undef(IrType::I64), IrType::I64)),
            },
            // Explicit cast `(T)expr` — evaluate the operand and `coerce` it to
            // the target type (the same machinery implicit coercions use:
            // int↔int width changes, int↔float, float width changes).
            Expr::Cast { ty, operand, .. } => {
                let (v, vt) = self.expr(operand, src);
                let to = lower_ty_env(ty, src, self.structs, self.env);
                (self.coerce(v, vt, to), to)
            }
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
        // Unary operator overloading: a user type may define a static one-arg
        // `operator -` / `operator !` / `operator ~`. (Binary `-` is also
        // `operator-`, but a one-param signature picks the unary form.)
        if matches!(t, IrType::Struct(_) | IrType::Ref(_))
            && let Some(sym) = unary_operator_symbol(op)
            && let Some(res) = self.try_unary_operator_overload(sym, v.clone(), t)
        {
            return res;
        }
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

    /// `a ?? b` — null-coalescing: the value of `a` if it isn't null, else `b`.
    /// Short-circuits (only evaluates `b` when `a` is null), so it's lowered like
    /// a `?:` keyed on `a == null` rather than as an eager binary op. `a` is
    /// evaluated once; both arms coerce to the common type and join via a phi.
    fn null_coalesce(&mut self, lhs: &Expr, rhs: &Expr, src: &str) -> (Value, IrType) {
        let (lv, lt) = self.expr(lhs, src);
        let lhs_b = self.fb.create_block("nc.lhs");
        let rhs_b = self.fb.create_block("nc.rhs");
        let join_b = self.fb.create_block("nc.join");
        let is_null = self.fb.cmp(CmpPred::Eq, lv.clone(), zero_of(lt));
        self.fb.cond_br(is_null, rhs_b, lhs_b);

        // The fallback arm runs only when `a` was null.
        self.switch(rhs_b);
        let (rv, rt) = self.expr(rhs, src);
        let rhs_end = self.fb.current_block();
        let ty = common_type(lt, rt);
        self.fb.switch_to(rhs_end);
        let r2 = self.coerce(rv, rt, ty);
        self.fb.br(join_b);

        // The `a`-is-non-null arm: just coerce the already-computed `a`.
        self.fb.switch_to(lhs_b);
        let l2 = self.coerce(lv, lt, ty);
        self.fb.br(join_b);

        self.switch(join_b);
        let r = self.fb.phi(vec![(lhs_b, l2), (rhs_end, r2)], ty);
        (r, ty)
    }

    fn binary(&mut self, op: AstBin, lhs: &Expr, rhs: &Expr, src: &str) -> (Value, IrType) {
        let (l, lt) = self.expr(lhs, src);
        let (r, rt) = self.expr(rhs, src);
        // Operator overloading: when an operand is a user type (`struct`/`class`)
        // and it (or the other operand's type) defines a static `operator <sym>`
        // taking both operands, call it. Scalars skip this and take the kernel
        // paths below; a `Ref==Ref` with no `operator==` still falls to the
        // String-`Equals` / identity path.
        if (matches!(lt, IrType::Struct(_) | IrType::Ref(_))
            || matches!(rt, IrType::Struct(_) | IrType::Ref(_)))
            && let Some(sym) = operator_symbol(op)
            && let Some(res) = self.try_operator_overload(sym, l.clone(), lt, r.clone(), rt)
        {
            return res;
        }
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

    /// Lower a switch case-label value. A bare `.Case` (`DotIdent`) is resolved
    /// against the scrutinee's int-backed enum `scrut_enum` (which the plain
    /// expression path can't, since the enum name is only known from the
    /// scrutinee); everything else lowers as an ordinary expression.
    fn lower_case_value(
        &mut self,
        pat: &Expr,
        scrut_enum: Option<&str>,
        src: &str,
    ) -> (Value, IrType) {
        if let Expr::DotIdent { name, .. } = pat
            && let Some(en) = scrut_enum
            && let Some(cases) = self.structs.enums.get(en)
            && let Some(&v) = cases.get(name.text(src))
        {
            let i32t = IrType::Int {
                bits: 32,
                signed: true,
            };
            return (Value::int(v as i128, i32t), i32t);
        }
        self.expr(pat, src)
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

    /// Resolve `l <sym> r` to a user-defined `operator <sym>`: scan both operand
    /// types for a static two-argument operator method whose name matches (with
    /// whitespace ignored — `operator +` and `operator+` are the same), coerce
    /// each operand to the method's declared parameter type, and emit the call.
    /// `None` if neither type defines it (the caller then tries the kernel paths).
    fn try_operator_overload(
        &mut self,
        sym: &str,
        l: Value,
        lt: IrType,
        r: Value,
        rt: IrType,
    ) -> Option<(Value, IrType)> {
        let want = format!("operator{sym}");
        let mut found: Option<MethodSig> = None;
        for ty in [lt, rt] {
            let id = match ty {
                IrType::Struct(id) | IrType::Ref(id) => id,
                _ => continue,
            };
            // Among this type's `operator<sym>` overloads, pick the one whose two
            // parameters best fit the operand types — so `String + String` and
            // `String + char8` resolve to their respective overloads instead of
            // whichever was registered first (which would coerce a `char8` into a
            // `String` reference and dereference garbage).
            let mut best: Option<(MethodSig, u32)> = None;
            for (key, sigs) in &self.structs.methods[id.0 as usize] {
                if key.split_whitespace().collect::<String>() != want {
                    continue;
                }
                for s in sigs.iter().filter(|s| !s.is_instance && s.params.len() == 2) {
                    let score = type_affinity(s.params[0], lt) + type_affinity(s.params[1], rt);
                    if best.as_ref().is_none_or(|(_, bs)| score > *bs) {
                        best = Some((s.clone(), score));
                    }
                }
            }
            if let Some((sig, _)) = best {
                found = Some(sig);
                break;
            }
        }
        let sig = found?;
        let a = self.coerce(l, lt, sig.params[0]);
        let b = self.coerce(r, rt, sig.params[1]);
        Some((
            self.fb.call(sig.full_name.clone(), vec![a, b], sig.ret),
            sig.ret,
        ))
    }

    /// Resolve `<sym> v` to a user-defined one-arg `operator <sym>` on `v`'s type.
    /// Mirrors [`Self::try_operator_overload`] but for a single operand (so it
    /// picks the unary `operator-` over the binary one by param count).
    fn try_unary_operator_overload(
        &mut self,
        sym: &str,
        v: Value,
        t: IrType,
    ) -> Option<(Value, IrType)> {
        let id = match t {
            IrType::Struct(id) | IrType::Ref(id) => id,
            _ => return None,
        };
        let want = format!("operator{sym}");
        let mut found: Option<MethodSig> = None;
        for (key, sigs) in &self.structs.methods[id.0 as usize] {
            if key.split_whitespace().collect::<String>() != want {
                continue;
            }
            if let Some(sig) = sigs.iter().find(|s| !s.is_instance && s.params.len() == 1) {
                found = Some(sig.clone());
                break;
            }
        }
        let sig = found?;
        let a = self.coerce(v, t, sig.params[0]);
        Some((
            self.fb.call(sig.full_name.clone(), vec![a], sig.ret),
            sig.ret,
        ))
    }

    /// Construct a `.Case(args)` / bare `.Case` initializer against a *known
    /// target type* (a local's declared type, a return type). Unlike
    /// [`Self::try_enum_construct_dot`] this resolves the enum by the target — not
    /// by uniqueness — so it disambiguates `.Some(40)` between `Option<int32>` and
    /// `Option<bool>`. `None` if the target isn't a payload enum or `e` isn't a
    /// leading-dot case form (the caller then evaluates `e` normally).
    fn try_target_typed_enum(
        &mut self,
        target: IrType,
        e: &Expr,
        src: &str,
    ) -> Option<(Value, IrType)> {
        let IrType::Struct(id) = target else {
            return None;
        };
        if !self.structs.enum_cases.contains_key(&id) {
            return None;
        }
        let (case, args): (&str, &[Expr]) = match e {
            Expr::Call { callee, args, .. } => match &**callee {
                Expr::DotIdent { name, .. } => (name.text(src), args.as_slice()),
                _ => return None,
            },
            Expr::DotIdent { name, .. } => (name.text(src), &[]),
            _ => return None,
        };
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
            match want {
                Some(d) if arms[arm_i].guard.is_some() => {
                    // Guarded arm: the discriminant must match *and* the `when`
                    // guard must hold. On a disc match, jump to a guard block that
                    // binds the payload (so the guard can read it), evaluates the
                    // guard, and branches to the body or on to the next test.
                    let eq = self
                        .fb
                        .cmp(CmpPred::Eq, disc.clone(), Value::int(d as i128, i32t));
                    let guard_b = self.fb.create_block("match.guard");
                    self.fb.cond_br(eq, guard_b, next);
                    self.switch(guard_b);
                    self.scopes.push(HashMap::new());
                    self.bind_enum_payload(&slot, id, pat, src);
                    let g = arms[arm_i].guard.as_ref().unwrap();
                    let (gv, gt) = self.expr(g, src);
                    let gb = self.coerce_bool(gv, gt);
                    self.scopes.pop();
                    self.fb.cond_br(gb, body_blocks[arm_i], next);
                }
                Some(d) => {
                    let eq = self
                        .fb
                        .cmp(CmpPred::Eq, disc.clone(), Value::int(d as i128, i32t));
                    self.fb.cond_br(eq, body_blocks[arm_i], next);
                }
                None => self.fb.br(next),
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
            if let Some(pat) = arms[i].pattern.as_ref() {
                self.bind_enum_payload(&slot, id, pat, src);
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

    /// Bind an enum-`match`/`case` pattern's payload `let`-names to their fields
    /// in the scrutinee `slot` (the current scope). Shared by `match` arm bodies
    /// and `when`-guard evaluation. A pattern that isn't an enum-case shape (or
    /// whose case is unknown) binds nothing.
    fn bind_enum_payload(&mut self, slot: &Value, id: StructId, pat: &Expr, src: &str) {
        let Some((case, binds)) = enum_pattern(pat, src) else {
            return;
        };
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

    /// `obj[i]` for a user type with an indexer → call its `get_this(this,
    /// idx…)`. The receiver's body pointer is the leading `this`; the bracket
    /// args follow, coerced to the getter's parameter types. `None` if the
    /// receiver isn't a struct/class or has no matching indexer getter.
    fn try_indexer_get(
        &mut self,
        base: &Expr,
        args: &[Expr],
        src: &str,
    ) -> Option<(Value, IrType)> {
        let (body_ptr, owner) = self.struct_base(base, src)?;
        let arg_vals: Vec<(Value, IrType)> = args.iter().map(|a| self.expr(a, src)).collect();
        let arg_tys: Vec<IrType> = arg_vals.iter().map(|(_, t)| *t).collect();
        let getter = self.structs.methods[owner.0 as usize]
            .get("get_this")
            .and_then(|c| pick_overload(c, &arg_tys, true))?
            .clone();
        let mut call_args = vec![body_ptr];
        for (i, (v, t)) in arg_vals.into_iter().enumerate() {
            let pt = getter.params.get(i + 1).copied().unwrap_or(t);
            call_args.push(self.coerce(v, t, pt));
        }
        Some((
            self.fb.call(getter.full_name, call_args, getter.ret),
            getter.ret,
        ))
    }

    /// `obj[i] = v` for a user type with an indexer → call `set_this(this, idx…,
    /// value)`. `None` if there's no matching indexer setter.
    fn try_indexer_set(
        &mut self,
        base: &Expr,
        args: &[Expr],
        rhs: Value,
        rhs_ty: IrType,
        src: &str,
    ) -> Option<(Value, IrType)> {
        let (body_ptr, owner) = self.struct_base(base, src)?;
        let arg_vals: Vec<(Value, IrType)> = args.iter().map(|a| self.expr(a, src)).collect();
        let arg_tys: Vec<IrType> = arg_vals
            .iter()
            .map(|(_, t)| *t)
            .chain(std::iter::once(rhs_ty))
            .collect();
        let setter = self.structs.methods[owner.0 as usize]
            .get("set_this")
            .and_then(|c| pick_overload(c, &arg_tys, true))?
            .clone();
        let mut call_args = vec![body_ptr];
        for (i, (v, t)) in arg_vals.into_iter().enumerate() {
            let pt = setter.params.get(i + 1).copied().unwrap_or(t);
            call_args.push(self.coerce(v, t, pt));
        }
        let val_pty = *setter.params.last().unwrap();
        let val = self.coerce(rhs, rhs_ty, val_pty);
        call_args.push(val.clone());
        self.fb.call(setter.full_name, call_args, IrType::Void);
        Some((val, val_pty))
    }

    /// `obj[i] op= v` → `set_this(obj, i, get_this(obj, i) op v)`. Reads the
    /// element through the indexer getter, combines it (operator overload for a
    /// user type, `arith` for a scalar), and writes it back through the setter,
    /// evaluating the receiver and index args once. `None` if the type has no
    /// matching get+set indexer pair.
    fn try_indexer_compound(
        &mut self,
        astbin: AstBin,
        base: &Expr,
        args: &[Expr],
        rhs: Value,
        rhs_ty: IrType,
        src: &str,
    ) -> Option<(Value, IrType)> {
        let (body_ptr, owner) = self.struct_base(base, src)?;
        let arg_vals: Vec<(Value, IrType)> = args.iter().map(|a| self.expr(a, src)).collect();
        let arg_tys: Vec<IrType> = arg_vals.iter().map(|(_, t)| *t).collect();
        let getter = self.structs.methods[owner.0 as usize]
            .get("get_this")
            .and_then(|c| pick_overload(c, &arg_tys, true))?
            .clone();
        let pty = getter.ret;
        let set_tys: Vec<IrType> = arg_tys
            .iter()
            .copied()
            .chain(std::iter::once(pty))
            .collect();
        let setter = self.structs.methods[owner.0 as usize]
            .get("set_this")
            .and_then(|c| pick_overload(c, &set_tys, true))?
            .clone();
        // Read the current element through the getter.
        let mut get_args = vec![body_ptr.clone()];
        for (i, (v, t)) in arg_vals.iter().enumerate() {
            let pt = getter.params.get(i + 1).copied().unwrap_or(*t);
            let cv = self.coerce(v.clone(), *t, pt);
            get_args.push(cv);
        }
        let cur = self.fb.call(getter.full_name, get_args, pty);
        // Combine `cur op rhs` (operator overload for a user type, else numeric).
        let combined = if matches!(pty, IrType::Struct(_) | IrType::Ref(_))
            && let Some(sym) = operator_symbol(astbin)
            && let Some((res, _)) =
                self.try_operator_overload(sym, cur.clone(), pty, rhs.clone(), rhs_ty)
        {
            res
        } else {
            let v = self.coerce(rhs, rhs_ty, pty);
            self.arith(astbin, cur, v, pty)
        };
        // Write it back through the setter.
        let mut set_args = vec![body_ptr];
        for (i, (v, t)) in arg_vals.iter().enumerate() {
            let pt = setter.params.get(i + 1).copied().unwrap_or(*t);
            let cv = self.coerce(v.clone(), *t, pt);
            set_args.push(cv);
        }
        set_args.push(combined.clone());
        self.fb.call(setter.full_name, set_args, IrType::Void);
        Some((combined, pty))
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

    /// The byte size of an IR type as an `i64` — a value struct defers to the IR
    /// `SizeOf` (LLVM DataLayout), scalars/refs are constant-sized.
    fn size_of_ty(&mut self, ty: IrType) -> Value {
        match ty {
            IrType::Struct(id) => self.fb.size_of(id),
            IrType::Bool => Value::int(1, IrType::I64),
            IrType::Int { bits, .. } => Value::int((bits / 8) as i128, IrType::I64),
            IrType::Float { bits } => Value::int((bits / 8) as i128, IrType::I64),
            IrType::Ptr | IrType::Ref(_) => Value::int(8, IrType::I64),
            IrType::Void => Value::int(0, IrType::I64),
        }
    }

    /// The element IR type of an array-`new` size expression `T[n]` whose `base`
    /// names the element type `T`. Resolves a generic type-parameter through the
    /// monomorph env first (so `new T[n]` inside `List<int32>` sizes by `i32`),
    /// then registered types and primitives. `None` if the base isn't a bare name.
    fn array_elem_ty(&self, base: &Expr, src: &str) -> Option<IrType> {
        match base {
            Expr::Paren { inner, .. } => self.array_elem_ty(inner, src),
            Expr::Ident(s) => {
                let name = s.text(src);
                if let Some((_, t)) = self.env.iter().find(|(n, _)| n.as_str() == name) {
                    return Some(*t);
                }
                Some(self.structs.ty_of(name).unwrap_or_else(|| primitive(name)))
            }
            _ => None,
        }
    }

    /// Allocate a length-prefixed array block of `count` elements of type `elem`:
    /// `malloc(8 + count·sizeof(elem))`, store the length in the first 8 bytes,
    /// and yield a pointer to the *elements* (8 bytes past the block). So `a[i]`
    /// is an ordinary typed-pointer index and `a.Count` reads `ptr[-1]`.
    fn alloc_array(&mut self, count: Value, elem: IrType) -> Value {
        let esz = self.size_of_ty(elem);
        let bytes = self.fb.bin(IrBin::Mul, count.clone(), esz, IrType::I64);
        let total = self
            .fb
            .bin(IrBin::Add, bytes, Value::int(8, IrType::I64), IrType::I64);
        let block = self.fb.call("malloc", vec![total], IrType::Ptr);
        self.fb.store(block.clone(), count);
        self.fb
            .elem_addr(block, IrType::U8, Value::int(8, IrType::I64))
    }

    /// Build the argument list for a `params T[]` call: coerce the fixed leading
    /// args to their param types, then pack every remaining arg into a fresh
    /// `T[]`. `formal` is the callee's parameter types *without* `this`; its last
    /// entry is the `T[]` slot. The result excludes `this` (the caller prepends it
    /// for an instance method).
    fn pack_variadic_args(
        &mut self,
        formal: &[IrType],
        elem: IrType,
        arg_vals: Vec<(Value, IrType)>,
    ) -> Vec<Value> {
        let fixed = formal.len().saturating_sub(1);
        let mut out: Vec<Value> = Vec::with_capacity(formal.len());
        let mut it = arg_vals.into_iter();
        for ft in formal.iter().take(fixed) {
            if let Some((v, t)) = it.next() {
                out.push(self.coerce(v, t, *ft));
            }
        }
        let rest: Vec<(Value, IrType)> = it.collect();
        let arr = self.alloc_array(Value::int(rest.len() as i128, IrType::I64), elem);
        for (i, (v, t)) in rest.into_iter().enumerate() {
            let cv = self.coerce(v, t, elem);
            let ep = self
                .fb
                .elem_addr(arr.clone(), elem, Value::int(i as i128, IrType::I64));
            self.fb.store(ep, cv);
        }
        out.push(arr);
        out
    }

    /// `new T[n]` → an `n`-element heap array (elements indeterminate).
    fn lower_array_new(&mut self, elem: IrType, len: &Expr, src: &str) -> (Value, IrType) {
        let (lv, lt) = self.expr(len, src);
        let n = self.coerce(lv, lt, IrType::I64);
        (self.alloc_array(n, elem), IrType::Ptr)
    }

    /// `new T[](v0, v1, …)` / `new T[N](v0, …)` — an array initializer. The count
    /// is the explicit size if present, else the number of values; each value is
    /// stored into its element slot (coerced to `T`). Slots past the value list
    /// (when `N` exceeds the value count) are left indeterminate.
    fn lower_array_new_init(
        &mut self,
        elem: IrType,
        size: Option<&Expr>,
        values: &[Expr],
        src: &str,
    ) -> (Value, IrType) {
        let count = match size {
            Some(e) => {
                let (v, t) = self.expr(e, src);
                self.coerce(v, t, IrType::I64)
            }
            None => Value::int(values.len() as i128, IrType::I64),
        };
        let elems = self.alloc_array(count, elem);
        for (i, val) in values.iter().enumerate() {
            let (v, vt) = self.expr(val, src);
            let cv = self.coerce(v, vt, elem);
            let ep = self
                .fb
                .elem_addr(elems.clone(), elem, Value::int(i as i128, IrType::I64));
            self.fb.store(ep, cv);
        }
        (elems, IrType::Ptr)
    }

    /// `a.Count` / `a.Length` for a heap-array local → load the length header
    /// stored at `elements_ptr - 8`. `None` unless `base` is a known array local
    /// and `name` is `Count`/`Length`.
    fn try_array_count(&mut self, base: &Expr, name: &str, src: &str) -> Option<(Value, IrType)> {
        if !matches!(name, "Count" | "Length") {
            return None;
        }
        let Expr::Ident(s) = base else { return None };
        if !self.array_locals.contains(s.text(src)) {
            return None;
        }
        let (ptr, _) = self.expr(base, src);
        let hdr = self
            .fb
            .elem_addr(ptr, IrType::U8, Value::int(-8, IrType::I64));
        Some((self.fb.load(hdr, IrType::I64), IrType::I64))
    }

    fn lower_new(&mut self, operand: &Expr, src: &str) -> (Value, IrType) {
        // Object initializer `new T(args) { field = value, … }`: the operand is an
        // `Initializer` wrapping the construction. Allocate + run the ctor on the
        // inner base, then store each field through the new reference.
        if let Expr::Initializer { base, entries, .. } = operand {
            // Array collection init `new T[N] { v0, v1, … }`: the entries are the
            // element values, so build the array directly (the size from `[N]`, or
            // the entry count).
            if let Expr::Index {
                base: ibase,
                args: sz,
                ..
            } = &**base
                && let Some(elem) = self.array_elem_ty(ibase, src)
            {
                return self.lower_array_new_init(elem, sz.first(), entries, src);
            }
            let (obj, t) = self.lower_new(base, src);
            if let IrType::Ref(id) = t {
                self.assign_init_fields(obj.clone(), id, entries, src);
            }
            return (obj, t);
        }
        // Array allocation `new T[n]`: the operand is an `Index` whose base names
        // the element type. (A user-indexer `new` would have a *value* base, not a
        // type name, so `array_elem_ty` returning `Some` discriminates.)
        if let Expr::Index { base, args, .. } = operand
            && let Some(len) = args.first()
            && let Some(elem) = self.array_elem_ty(base, src)
        {
            return self.lower_array_new(elem, len, src);
        }
        // Array initializer `new T[](v0, …)` / `new T[N](v0, …)`: a `Call` whose
        // callee is the `T[size?]` index shape; the call args are the elements.
        if let Expr::Call {
            callee,
            args: values,
            ..
        } = operand
            && let Expr::Index { base, args: sz, .. } = &**callee
            && let Some(elem) = self.array_elem_ty(base, src)
        {
            return self.lower_array_new_init(elem, sz.first(), values, src);
        }
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
            // Apply constant field defaults (`int32 v = 9;`) for this class and its
            // bases before any constructor runs, so a ctor body can override them.
            self.emit_field_inits(p.clone(), id);
            // Implicitly chain the parameterless base constructors, root-first, so
            // inherited fields are initialized before this class's own ctor runs.
            // (Explicit `: base(args)` chaining isn't parsed yet, so every ctor is
            // implicit — this is the whole chain.)
            let mut chain: Vec<StructId> = Vec::new();
            let mut bid = self.structs.bases[id.0 as usize];
            while let Some(b) = bid {
                chain.push(b);
                bid = self.structs.bases[b.0 as usize];
            }
            for &b in chain.iter().rev() {
                if let Some(ctor) = self.structs.ctor_for(b, 0) {
                    self.fb.call(ctor.full_name, vec![p.clone()], IrType::Void);
                }
            }
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

    /// Lower an interpolated string `$"…{expr}…"` to a freshly-allocated
    /// `String`: `new String()`, then append each literal run (byte-by-byte) and
    /// each hole's value through the type-matched `String.Append` overload —
    /// `Append(String)` for a `String`, `Append(char8)` for a `char8`,
    /// `Append(int)` for any integer (widened to `int`). Other hole types are
    /// evaluated for effect and skipped (no `Append` overload yet). The result is
    /// a `String` reference the caller owns (must `delete`), like a target-typed
    /// string literal.
    fn lower_interp(&mut self, parts: &[InterpPart], src: &str) -> (Value, IrType) {
        let Some(id) = self.structs.by_name.get("String").copied() else {
            return (undef(IrType::Ptr), IrType::Ptr);
        };
        // new String(): malloc the body, null the header, run the 0-arg ctor.
        let size = self.fb.size_of(id);
        let s = self.fb.call("malloc", vec![size], IrType::Ref(id));
        let hdr = self.fb.field_addr(s.clone(), id, 0);
        self.fb.store(hdr, Value::Const(Const::Null));
        if let Some(ctor) = self.structs.ctor_for(id, 0) {
            self.fb.call(ctor.full_name, vec![s.clone()], IrType::Void);
        }

        for part in parts {
            match part {
                InterpPart::Lit(text) => {
                    // Append each UTF-8 byte as a char8 (the corlib String is
                    // byte-based), so multibyte text round-trips like a `char8*`.
                    for &byte in text.as_bytes() {
                        self.append_to_string(s.clone(), id, Value::int(byte as i128, IrType::U8), IrType::U8);
                    }
                }
                InterpPart::Hole(e) => {
                    let (v, t) = self.expr(e, src);
                    match t {
                        IrType::Ref(rid) if rid == id => {
                            self.append_to_string(s.clone(), id, v, IrType::Ref(id));
                        }
                        IrType::U8 => {
                            self.append_to_string(s.clone(), id, v, IrType::U8);
                        }
                        IrType::Int { .. } => {
                            let w = self.coerce(v, t, IrType::I64);
                            self.append_to_string(s.clone(), id, w, IrType::I64);
                        }
                        IrType::Bool => {
                            self.append_to_string(s.clone(), id, v, IrType::Bool);
                        }
                        // No matching Append overload (float, refs…): the value
                        // was evaluated above for its effects; skip it.
                        _ => {}
                    }
                }
            }
        }
        (s, IrType::Ref(id))
    }

    /// Call the `String.Append` overload whose argument type is `arg_ty`,
    /// passing `s` as the receiver. A no-op if no such overload exists.
    fn append_to_string(&mut self, s: Value, id: StructId, arg: Value, arg_ty: IrType) {
        let sig = self.structs.methods[id.0 as usize]
            .get("Append")
            .and_then(|cands| cands.iter().find(|m| m.params.get(1) == Some(&arg_ty)))
            .cloned();
        if let Some(sig) = sig {
            self.fb.call(sig.full_name, vec![s, arg], IrType::Void);
        }
    }

    /// `delete x` → `free(x)`. The destructor is deferred (a later sprint).
    fn lower_delete(&mut self, operand: &Expr, src: &str) -> (Value, IrType) {
        // A heap array's allocation base is 8 bytes before its elements pointer
        // (the length header), so free that, not the elements pointer.
        if let Expr::Ident(s) = operand
            && self.array_locals.contains(s.text(src))
        {
            let (ptr, _) = self.expr(operand, src);
            let base = self
                .fb
                .elem_addr(ptr, IrType::U8, Value::int(-8, IrType::I64));
            self.fb.call("free", vec![base], IrType::Void);
            return (Value::Const(Const::Undef(IrType::Void)), IrType::Void);
        }
        let (v, t) = self.expr(operand, src);
        if let IrType::Ref(id) = t {
            self.emit_destroy(v, id);
        } else {
            self.fb.call("free", vec![v], IrType::Void);
        }
        (Value::Const(Const::Undef(IrType::Void)), IrType::Void)
    }

    /// Run a class instance's destructor chain then free it: the derived dtor
    /// first, each base's next, root last (reverse of construction order), then
    /// `free`. Inheritance composes a base dtor into a derived that declares
    /// none, so the same symbol can repeat down the chain — dedup to call once.
    /// Shared by `delete` and `scope`-lifetime cleanup.
    fn emit_destroy(&mut self, v: Value, id: StructId) {
        let mut seen: Vec<String> = Vec::new();
        let mut cur = Some(id);
        while let Some(cid) = cur {
            if let Some(dtor) = self.structs.dtor_of(cid)
                && !seen.contains(&dtor)
            {
                self.fb.call(dtor.clone(), vec![v.clone()], IrType::Void);
                seen.push(dtor);
            }
            cur = self.structs.bases[cid.0 as usize];
        }
        self.fb.call("free", vec![v], IrType::Void);
    }

    /// Lower a method call `receiver.Method(args)`. Resolves the receiver's
    /// type, looks up the method (this-aware), and emits a direct call — passing
    /// the receiver as `this` for an instance method. Degrades (evaluating args
    /// for their effects) when the method can't be resolved.
    /// Whether `id` names a synthetic tuple struct (so `(a, b)` against it builds
    /// a tuple rather than being mistaken for a named-struct target).
    fn is_tuple_struct(&self, id: StructId) -> bool {
        self.structs.defs[id.0 as usize].name.starts_with("$tuple$")
    }

    /// A tuple-typed local/field target (`(int32,int32) t = (3,4)`) builds the
    /// tuple with element coercion to the declared field types. `None` if the
    /// target isn't a tuple struct or the initializer isn't a tuple literal.
    fn try_target_typed_tuple(
        &mut self,
        target: IrType,
        e: &Expr,
        src: &str,
    ) -> Option<(Value, IrType)> {
        let IrType::Struct(id) = target else {
            return None;
        };
        if !self.is_tuple_struct(id) {
            return None;
        }
        let Expr::Tuple { elems, .. } = e else {
            return None;
        };
        self.build_tuple(Some(id), elems, src)
    }

    /// Build a tuple value: a synthetic value struct whose fields "0".."n-1" hold
    /// the elements. With a `target` (from a tuple-typed annotation) each element
    /// coerces to its declared field type; without one the shape is inferred from
    /// the element types and matched against a registered tuple. `None` if no
    /// matching tuple struct exists or the arity differs.
    fn build_tuple(
        &mut self,
        target: Option<StructId>,
        elems: &[Expr],
        src: &str,
    ) -> Option<(Value, IrType)> {
        let vals: Vec<(Value, IrType)> = elems.iter().map(|e| self.expr(e, src)).collect();
        let id = match target {
            Some(id) => id,
            None => {
                let etys: Vec<IrType> = vals.iter().map(|(_, t)| *t).collect();
                *self.structs.tuples.get(&type_codes(&etys))?
            }
        };
        let ftys: Vec<IrType> = self.structs.defs[id.0 as usize]
            .fields
            .iter()
            .map(|f| f.ty)
            .collect();
        if ftys.len() != vals.len() {
            return None;
        }
        let ty = IrType::Struct(id);
        let slot = self.fb.alloca(ty);
        for (i, (v, vt)) in vals.into_iter().enumerate() {
            let cv = self.coerce(v, vt, ftys[i]);
            let fp = self.fb.field_addr(slot.clone(), id, i as u32);
            self.fb.store(fp, cv);
        }
        Some((self.fb.load(slot, ty), ty))
    }

    /// A target-typed `.(args)` constructor-invocation shorthand against a
    /// value-struct `target` (e.g. `Vec2 v = .(2, 3)` / `return .(x, y)`).
    /// `None` unless `target` is a value struct and `e` is the `.( … )` form
    /// (a `Call` whose callee is a bare-`.` `DotIdent`, distinct from a named
    /// `.Case(...)` enum constructor).
    fn try_target_typed_ctor(
        &mut self,
        target: IrType,
        e: &Expr,
        src: &str,
    ) -> Option<(Value, IrType)> {
        let IrType::Struct(id) = target else {
            return None;
        };
        let Expr::Call { callee, args, .. } = e else {
            return None;
        };
        let Expr::DotIdent { name, .. } = &**callee else {
            return None;
        };
        if name.text(src) != "." {
            return None; // `.Case(args)` is an enum constructor, not this
        }
        Some(self.construct_value_struct(id, args, src))
    }

    /// Apply constant field default initializers (`int32 v = 9;`) to the object
    /// at `obj` (a pointer to the body), for `id` and every base in its chain.
    /// Keyed by field name so it survives inheritance's field reindexing. Run at
    /// construction, before the constructor body, so a ctor can still override.
    fn emit_field_inits(&mut self, obj: Value, id: StructId) {
        // Collect (field name, const) for id and its bases (most-derived first;
        // names are unique across a hierarchy, so order doesn't matter).
        let mut inits: Vec<(String, Const)> = Vec::new();
        let mut cur = Some(id);
        while let Some(cid) = cur {
            if let Some(list) = self.structs.field_inits.get(&cid) {
                inits.extend(list.iter().cloned());
            }
            cur = self.structs.bases[cid.0 as usize];
        }
        for (name, c) in inits {
            if let Some(idx) = self.structs.defs[id.0 as usize]
                .fields
                .iter()
                .position(|f| f.name == name)
            {
                let fp = self.fb.field_addr(obj.clone(), id, idx as u32);
                self.fb.store(fp, Value::Const(c));
            }
        }
    }

    /// Stack-construct a value struct: alloca a slot, apply constant field
    /// defaults, run the arity-matched constructor through it (`this` is a pointer
    /// to the body), and load the initialized value. With no matching ctor the
    /// slot keeps its field defaults (or is left as-is when there are none).
    fn construct_value_struct(&mut self, id: StructId, args: &[Expr], src: &str) -> (Value, IrType) {
        let ty = IrType::Struct(id);
        let slot = self.fb.alloca(ty);
        self.emit_field_inits(slot.clone(), id);
        if let Some(ctor) = self.structs.ctor_for(id, args.len()) {
            let mut call_args = vec![slot.clone()];
            for (i, a) in args.iter().enumerate() {
                let (v, t) = self.expr(a, src);
                let pt = ctor.params.get(i + 1).copied().unwrap_or(t);
                call_args.push(self.coerce(v, t, pt));
            }
            self.fb.call(ctor.full_name, call_args, IrType::Void);
        }
        (self.fb.load(slot, ty), ty)
    }

    /// A target-typed `.{ field = value }` initializer on a `Stmt::Local` whose
    /// declared type is `target`. `None` if the init isn't an `Initializer`.
    fn try_target_typed_initializer(
        &mut self,
        target: IrType,
        e: &Expr,
        src: &str,
    ) -> Option<(Value, IrType)> {
        if let Expr::Initializer { base, entries, .. } = e {
            return Some(self.lower_initializer(base, entries, Some(target), src));
        }
        None
    }

    /// Lower an object/collection initializer: obtain the object (a fresh value-
    /// struct slot for a target-typed `.{ }`, or the reference/struct the `base`
    /// evaluates to — e.g. `new T()`), then store each `field = value` entry into
    /// the matching field. Returns the initialized value (the struct, or the ref).
    fn lower_initializer(
        &mut self,
        base: &Expr,
        entries: &[Expr],
        target: Option<IrType>,
        src: &str,
    ) -> (Value, IrType) {
        // Resolve the object to write into: `(write-pointer, struct id, is value
        // struct)`. A `.{ }` (DotIdent base) target-types to `target`; otherwise
        // the base is evaluated (a class ref writes in place; a struct value is
        // spilled to a slot so its fields are addressable).
        let (obj, id, is_value) = if matches!(base, Expr::DotIdent { .. }) {
            match target {
                Some(IrType::Struct(id)) => {
                    let slot = self.fb.alloca(IrType::Struct(id));
                    // Field defaults first; explicit `field = value` entries below
                    // (in assign_init_fields) override them.
                    self.emit_field_inits(slot.clone(), id);
                    (slot, id, true)
                }
                Some(IrType::Ref(id)) => {
                    let (v, _) = self.expr(base, src);
                    let _ = v;
                    return (undef(IrType::Ref(id)), IrType::Ref(id));
                }
                _ => return (undef(IrType::I64), IrType::I64),
            }
        } else {
            let (v, t) = self.expr(base, src);
            match t {
                IrType::Ref(id) => (v, id, false),
                IrType::Struct(id) => {
                    let slot = self.fb.alloca(t);
                    self.fb.store(slot.clone(), v);
                    (slot, id, true)
                }
                _ => return (v, t),
            }
        };
        self.assign_init_fields(obj.clone(), id, entries, src);
        if is_value {
            (self.fb.load(obj, IrType::Struct(id)), IrType::Struct(id))
        } else {
            (obj, IrType::Ref(id))
        }
    }

    /// Apply each initializer entry to the object at `obj` (a pointer to the
    /// struct body / the class reference). A `field = value` entry stores into the
    /// matching field (an object initializer); a bare value entry is added via the
    /// type's `Add` method when it has one (a collection initializer, e.g.
    /// `new List<int>() { 1, 2, 3 }`). Unrecognized entries are ignored.
    fn assign_init_fields(&mut self, obj: Value, id: StructId, entries: &[Expr], src: &str) {
        let fields: Vec<(String, IrType)> = self.structs.defs[id.0 as usize]
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.ty))
            .collect();
        let add_sigs = self.structs.methods[id.0 as usize].get("Add").cloned();
        for entry in entries {
            if let Expr::Assign { target: tgt, value, .. } = entry
                && let Expr::Ident(nm) = &**tgt
                && let Some(i) = fields.iter().position(|(n, _)| n == nm.text(src))
            {
                let (v, vt) = self.expr(value, src);
                let cv = self.coerce(v, vt, fields[i].1);
                let fp = self.fb.field_addr(obj.clone(), id, i as u32);
                self.fb.store(fp, cv);
            } else if let Some(cands) = &add_sigs {
                // Collection initializer: `obj.Add(entry)`.
                let (v, vt) = self.expr(entry, src);
                if let Some(sig) = pick_overload(cands, &[vt], true).cloned() {
                    let arg = self.coerce(v, vt, sig.params[1]);
                    self.fb.call(sig.full_name, vec![obj.clone(), arg], sig.ret);
                }
            }
        }
    }

    /// Lower a call argument. A `ref`/`out` argument is passed *by address*: the
    /// operand's lvalue (a pointer to its storage) becomes the argument value
    /// (typed `Ptr`), so the callee — whose matching param is also `Ptr` — can
    /// mutate the caller's variable. Every other argument is an ordinary value.
    fn arg_value(&mut self, a: &Expr, src: &str) -> (Value, IrType) {
        if let Expr::Prefix {
            kw: PrefixKw::Ref | PrefixKw::Out,
            operand,
            ..
        } = a
            && let Some((addr, _)) = self.lvalue(operand, src)
        {
            return (addr, IrType::Ptr);
        }
        self.expr(a, src)
    }

    /// Whether class `c` is `t` or a transitive subclass of it (walks `bases`).
    fn is_subtype_of(&self, c: StructId, t: StructId) -> bool {
        let mut cur = Some(c);
        while let Some(id) = cur {
            if id == t {
                return true;
            }
            cur = self.structs.bases[id.0 as usize];
        }
        false
    }

    /// The runtime type test behind `is`/`as`: true iff `obj`'s `$header` (its
    /// runtime vtable pointer) equals the vtable of `tid` or of any class derived
    /// from it — the set is fixed at compile time. Emitted as an OR-chain of
    /// pointer equalities. `None` if no class in `tid`'s subtree carries a vtable
    /// (e.g. a non-virtual class), since the header tag isn't available then.
    fn type_test(&mut self, obj: Value, oid: StructId, tid: StructId) -> Option<Value> {
        let targets: Vec<StructId> = (0..self.structs.defs.len() as u32)
            .map(StructId)
            .filter(|&c| self.is_subtype_of(c, tid) && !self.structs.vimpls[c.0 as usize].is_empty())
            .collect();
        if targets.is_empty() {
            return None;
        }
        let hdr_addr = self.fb.field_addr(obj, oid, 0);
        let hdr = self.fb.load(hdr_addr, IrType::Ptr);
        let mut acc: Option<Value> = None;
        for c in targets {
            let vt = self
                .fb
                .global_addr(vtable_name(&self.structs.prefixes[c.0 as usize]));
            let eq = self.fb.cmp(CmpPred::Eq, hdr.clone(), vt);
            acc = Some(match acc {
                None => eq,
                Some(a) => self.fb.bin(IrBin::Or, a, eq, IrType::Bool),
            });
        }
        acc
    }

    /// Resolve a type-name expression (the RHS of `is`/`as`) to a class id.
    fn type_id_of(&self, ty_expr: &Expr, src: &str) -> Option<StructId> {
        match ty_expr {
            Expr::Ident(s) => self.structs.by_name.get(s.text(src)).copied(),
            Expr::Paren { inner, .. } => self.type_id_of(inner, src),
            _ => None,
        }
    }

    /// `obj is T` → a `bool`. False when `obj` isn't a reference, `T` isn't a
    /// known class, or the test can't be expressed via vtable tags.
    fn lower_is(&mut self, lhs: &Expr, rhs: &Expr, src: &str) -> (Value, IrType) {
        let (ov, ot) = self.expr(lhs, src);
        if let IrType::Ref(oid) = ot
            && let Some(tid) = self.type_id_of(rhs, src)
            && let Some(test) = self.type_test(ov, oid, tid)
        {
            return (test, IrType::Bool);
        }
        (Value::bool(false), IrType::Bool)
    }

    /// `obj as T` → `obj` typed as `T` when the runtime type matches, else `null`.
    fn lower_as(&mut self, lhs: &Expr, rhs: &Expr, src: &str) -> (Value, IrType) {
        let (ov, ot) = self.expr(lhs, src);
        if let IrType::Ref(oid) = ot
            && let Some(tid) = self.type_id_of(rhs, src)
            && let Some(test) = self.type_test(ov.clone(), oid, tid)
        {
            let null = Value::Const(Const::Null);
            let r = self.fb.select(test, ov, null, IrType::Ref(tid));
            return (r, IrType::Ref(tid));
        }
        (Value::Const(Const::Null), IrType::Ptr)
    }

    /// `a?.field` — evaluate `a` once; if it's null the result is the field's
    /// default (`null`/`0`), else `a.field`. Lowered as a null test + branch with
    /// a phi join. Exactly right for a reference-typed field (null default); for a
    /// value field it yields `0` rather than Beef's `T?` (a documented
    /// simplification). Falls back to a plain read if `a` isn't a reference or the
    /// member isn't a stored field.
    fn lower_conditional_member(&mut self, base: &Expr, name: &str, src: &str) -> (Value, IrType) {
        let (bv, bt) = self.expr(base, src);
        let IrType::Ref(id) = bt else {
            // Non-reference base can't be null here — read the member plainly.
            return self.member_field_on(bv, bt, name);
        };
        // Resolve the field's index + type from the reference's layout.
        let field = self.structs.defs[id.0 as usize]
            .fields
            .iter()
            .position(|f| f.name == name)
            .map(|i| (i as u32, self.structs.defs[id.0 as usize].fields[i].ty));
        let Some((idx, fty)) = field else {
            // Not a plain field (a property/method) — degrade to a guarded plain
            // read isn't worth it; just read non-conditionally on the value.
            return self.member_field_on(bv, bt, name);
        };
        let is_null = self.fb.cmp(CmpPred::Eq, bv.clone(), Value::Const(Const::Null));
        let entry = self.fb.current_block();
        let nonnull_b = self.fb.create_block("qdot.nonnull");
        let join_b = self.fb.create_block("qdot.join");
        self.fb.cond_br(is_null, join_b, nonnull_b);
        self.switch(nonnull_b);
        let fp = self.fb.field_addr(bv, id, idx);
        let v = self.fb.load(fp, fty);
        let nn_end = self.fb.current_block();
        self.fb.br(join_b);
        self.switch(join_b);
        let r = self
            .fb
            .phi(vec![(entry, zero_of(fty)), (nn_end, v)], fty);
        (r, fty)
    }

    /// `a?.M(args)` — null-guard a method call. Evaluates `a` once for the null
    /// test; the non-null branch performs the ordinary call (which re-resolves the
    /// receiver — safe because `?.` is only short-circuited for a side-effect-free
    /// base: an identifier, `this`, or a member/index chain). The result joins the
    /// method's default on the null path. A `void` method needs no value.
    fn lower_conditional_call(
        &mut self,
        base: &Expr,
        mname: &str,
        args: &[Expr],
        src: &str,
    ) -> (Value, IrType) {
        let (bv, bt) = self.expr(base, src);
        if !matches!(bt, IrType::Ref(_)) || !is_idempotent(base) {
            // Can't null-guard (non-reference or effectful base) — call plainly.
            return self.lower_method_call(base, mname, args, src);
        }
        let is_null = self.fb.cmp(CmpPred::Eq, bv, Value::Const(Const::Null));
        let entry = self.fb.current_block();
        let nonnull_b = self.fb.create_block("qcall.nonnull");
        let join_b = self.fb.create_block("qcall.join");
        self.fb.cond_br(is_null, join_b, nonnull_b);
        self.switch(nonnull_b);
        let (rv, rty) = self.lower_method_call(base, mname, args, src);
        let nn_end = self.fb.current_block();
        self.fb.br(join_b);
        self.switch(join_b);
        if rty == IrType::Void {
            return (Value::Const(Const::Undef(IrType::Void)), IrType::Void);
        }
        let r = self.fb.phi(vec![(entry, zero_of(rty)), (nn_end, rv)], rty);
        (r, rty)
    }

    /// Read field `name` directly from an already-evaluated base value (used when
    /// a `?.` base turns out to be non-null-able or the member isn't a field).
    fn member_field_on(&mut self, bv: Value, bt: IrType, name: &str) -> (Value, IrType) {
        let id = match bt {
            IrType::Ref(id) | IrType::Struct(id) => id,
            _ => return (undef(IrType::I64), IrType::I64),
        };
        let body = if matches!(bt, IrType::Ref(_)) {
            bv
        } else {
            // A struct value has no address here; defaulting keeps the IR valid.
            return (undef(IrType::I64), IrType::I64);
        };
        match self.structs.defs[id.0 as usize]
            .fields
            .iter()
            .position(|f| f.name == name)
        {
            Some(i) => {
                let fty = self.structs.defs[id.0 as usize].fields[i].ty;
                let fp = self.fb.field_addr(body, id, i as u32);
                (self.fb.load(fp, fty), fty)
            }
            None => (undef(IrType::I64), IrType::I64),
        }
    }

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
        let arg_vals: Vec<(Value, IrType)> = args.iter().map(|a| self.arg_value(a, src)).collect();
        let arg_tys: Vec<IrType> = arg_vals.iter().map(|(_, t)| *t).collect();

        // `base.Method(args)`: a *direct* (non-virtual) call to the nearest base
        // class's implementation of `Method`, with the current `this` as receiver
        // — so an `override` can chain to the parent without re-dispatching to
        // itself. Walks up the `bases` chain from the enclosing type.
        if let Expr::Base(_) = base
            && let Some((slot, this_ty @ IrType::Ref(cur_id))) = self.this_slot.clone()
        {
            let mut bid = self.structs.bases[cur_id.0 as usize];
            while let Some(id) = bid {
                if let Some(sig) = self.structs.methods[id.0 as usize]
                    .get(mname)
                    .and_then(|cands| pick_overload(cands, &arg_tys, true))
                    .cloned()
                {
                    let this_v = self.fb.load(slot, this_ty);
                    let mut call_args = vec![this_v];
                    for (i, (v, t)) in arg_vals.into_iter().enumerate() {
                        let pt = sig.params.get(i + 1).copied().unwrap_or(t);
                        call_args.push(self.coerce(v, t, pt));
                    }
                    let r = self.fb.call(sig.full_name, call_args, sig.ret);
                    return (r, sig.ret);
                }
                bid = self.structs.bases[id.0 as usize];
            }
        }

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
                let call_args: Vec<Value> = if let Some(elem) = sig.variadic {
                    self.pack_variadic_args(&sig.params, elem, arg_vals.clone())
                } else {
                    arg_vals
                        .iter()
                        .cloned()
                        .enumerate()
                        .map(|(i, (v, t))| self.coerce(v, t, sig.params[i]))
                        .collect()
                };
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
            if sig.is_instance {
                call_args.push(body_ptr.clone());
            }
            if let Some(elem) = sig.variadic {
                // Pack overflow args into a `T[]` (formal params exclude `this`).
                let formal = if sig.is_instance {
                    &sig.params[1..]
                } else {
                    &sig.params[..]
                };
                let packed = self.pack_variadic_args(formal, elem, arg_vals);
                call_args.extend(packed);
            } else {
                let mut pidx = if sig.is_instance { 1 } else { 0 };
                for (v, t) in arg_vals {
                    let pt = sig.params.get(pidx).copied().unwrap_or(t);
                    call_args.push(self.coerce(v, t, pt));
                    pidx += 1;
                }
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
        // Plain `=` to a known place: resolve the place first so the RHS can be
        // target-typed against it (`.(args)`/`.Case`/`.{ }`/tuple construct
        // against the field/local type, exactly as a typed local-init does).
        if matches!(op, AssignOp::Assign)
            && let Some((slot, ty)) = self.lvalue(target, src)
        {
            let (rhs, rhs_ty) = self
                .try_target_typed_enum(ty, value, src)
                .or_else(|| self.try_target_typed_ctor(ty, value, src))
                .or_else(|| self.try_target_typed_tuple(ty, value, src))
                .or_else(|| self.try_target_typed_initializer(ty, value, src))
                .unwrap_or_else(|| self.expr(value, src));
            let rhs = self.coerce(rhs, rhs_ty, ty);
            self.fb.store(slot, rhs.clone());
            return (rhs, ty);
        }
        let (rhs, rhs_ty) = self.expr(value, src);
        // Resolve the target to a place (local slot or struct field). The
        // stored value takes the place's type so later loads stay consistent.
        if let Some((slot, ty)) = self.lvalue(target, src) {
            let rhs = self.coerce(rhs, rhs_ty, ty);
            // `a ??= b`: store `b` only when `a` is currently null; the result is
            // the final value of `a`. (`b` is eagerly evaluated, as with the other
            // compound forms.) Only the local/field slot path is handled here.
            if matches!(op, AssignOp::NullCoalesce) {
                let cur = self.fb.load(slot.clone(), ty);
                let is_null = self.fb.cmp(CmpPred::Eq, cur.clone(), zero_of(ty));
                let from = self.fb.current_block();
                let assign_b = self.fb.create_block("nca.assign");
                let join_b = self.fb.create_block("nca.join");
                self.fb.cond_br(is_null, assign_b, join_b);
                self.switch(assign_b);
                self.fb.store(slot.clone(), rhs.clone());
                self.fb.br(join_b);
                self.switch(join_b);
                let r = self.fb.phi(vec![(from, cur), (assign_b, rhs)], ty);
                return (r, ty);
            }
            let stored = match compound_op(op) {
                Some(astbin) => {
                    let cur = self.fb.load(slot.clone(), ty);
                    // A struct/class `v op= w` uses the `operator op` overload if
                    // defined; scalars take the numeric `arith` path.
                    if matches!(ty, IrType::Struct(_) | IrType::Ref(_))
                        && let Some(sym) = operator_symbol(astbin)
                        && let Some((res, _)) =
                            self.try_operator_overload(sym, cur.clone(), ty, rhs.clone(), ty)
                    {
                        res
                    } else {
                        self.arith(astbin, cur, rhs, ty)
                    }
                }
                None => rhs, // plain `=`
            };
            self.fb.store(slot, stored.clone());
            return (stored, ty);
        }
        // Indexer assignment `obj[i] = v` → `set_this(obj, i, v)`.
        if matches!(op, AssignOp::Assign)
            && let Expr::Index { base, args, .. } = target
            && let Some(r) = self.try_indexer_set(base, args, rhs.clone(), rhs_ty, src)
        {
            return r;
        }
        // Compound indexer assignment `obj[i] op= v` → set the combined value.
        if let Some(astbin) = compound_op(op)
            && let Expr::Index { base, args, .. } = target
            && let Some(r) = self.try_indexer_compound(astbin, base, args, rhs.clone(), rhs_ty, src)
        {
            return r;
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
                    let combined = if matches!(pty, IrType::Struct(_) | IrType::Ref(_))
                        && let Some(sym) = operator_symbol(astbin)
                        && let Some((res, _)) =
                            self.try_operator_overload(sym, cur.clone(), pty, v.clone(), pty)
                    {
                        res
                    } else {
                        self.arith(astbin, cur, v, pty)
                    };
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

/// Whether evaluating `e` twice is safe (no side effects / no allocation): a bare
/// identifier, `this`, or a member/index/paren chain over such. Used to decide
/// whether a `?.M()` can re-evaluate its receiver in the non-null branch.
fn is_idempotent(e: &Expr) -> bool {
    match e {
        Expr::Ident(_) | Expr::This(_) => true,
        Expr::Paren { inner, .. } => is_idempotent(inner),
        Expr::Member { base, .. } => is_idempotent(base),
        Expr::Index { base, args, .. } => is_idempotent(base) && args.iter().all(is_idempotent),
        _ => false,
    }
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
/// Whether a member carries the `[Comptime]` attribute — marking it as
/// compile-time-only code that the comptime evaluator JIT-runs and folds into
/// literals at its call sites (rather than emitting into the final program).
fn has_comptime_attr(attrs: &[Attribute], src: &str) -> bool {
    attrs.iter().any(|a| {
        matches!(&a.name,
            AstType::Path { segments, .. }
                if segments.last().map(|s| s.name.text(src)) == Some("Comptime"))
    })
}

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
        // A heap array `T[]` records its element type so `a[i]` indexes through
        // the same typed-pointer path (the value is a pointer to the elements).
        AstType::Array { inner, .. } => Some(lower_ty_env(inner, src, structs, env)),
        AstType::Nullable { inner, .. } => pointer_elem_env(inner, src, structs, env),
        _ => None,
    }
}

/// A generic type-parameter environment: param name → the concrete IR type it
/// was monomorphized to. Empty for ordinary (non-generic) lowering.
type TyEnv<'a> = &'a [(String, IrType)];

/// Whether a parameter is passed by reference (`ref`/`out`): the caller passes
/// the address of an lvalue and the callee reads/writes through it. `ref` and
/// `out` are identical at the IR level — both a pointer to the caller's
/// storage; `out`'s definite-assignment requirement isn't enforced yet.
fn is_by_ref(p: &AstParam) -> bool {
    matches!(
        p.modifier,
        Some((ParamModifier::Ref | ParamModifier::Out, _))
    )
}

/// A parameter's IR type in ABI order: a by-ref (`ref`/`out`) parameter is a
/// raw pointer to the caller's storage; any other is its value type. Used at
/// every signature-building site so the mangled symbol and the call's coercions
/// agree on the pointer shape.
fn param_ir_ty(p: &AstParam, src: &str, structs: &StructTable, env: TyEnv) -> IrType {
    if is_by_ref(p) {
        IrType::Ptr
    } else {
        lower_ty_env(&p.ty, src, structs, env)
    }
}

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
        // A heap array `T[]` is a pointer to its elements (length-prefixed block).
        AstType::Array { .. } => IrType::Ptr,
        AstType::Nullable { inner, .. } => lower_ty_env(inner, src, structs, env),
        // A tuple resolves to its synthetic value struct (registered by the
        // pre-pass under the element `type_codes`). Unregistered ⇒ a pointer-
        // sized fallback (only reached for tuples in positions the pass skips).
        AstType::Tuple { elems, .. } => {
            let etys: Vec<IrType> = elems
                .iter()
                .map(|e| lower_ty_env(e, src, structs, env))
                .collect();
            structs
                .tuples
                .get(&type_codes(&etys))
                .map(|&id| IrType::Struct(id))
                .unwrap_or(IrType::Ptr)
        }
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

/// Evaluate a *constant* field default initializer (`int32 v = 9;`) to an IR
/// constant typed for the field `fty`. Handles the common literal forms (incl. a
/// negated numeric literal and parentheses); returns `None` for anything that
/// isn't a compile-time literal (a call / `new` / enum-const default — applied
/// at construction later, a follow-on).
fn const_field_init(init: &Expr, fty: IrType, src: &str) -> Option<Const> {
    match init {
        Expr::Int(s) => Some(Const::Int(parse_int(s.text(src)), fty)),
        Expr::Float(s) => Some(Const::Float(parse_float(s.text(src)), fty)),
        Expr::Bool(s) => Some(Const::Bool(s.text(src) == "true")),
        Expr::Char(s) => Some(Const::Int(decode_char_literal(s.text(src)), fty)),
        Expr::Null(_) => Some(Const::Null),
        Expr::Paren { inner, .. } => const_field_init(inner, fty, src),
        Expr::Unary {
            op: UnOp::Neg,
            operand,
            ..
        } => match &**operand {
            Expr::Int(s) => Some(Const::Int(-parse_int(s.text(src)), fty)),
            Expr::Float(s) => Some(Const::Float(-parse_float(s.text(src)), fty)),
            _ => None,
        },
        _ => None,
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
