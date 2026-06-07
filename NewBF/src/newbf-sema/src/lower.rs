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

/// Whether a registered type is a value `struct` (inline aggregate), a `class`
/// (heap object referenced by pointer), or an `interface` (a nominal,
/// pointer-like type with no layout — no `$header`, no fields; never `new`'d).
/// An interface-typed value is an `IrType::Ref(iface_id)`: a plain pointer to
/// *some* object body, carrying the interface's nominal id at the sema level
/// (see itables.md §4).
#[derive(Clone, Copy)]
enum StructKind {
    Value,
    Ref,
    Interface,
}

fn struct_kind(td: &TypeDecl) -> Option<StructKind> {
    match td.kind {
        TypeKind::Struct => Some(StructKind::Value),
        TypeKind::Class => Some(StructKind::Ref),
        TypeKind::Interface => Some(StructKind::Interface),
        _ => None, // enum / extension — not yet
    }
}

/// Type layouts collected before lowering: simple-name → id, the per-id kind
/// (value vs reference), and field lists (mirrored into [`Module::structs`] for
/// the backend). Two passes so a field whose type is another registered type
/// resolves.
#[derive(Default)]
struct StructTable {
    /// The well-known `$Func` value-struct id (`{ code: Ptr, target: Ptr }`),
    /// registered FIRST in [`StructTable::build`] so it is genuinely
    /// `StructId(0)`. A function value in a *closure-carrying* position (param /
    /// local / return) lowers to `Struct(func_struct)` via [`lower_value_ty`];
    /// C-ABI function-pointer positions (fields/casts/externs) stay bare `Ptr`.
    /// NOTE on the default-id hazard: `StructTable` derives `Default`, so an
    /// unset `func_struct` would silently be `StructId(0)`. Registering `$Func`
    /// first makes `StructId(0)` genuinely `$Func`; `build` asserts this.
    func_struct: StructId,
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
    // --- itables (dynamic interface dispatch, itables.md §4) ---
    // The HashMaps default-construct via `#[derive(Default)]`; the per-id Vec
    // fields (`iface_bases`/`imethods`/`idefaults`) are pushed in lockstep at
    // every id-minting site (register_func_struct/register_mono/
    // register_type_struct + the payload-enum site). Populated by IT-T2/T3;
    // declared (and kept in lockstep) here in IT-T1.
    /// Per class id: the interfaces it implements, transitively flattened and
    /// dedup'd, deterministic order. Empty for value structs and interfaces.
    /// Populated in IT-T2 (`collect_iface_bases`); read in IT-T3 (`apply_itables`).
    iface_bases: Vec<Vec<StructId>>,
    /// Per interface id: its instance, NON-GENERIC method slot signature,
    /// (name, sig) in declaration order (base-interface methods first). Drives
    /// slot layout and the method->index lookup at dispatch. Populated in IT-T2
    /// (`fill_iface_members`); read in IT-T3 (`apply_itables`).
    imethods: Vec<Vec<(String, MethodSig)>>,
    /// Per interface id: a default-body symbol per slot (`Some` for a default
    /// interface method, `None` for an abstract one), parallel to `imethods`.
    /// Populated in IT-T2; read in IT-T3 (`apply_itables`, resolution step 3).
    idefaults: Vec<Vec<Option<String>>>,
    /// Explicit interface implementations: (class id, iface id, method name)
    /// -> the impl MethodSig. Consulted by `apply_itables` before the implicit
    /// same-name `pick_overload`. Filled from `Member::Method.explicit_iface`
    /// in IT-T2; read in IT-T3.
    explicit_impls: HashMap<(StructId, StructId, String), MethodSig>,
    /// Global per-interface vtable slot base: interface id -> first vtable slot
    /// every implementer reserves for it. Stable across all implementers.
    /// Computed in IT-T3 (`apply_itables`).
    iface_slot_base: HashMap<StructId, usize>,
    /// Monomorphized generic instantiations to lower: `(mono id, generic type
    /// name, type-parameter env)`. `lower_program` re-finds each generic decl by
    /// name and lowers its methods at the mono id/prefix with the env.
    monos: Vec<MonoRecord>,
    /// Generic-method monomorphs: composite key `(owner, name, type_codes)` ->
    /// lowered signature, so a call site `Identity<int>(x)` resolves to a direct
    /// call. Keyed by the triple (not the mangled string) so two owners can't
    /// alias; in GM-A1 `owner` is always `None`.
    gen_method_sigs: HashMap<GenMKey, MethodSig>,
    /// One `GenMethodMono` per generic-method instantiation, so lowering
    /// re-finds the decl and emits its body.
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
    /// FV-T6b: resolved inner signature `(ret, ptys)` for each INLINE-arg lambda
    /// symbol (`$lambdaN`). An inline lambda (`xs.Map<int32>(x => x*10)`) has no
    /// declared `function`-typed local to supply its param types; they come from
    /// the callee parameter resolved at the call site. Filled via interior
    /// mutability when the call is lowered (the param sig is known then), and read
    /// back in `lower_program`'s emit pass to bind the lambda body's params at the
    /// right IR types. Absent ⇒ the lambda was never reached as a typed call arg
    /// (it emits with no params — a degenerate but well-typed body).
    inline_lambda_sigs: RefCell<HashMap<String, (IrType, Vec<IrType>)>>,
    /// Anonymous tuple types → the synthetic value-struct id backing each
    /// distinct shape, keyed by element `type_codes` (so `(int32, int32)`
    /// everywhere is one struct, fields named "0", "1", …). A pre-pass over
    /// type positions registers them; `lower_ty_env` resolves a `Tuple` here.
    tuples: HashMap<String, StructId>,
    /// Local (nested) functions: the declaration's name span → its emitted free-
    /// function symbol. A pre-pass assigns each `$localfn{N}`; the body lowers it
    /// and a same-method call resolves the name to a direct call.
    local_fn_syms: HashMap<Span, String>,
    /// FV-T4: static method-ref thunks to emit. A `Type.M` value reference is
    /// wrapped by a `$mref$<full>($self /*ignored*/, P…){ return <full>(P…); }`
    /// thunk so it fits the uniform `code(target, args…)` convention (the real
    /// `<full>` has no `$self` param). Keyed by full method symbol (the thunk's
    /// callee) to de-dup; the value is `(thunk symbol, full method symbol, ret,
    /// param types)`. Filled via interior mutability at the reference site (where
    /// only `&StructTable` is held) and drained in the emit pass.
    method_ref_thunks: RefCell<HashMap<String, MethodRefThunk>>,
    /// Per-type field default initializers (`int32 v = 9;`) that are *constant*
    /// literals, keyed by struct id → list of (field name, constant). Applied at
    /// construction (before the constructor body) by name, so they survive
    /// inheritance's field reindexing. Non-constant inits (calls/`new`) aren't
    /// captured yet.
    field_inits: HashMap<StructId, Vec<(String, Const)>>,
}

/// A monomorphization to lower: `(mono id, generic type name, type-param env)`.
type MonoRecord = (StructId, String, Vec<(String, IrType)>);

/// A method-ref thunk to emit, absorbing the uniform `code(target, args…)`
/// convention's hidden leading `$self` (param 0):
///   - **static** (FV-T4): `$mref$<full>($self /*ignored*/, P…){ return <full>(P…); }`
///     — drops `$self` and tail-calls the static `<full>(P…)`. `target = null`.
///   - **bound** (FV-T5): `$mrefb$<full>($self, P…){ return ((T)$self).M(P…); }`
///     — forwards `$self` as the receiver `this` (the instance method's leading
///     parameter) and tail-calls `<full>($self, P…)`. `target = receiver body
///     pointer`. Class receivers only (a class `this` is a body `Ptr`, ABI-
///     identical to the `$self` `Ptr` the convention passes — no cast needed in
///     opaque-pointer IR; Risk 7.9 — value-struct/`mut`/`ref` receivers are
///     declined at the reference site, never reaching here).
#[derive(Clone)]
struct MethodRefThunk {
    /// The thunk's own symbol (`$mref$<full>` static, `$mrefb$<full>` bound) —
    /// the value (`code`) returned by the ref.
    thunk_sym: String,
    /// The wrapped method's real symbol (the thunk's callee).
    callee: String,
    /// The wrapped method's return type and explicit parameter types (NOT
    /// including a bound method's leading `this`).
    ret: IrType,
    params: Vec<IrType>,
    /// `true` for a bound instance method ref: the thunk forwards `$self` as the
    /// method's leading `this`. `false` for a static ref (drops `$self`).
    bound: bool,
}

/// Composite resolution key for a generic-method monomorph:
/// `(owner, method name, type_codes(args))`. Keying by the triple (rather than
/// the mangled symbol) means resolution never re-parses a symbol and two owners
/// can never alias. In task GM-A1 the `owner` slot is always `None` (so the
/// produced key/symbol is byte-identical to the old name-only mangling); later
/// tasks fill it from the enclosing/receiver type.
type GenMKey = (Option<StructId>, String, String);

/// A generic-method monomorph to lower. `lower_program` re-finds the decl via
/// `(owner, name)` and emits its body with `env` (its type-param bindings).
/// `sym` is the owner-mangled IR symbol used as the function name and call
/// target. In task GM-A1 `owner` is always `None`, so `sym` equals the old
/// name-only `mangle_generic` output.
struct GenMethodMono {
    owner: Option<StructId>,
    /// Owner-mangled IR symbol (the lowered function's name / call target).
    sym: String,
    /// Template method name (used to re-find the decl for emission).
    name: String,
    /// The method's own type-parameter bindings.
    env: Vec<(String, IrType)>,
}

impl StructTable {
    fn build(files: &[SourceFile<'_>]) -> Self {
        let mut t = StructTable::default();
        // 0. Register the well-known `$Func` value-struct FIRST so it gets
        //    `StructId(0)` and `func_struct` genuinely points at it (the
        //    default-id hazard: `func_struct` defaults to `StructId(0)`, so the
        //    default MUST be a real `$Func`). `$Func` = `{ code: Ptr, target:
        //    Ptr }` is the uniform two-word function-value representation used in
        //    closure-carrying positions (param/local/return) by `lower_value_ty`.
        //    Registering it first shifts every subsequent struct's id by one,
        //    which is safe: ids are only ever obtained via registration
        //    (`StructId(t.defs.len())`) or `by_name`/loop indices — nothing
        //    hardcodes a numeric id.
        t.func_struct = register_func_struct(&mut t);
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
            index_generic_methods(&f.unit.items, f.src, &t, &mut gmethods);
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
        // 4c-bis. GM-B1: now the full type-mono table (`t.monos`) exists, index
        //     each type monomorph's generic methods under `(Some(mono_id), name)`
        //     using the *template's* decl, so an instance generic call on a
        //     generic owner (`List<int32>.Map<R>`) resolves its decl (collection
        //     below) and emission can re-find it. Must follow step 4 (monos
        //     filled) and precede the second collection pass that records them.
        index_gmethods_on_monos(&t.monos, &generics, &mut gmethods);
        // 4d. Second generic-instantiation pass, now that fields are filled
        //     (GM-A3a). A *field-receiver* instance generic call `this.f.M<T>(…)`
        //     needs `f`'s declared type, which `instance_recv_owner` reads from
        //     `t.defs[owner].fields` — empty during step 3. Also records
        //     generic-method monomorphs on generic owners (GM-B1), now that the
        //     `(Some(mono_id), name)` decl entries exist (4c-bis above) and the
        //     owner-mono env is available in `t.monos` for the combined env.
        //     Re-running with the SAME `seen` set means no type monomorph is
        //     re-registered (all were discovered syntactically in step 3, before
        //     fields mattered), and the gen-method dedup
        //     (`gen_method_sigs.contains_key`) appends only the newly-resolvable
        //     field-receiver / generic-owner monomorphs. The throwaway `monos`
        //     stays empty (everything is already `seen`).
        let mut monos2: MonoList = Vec::new();
        for f in files {
            collect_insts_items(
                &f.unit.items,
                f.src,
                &generics,
                &gmethods,
                &mut t,
                &mut seen,
                &mut monos2,
                &[],
            );
        }
        debug_assert!(
            monos2.is_empty(),
            "second collection pass registered {} unexpected type monomorphs",
            monos2.len()
        );
        // 4e. Itables (IT-T2): populate the interface data tables now that every
        //     type's name/id and member layout exist, and BEFORE inheritance /
        //     vtable composition (so IT-T3's `apply_itables` can read `imethods`
        //     and `iface_bases` after `apply_vtables`). `fill_iface_members`
        //     records each interface's instance/non-generic method slots (base-
        //     interface methods first, transitively flattened) into
        //     `imethods`/`idefaults`; `collect_iface_bases` routes each class's
        //     interface bases into `iface_bases` (transitively flattened, value
        //     structs and interfaces skipped). `explicit_impls` was filled by the
        //     `Member::Method` arm during the member fill above.
        fill_iface_members(files, &mut t);
        collect_iface_bases(files, &mut t);
        // 5. Compose single inheritance once every type's own layout is filled,
        //    then lay out vtables (which inherit/override across that hierarchy).
        apply_inheritance(&mut t);
        apply_vtables(&mut t);
        // 5b. Itables (IT-T3): compose each implemented interface's methods into
        //     the class vtables at a globally-fixed per-interface slot base. Runs
        //     immediately after `apply_vtables` so every class's `vimpls` length
        //     is final (including generic-class monomorphs) and `methods[class]`
        //     already includes inherited methods (`apply_inheritance` ran). After
        //     this, a class implementing an interface has the impl symbol sitting
        //     in its (grown, null-padded) vtable at the interface's slot base; the
        //     call site does not dispatch through it yet (that is IT-T5).
        apply_itables(&mut t);
        // Default-id hazard guard: `func_struct` must genuinely be the `$Func`
        // value-struct (registered first, at `StructId(0)`), with exactly two
        // `Ptr` fields. If this ever fails, an unset/aliased `func_struct` would
        // silently point at the wrong (or no) struct.
        let fd = &t.defs[t.func_struct.0 as usize];
        debug_assert_eq!(t.func_struct, StructId(0), "$Func must be StructId(0)");
        debug_assert_eq!(fd.name, "$Func", "func_struct must name the $Func struct");
        debug_assert!(
            fd.fields.len() == 2
                && fd.fields[0].ty == IrType::Ptr
                && fd.fields[1].ty == IrType::Ptr,
            "$Func must have exactly two Ptr fields (code, target)"
        );
        t
    }

    /// The IR type naming `name`: a value struct → `Struct(id)`, a class →
    /// `Ref(id)`, an interface → `Ref(id)` (pointer-like; the nominal id carries
    /// the interface identity). `None` if `name` isn't a registered type.
    fn ty_of(&self, name: &str) -> Option<IrType> {
        self.by_name
            .get(name)
            .map(|&id| match self.kinds[id.0 as usize] {
                StructKind::Value => IrType::Struct(id),
                // An interface-typed value is a plain pointer to some object
                // body (itables.md §4); it lowers to `Ref(iface_id)` just like
                // a class so coercion/ABI treat it uniformly as `ptr`.
                StructKind::Ref | StructKind::Interface => IrType::Ref(id),
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

    /// The ctor analogue of [`pick_overload_partial`] (§3.2), used ONLY under the
    /// `has_pending` fork — the non-pending path keeps the arity-only
    /// [`ctor_for`] fast path verbatim. Selects a ctor of `id` by arity plus the
    /// shape gate on pending slots: among ctors whose explicit-arity matches
    /// `arg_shapes.len()`, a concrete slot scores by [`type_affinity`] against the
    /// formal param (past the leading `this`), a *compatible* pending slot adds
    /// +1, and an *incompatible* pending slot DISQUALIFIES the ctor. Ties keep the
    /// first-registered ctor (`>` is strict). `None` if no ctor survives — the
    /// caller then diagnoses/recovers rather than constructing against a wrong
    /// param type. (Ctors are not variadic, so arity is an exact match.)
    // Used under the `has_pending` fork wired in TA-5 (`lower_new`) and TA-6
    // (`construct_value_struct`).
    fn ctor_for_partial(&self, id: StructId, arg_shapes: &[ArgShape]) -> Option<MethodSig> {
        let mut best: Option<(&MethodSig, u32)> = None;
        'ctor: for c in &self.ctors[id.0 as usize] {
            // Explicit params past the leading `this`; arity must match exactly.
            let Some(formal) = c.params.get(1..) else {
                continue;
            };
            if formal.len() != arg_shapes.len() {
                continue;
            }
            let mut raw: u32 = 0;
            for (f, s) in formal.iter().zip(arg_shapes) {
                raw += match s {
                    ArgShape::Concrete(a) => type_affinity(*f, *a),
                    ArgShape::Pending(kind) => {
                        if pending_shape_compatible(*kind, *f, self) {
                            1
                        } else {
                            continue 'ctor; // incompatible pending slot → disqualify
                        }
                    }
                };
            }
            if best.is_none_or(|(_, bs)| raw > bs) {
                best = Some((c, raw));
            }
        }
        best.map(|(c, _)| c.clone())
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
            // Generic *interfaces* are EXCLUDED (itables.md §6/§10): they stay on
            // the generic-constraint static path and are never monomorphized in
            // v1, so `IFaceD<int16>` must resolve to `Ptr` (the unregistered
            // fallback), not `Ref(mono_id)`. (Now that `struct_kind` returns
            // `Some(Interface)`, this exclusion must be explicit.)
            Item::Type(td)
                if !td.generic_params.is_empty()
                    && td.kind != TypeKind::Interface
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

/// Generic-method decl index: `(owner, name)` -> the overloads declared under
/// that owner+name (each `(member, src)`). The value is a `Vec` so a single
/// owner can carry multiple same-named overloads. GM-A2 keys each decl under
/// BOTH `(Some(owner), name)` (the enclosing type's id) AND `(None, name)` —
/// the latter is the *retained* bare-cross-class fallback bucket so a bare
/// cross-class static call (e.g. `list_hof.bf`'s `Map`/`Filter`/`Fold`, called
/// bare from another class) keeps resolving exactly as before.
type GenMethodDecls<'a> = HashMap<(Option<StructId>, String), Vec<(&'a Member, &'a str)>>;

/// Index generic methods (those with type parameters) keyed by `(owner, name)`,
/// paired with the owning file's `src`. Each decl is inserted under both its
/// enclosing type's id (`Some(owner)`) — resolved via `by_name`, populated in
/// the pre-pass step 1 before this runs — and under `(None, name)`, the retained
/// bare-cross-class fallback bucket.
fn index_generic_methods<'a>(
    items: &'a [Item],
    src: &'a str,
    t: &StructTable,
    out: &mut GenMethodDecls<'a>,
) {
    for item in items {
        match item {
            Item::Namespace {
                body: Some(body), ..
            } => index_generic_methods(body, src, t, out),
            Item::Type(td) => {
                // The enclosing type's registered id (None for an unregistered
                // type — e.g. a generic *template* or interface, which has no
                // concrete owner here; its decls still land in the None bucket).
                let owner = t.by_name.get(td.name.text(src)).copied();
                for m in &td.members {
                    if let Member::Method {
                        name,
                        generic_params,
                        ..
                    } = m
                        && !generic_params.is_empty()
                    {
                        let nm = name.text(src).to_string();
                        // Owner-keyed entry: same-class and qualified calls find
                        // the decl under the precise owner, fixing §107 collisions.
                        if let Some(owner) = owner {
                            out.entry((Some(owner), nm.clone()))
                                .or_default()
                                .push((m, src));
                        }
                        // Retained None bucket: bare cross-class static calls.
                        out.entry((None, nm)).or_default().push((m, src));
                    }
                }
            }
            _ => {}
        }
    }
}

/// GM-B1: index a generic *type* monomorph's generic methods under
/// `(Some(mono_id), name)`, pointing at the **template's** decl.
///
/// A generic method on a generic owner (`List<int64>.Map<R>`) has its decl on
/// the generic *template* `List<T>`, which is never registered (only its monos
/// are) — so `index_generic_methods` files that decl only in the `(None, name)`
/// bucket. To resolve an *instance* call `lst.Map<R>(…)` on a `List<int32>`
/// receiver, both collection (`record_method_inst`) and emission (the gen-method
/// loop) look the decl up under `(Some(owner_mono_id), name)`. This helper makes
/// that key resolve by re-using the *template's* `(member, src)` for every type
/// monomorph in `t.monos`, keyed at the monomorph's id.
///
/// **Ordering (doc §9 B1):** must run only *after* the full type-mono table
/// (`t.monos`) exists — a `List<int32>` mono may be registered late in source
/// order, so owner-mono prefixes can't be resolved during the first collection.
/// Run once after step 4 (build) and once before the gen-method emission loop
/// (lower_program). Idempotent re-runs only re-append identical `(member, src)`
/// pairs; overload arity selection at the use site stays correct.
fn index_gmethods_on_monos<'a>(
    monos: &[MonoRecord],
    generics: &GenericDecls<'a>,
    out: &mut GenMethodDecls<'a>,
) {
    for (mono_id, gen_name, _env) in monos {
        let Some(&(td, td_src)) = generics.get(gen_name) else {
            continue;
        };
        for m in &td.members {
            if let Member::Method {
                name,
                generic_params,
                ..
            } = m
                && !generic_params.is_empty()
            {
                out.entry((Some(*mono_id), name.text(td_src).to_string()))
                    .or_default()
                    .push((m, td_src));
            }
        }
    }
}

/// Register the well-known `$Func` value-struct `{ code: Ptr, target: Ptr }`
/// and return its id. Called FIRST in [`StructTable::build`] so it is
/// `StructId(0)` (see the `func_struct` field's default-id note). Both fields
/// are opaque `Ptr`, so there is one `$Func` layout independent of the function
/// signature (no monomorph explosion). Pushes onto every parallel per-id vector
/// (as the other registration helpers do) to keep the table well-formed.
fn register_func_struct(t: &mut StructTable) -> StructId {
    let id = StructId(t.defs.len() as u32);
    let fields = vec![
        FieldDef {
            name: "code".into(),
            ty: IrType::Ptr,
        },
        FieldDef {
            name: "target".into(),
            ty: IrType::Ptr,
        },
    ];
    let nfields = fields.len();
    t.defs.push(StructDef {
        name: "$Func".into(),
        fields,
    });
    t.kinds.push(StructKind::Value);
    t.prefixes.push("$Func.".into());
    t.ctors.push(Vec::new());
    t.dtors.push(None);
    t.methods.push(HashMap::new());
    t.field_elems.push(vec![None; nfields]);
    t.bases.push(None);
    t.virtuals.push(Vec::new());
    t.vslots.push(HashMap::new());
    t.vimpls.push(Vec::new());
    // itables (IT-T1): keep the per-id Vec fields in lockstep.
    t.iface_bases.push(Vec::new());
    t.imethods.push(Vec::new());
    t.idefaults.push(Vec::new());
    t.by_name.insert("$Func".into(), id);
    id
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
    // itables (IT-T1): keep the per-id Vec fields in lockstep.
    t.iface_bases.push(Vec::new());
    t.imethods.push(Vec::new());
    t.idefaults.push(Vec::new());
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

/// Whether two IR types are ABI-compatible for an itable slot (itables.md §5 T3):
/// equal types match, and any two pointer-likes match (all pointers/refs lower to
/// the same LLVM `ptr`, so a concrete impl may legitimately name a different
/// nominal id than the interface slot signature — e.g. `Ref(square)` vs
/// `Ref(ishape)` for `this`, or a `Ref(class)` arg vs an `Ref(iface)` formal).
/// A non-pointer scalar (int/float width, struct id) must match exactly, or
/// `call_indirect` through the slot would read/write the wrong ABI.
fn abi_compatible(a: IrType, b: IrType) -> bool {
    a == b || (a.is_pointer() && b.is_pointer())
}

/// IT-T3 — Compose itables into the class vtables (itables.md §4/§5/§9 T3).
///
/// For every CLASS, each implemented interface's methods are appended to the
/// class vtable at a **globally-fixed per-interface slot base**, so the concrete
/// class is not needed at a dispatch site (IT-T5): the slot for `(I, method k)`
/// is `iface_slot_base[I] + k` in every implementer.
///
/// Slot-base assignment (itables.md §4, exact): `N = max over ALL ids of
/// vimpls[c].len()` (the longest class vtable across the whole table, monomorphs
/// included). Walk interfaces in `StructId` order with cursor `base = N`; each
/// interface `I` gets `iface_slot_base[I] = base`, then `base += imethods[I]`.
/// Because every class's class block is `[0, vimpls[c].len()) ⊆ [0, N)`, no
/// interface block ever overlaps a class block or another interface block — so
/// growing each implementer's `vimpls` to cover its used iface slots (null-padding
/// any gap) is bounds-safe.
///
/// Per-class impl resolution for slot `(name, isig)` at index `k` of interface
/// `iface`: (1) `explicit_impls[(class, iface, name)]`; else (2) `pick_overload`
/// over `methods[class]` (which includes INHERITED methods — `apply_inheritance`
/// ran first); else (3) the interface default `idefaults[iface][k]`; else (4) an
/// empty-string placeholder (→ `const_null` slot via `emit_vtables`). Before
/// wiring a chosen impl its non-pointer param/return types are asserted equal to
/// the slot signature's (`abi_compatible`); a mismatch falls back to the null
/// placeholder rather than an ill-typed `call_indirect` target.
///
/// Value structs and interfaces themselves get no itable slots (a value struct
/// listing an interface base is boxing, out of scope — and has no vtable to
/// dispatch through). This composes only the slot tables; it changes no call site
/// (IT-T5) and emits no default-method body (IT-T6).
fn apply_itables(t: &mut StructTable) {
    let nids = t.vimpls.len();
    // (2) N = the longest class vtable across the WHOLE table (monomorphs
    // included), so every interface block sits strictly after every class block.
    let n = t.vimpls.iter().map(|v| v.len()).max().unwrap_or(0);
    // (2) Global per-interface slot base: walk interfaces in StructId order from
    // cursor `N`, reserving `imethods[I].len()` slots for each.
    let mut base = n;
    for i in 0..nids {
        let id = StructId(i as u32);
        if is_interface(t, id) {
            t.iface_slot_base.insert(id, base);
            base += t.imethods[i].len();
        }
    }
    // (3) Per-class composition. Iterate classes only; value structs/interfaces
    // are skipped (their `iface_bases` is empty by construction in IT-T2, but the
    // kind guard makes the intent explicit and tolerates a stray entry).
    for i in 0..nids {
        let class = StructId(i as u32);
        if !matches!(t.kinds[i], StructKind::Ref) {
            continue;
        }
        // Clone the per-class iface list so the loop body can borrow `t` mutably.
        let ifaces = t.iface_bases[i].clone();
        for iface in ifaces {
            let slot_base = t.iface_slot_base[&iface];
            // Bounds keystone (itables.md §4): the interface block starts at or
            // beyond this class's class block, so no interface slot overwrites a
            // class virtual slot (or another interface's block).
            debug_assert!(
                slot_base >= t.vimpls[i].len(),
                "iface block (base {slot_base}) overlaps class block (len {}) for class {i}",
                t.vimpls[i].len()
            );
            let slots = t.imethods[iface.0 as usize].clone();
            for (k, (name, isig)) in slots.iter().enumerate() {
                let target_slot = slot_base + k;
                // Resolve the impl symbol in priority order.
                let sym = resolve_itable_impl(t, class, iface, name, isig, k);
                // Grow `vimpls[class]` so `target_slot` is in range, null-padding
                // the gap (between the class block and the first iface block, and
                // between non-contiguous iface blocks) with empty strings —
                // `emit_vtables` lowers an empty entry to `const_null`.
                if t.vimpls[i].len() <= target_slot {
                    t.vimpls[i].resize(target_slot + 1, String::new());
                }
                t.vimpls[i][target_slot] = sym;
            }
        }
    }
}

/// Resolve the implementing symbol for interface slot `(name, isig)` (index `k`
/// of `iface`) on `class`, per itables.md §5 T3: explicit impl → same-name
/// overload (incl. inherited) → interface default → null placeholder. Returns the
/// impl symbol, or an empty string (→ null slot) when unresolved or ABI-mismatched.
fn resolve_itable_impl(
    t: &StructTable,
    class: StructId,
    iface: StructId,
    name: &str,
    isig: &MethodSig,
    k: usize,
) -> String {
    // The interface slot's formal params past the leading `this`.
    let formals: &[IrType] = if isig.params.is_empty() {
        &[]
    } else {
        &isig.params[1..]
    };
    // (1) Explicit interface implementation `Ret IFace.Member(…)`.
    if let Some(sig) = t
        .explicit_impls
        .get(&(class, iface, name.to_string()))
        .filter(|sig| itable_abi_matches(sig, isig))
    {
        return sig.full_name.clone();
    }
    // (2) Implicit same-name method on the class (incl. INHERITED methods —
    // `apply_inheritance` already merged the base's methods into `methods[class]`).
    if let Some(sig) = t.methods[class.0 as usize]
        .get(name)
        .and_then(|cands| pick_overload(cands, formals, /*members=*/ true))
        .filter(|sig| itable_abi_matches(sig, isig))
    {
        return sig.full_name.clone();
    }
    // (3) The interface's own default-body symbol, if this is a default method.
    if let Some(Some(default_sym)) = t.idefaults[iface.0 as usize].get(k) {
        return default_sym.clone();
    }
    // (4) Unresolved required slot → null placeholder. v1 policy (itables.md §4,
    // §10): a null slot segfaults cleanly if ever called, paired with a
    // composition-time diagnostic. `StructTable::build` has no diagnostic sink
    // (diagnostics live in the separate model-graph `resolve_and_check` pass), so
    // this surfaces as a `debug_assert!` — loud in test/debug builds, a graceful
    // null slot in release. Routing a real `Diagnostic` would require threading a
    // sink through `build` and every call site (out of IT-T3 scope).
    debug_assert!(
        false,
        "class {} (id {}) does not implement {}.{name}",
        t.prefixes[class.0 as usize],
        class.0,
        t.prefixes[iface.0 as usize],
    );
    String::new()
}

/// ABI assertion (itables.md §5 T3): a chosen impl's non-pointer param/return IR
/// types must equal the interface slot signature's; pointer-likes are
/// ABI-identical and may differ in nominal id (the leading `this` always does:
/// `Ref(class)` impl vs `Ref(iface)` slot). Arity must match too. A mismatch
/// (a sema/typing inconsistency) is logged loud in debug and rejected so the slot
/// falls back to the null placeholder instead of a wrong-typed `call_indirect`.
fn itable_abi_matches(sig: &MethodSig, isig: &MethodSig) -> bool {
    let ok = sig.params.len() == isig.params.len()
        && abi_compatible(sig.ret, isig.ret)
        && sig
            .params
            .iter()
            .zip(&isig.params)
            .all(|(&a, &b)| abi_compatible(a, b));
    debug_assert!(
        ok,
        "itable ABI mismatch: impl {} {:?}->{:?} vs slot {:?}->{:?}",
        sig.full_name, sig.params, sig.ret, isig.params, isig.ret
    );
    ok
}

// --- itables: interface members & bases (itables.md §5 T2) ---------------------

/// An interface's OWN (non-flattened) method slots, keyed by interface id: each
/// entry is `(method name, this-leading slot sig, default-body symbol)` where
/// the default symbol is `Some` for a bodied (default) method, `None` for an
/// abstract one. Built by `collect_iface_own`, consumed by
/// `compose_iface_members`.
type IfaceOwn = HashMap<StructId, Vec<(String, MethodSig, Option<String>)>>;

/// The transitively-flattened (base-first) interface slots per id: the
/// `(name, sig)` slot list paired with the parallel `idefaults` list. Produced
/// by `compose_iface_members`.
type IfaceComposed = HashMap<StructId, (Vec<(String, MethodSig)>, Vec<Option<String>>)>;

/// Per interface id: its direct interface-kind base ids (`interface IB : IA`).
type IfaceLinks = HashMap<StructId, Vec<StructId>>;

/// Whether `id` names an `interface`-kind registered type.
fn is_interface(t: &StructTable, id: StructId) -> bool {
    matches!(t.kinds[id.0 as usize], StructKind::Interface)
}

/// Populate `imethods`/`idefaults` for every interface: each interface's
/// **instance, NON-GENERIC** methods become its slot signature, in declaration
/// order, with **base-interface methods first** (transitive flattening). An
/// abstract (body-less) interface method is recorded directly — it does NOT go
/// through the class-method registration gate (which drops body-less members),
/// so a slot exists for dispatch even though no `full_name` is ever emitted. A
/// default (bodied) method is recorded too, with its default-body symbol in
/// `idefaults[id][k] = Some({IFace.prefix}{Method})`; an abstract one →
/// `None`. `static` and generic interface methods are filtered out so they
/// never consume a slot index (every implementer's layout must agree).
///
/// Defaults are deliberately NOT added to `methods[iface]` (itables.md §5 T2):
/// a class calling a default it overrides would otherwise resolve to a wrong
/// direct call. Defaults reach a class only through the itable slot (IT-T3/T6).
fn fill_iface_members(files: &[SourceFile<'_>], t: &mut StructTable) {
    // Collect each interface's own (non-flattened) method slots + its base
    // interface ids first, reading the AST. Keyed by interface id.
    let mut own: IfaceOwn = HashMap::new();
    let mut bases: IfaceLinks = HashMap::new();
    for f in files {
        collect_iface_own(&f.unit.items, f.src, t, &mut own, &mut bases);
    }
    // Flatten transitively (base-interface methods first), memoized by id.
    let ids: Vec<StructId> = own.keys().copied().collect();
    let mut composed: IfaceComposed = HashMap::new();
    for id in &ids {
        compose_iface_members(*id, &own, &bases, &mut composed);
    }
    for (id, (methods, defaults)) in composed {
        t.imethods[id.0 as usize] = methods;
        t.idefaults[id.0 as usize] = defaults;
    }
}

/// Walk all type decls (namespaces + nested), recording each interface's OWN
/// instance/non-generic method slots and its base-interface ids.
fn collect_iface_own(
    items: &[Item],
    src: &str,
    t: &StructTable,
    own: &mut IfaceOwn,
    bases: &mut IfaceLinks,
) {
    for item in items {
        match item {
            Item::Namespace {
                body: Some(body), ..
            } => collect_iface_own(body, src, t, own, bases),
            Item::Type(td) => collect_iface_own_type(td, src, t, own, bases),
            _ => {}
        }
    }
}

fn collect_iface_own_type(
    td: &TypeDecl,
    src: &str,
    t: &StructTable,
    own: &mut IfaceOwn,
    bases: &mut IfaceLinks,
) {
    if td.kind == TypeKind::Interface
        && td.generic_params.is_empty()
        && let Some(&id) = t.by_name.get(td.name.text(src))
    {
        // Base interfaces (`interface IB : IA`): only interface-kind bases.
        let mut bvec = Vec::new();
        for b in &td.bases {
            if let IrType::Ref(bid) = lower_ty_env(b, src, t, &[])
                && bid != id
                && is_interface(t, bid)
                && !bvec.contains(&bid)
            {
                bvec.push(bid);
            }
        }
        bases.insert(id, bvec);

        let mut slots: Vec<(String, MethodSig, Option<String>)> = Vec::new();
        for m in &td.members {
            if let Member::Method {
                return_ty,
                name,
                params,
                body,
                modifiers,
                generic_params,
                ..
            } = m
            {
                // Filter OUT static and generic interface methods: they stay on
                // the static/constraint path and must NOT consume slot indices.
                let is_static = modifiers
                    .iter()
                    .any(|(mo, _)| matches!(mo, Modifier::Static));
                if is_static || !generic_params.is_empty() {
                    continue;
                }
                let nm = name.text(src).to_string();
                // Full this-leading MethodSig: `this : Ref(iface_id)`, then the
                // explicit params; ret/params via lower_ty_env (Func$-widened in
                // value positions, matching the class-method path).
                let mut ps = vec![IrType::Ref(id)];
                for p in params {
                    ps.push(param_ir_ty(p, src, t, &[]));
                }
                let variadic = params
                    .last()
                    .filter(|p| matches!(p.modifier, Some((ParamModifier::Params, _))))
                    .and_then(|p| pointer_elem_env(&p.ty, src, t, &[]));
                // A default (bodied) interface method carries a symbol so the
                // itable slot can resolve to it; an abstract (body-less) one has
                // no emitted symbol and dispatches only through the slot.
                let default_sym = if matches!(body, MethodBody::None) {
                    None
                } else {
                    Some(format!("{}{}", t.prefixes[id.0 as usize], nm))
                };
                let sig = MethodSig {
                    // An abstract interface method's `full_name` is never emitted
                    // (it dispatches only via the slot); a default's matches the
                    // symbol IT-T6 will emit.
                    full_name: default_sym
                        .clone()
                        .unwrap_or_else(|| format!("{}{}", t.prefixes[id.0 as usize], nm)),
                    ret: lower_value_ty(return_ty, src, t, &[]),
                    params: ps,
                    is_instance: true,
                    variadic,
                    // Interface slot methods are non-generic and dispatched via
                    // the vtable, not target-typed for inline-lambda args.
                    param_fn_sigs: Vec::new(),
                };
                slots.push((nm, sig, default_sym));
            }
        }
        own.insert(id, slots);
    }
    for m in &td.members {
        if let Member::Nested(n) = m {
            collect_iface_own_type(n, src, t, own, bases);
        }
    }
}

/// Transitively compose an interface's flattened method slots: base-interface
/// methods first (in base-list order, each base recursively composed), then the
/// interface's own slots. Memoized by id (mirrors `compose_vtable`). A method
/// name already contributed by a base is NOT re-added (the base slot is reused),
/// so `interface IB : IA` with its own override of an `IA` method keeps a single
/// slot.
fn compose_iface_members(id: StructId, own: &IfaceOwn, bases: &IfaceLinks, out: &mut IfaceComposed) {
    if out.contains_key(&id) {
        return;
    }
    // Insert a placeholder first to guard against cyclic interface bases.
    out.insert(id, (Vec::new(), Vec::new()));
    let mut methods: Vec<(String, MethodSig)> = Vec::new();
    let mut defaults: Vec<Option<String>> = Vec::new();
    if let Some(bvec) = bases.get(&id) {
        for &b in bvec {
            compose_iface_members(b, own, bases, out);
            let (bm, bd) = out.get(&b).cloned().unwrap_or_default();
            for ((bn, bs), bdf) in bm.into_iter().zip(bd) {
                if !methods.iter().any(|(n, _)| *n == bn) {
                    methods.push((bn, bs));
                    defaults.push(bdf);
                }
            }
        }
    }
    if let Some(slots) = own.get(&id) {
        for (nm, sig, df) in slots {
            if let Some(pos) = methods.iter().position(|(n, _)| n == nm) {
                // Own declaration overrides a base slot (keeps the same index).
                methods[pos].1 = sig.clone();
                defaults[pos] = df.clone();
            } else {
                methods.push((nm.clone(), sig.clone()));
                defaults.push(df.clone());
            }
        }
    }
    out.insert(id, (methods, defaults));
}

/// Route each **class**'s interface bases into `iface_bases[class]`, transitively
/// flattened (each interface base contributes its own transitive interface
/// bases first) and dedup'd in a deterministic order. A `Ref`-kind (class) base
/// is the single inheritance base (already recorded by the guarded loop in
/// `fill_members_at`) — not an iface base. **Value structs and interfaces
/// themselves are SKIPPED**: a value struct listing an interface base has no
/// `$header`/vtable (boxing is out of scope) and must not enter `iface_bases`.
///
/// Interface→interface base links (`interface IB : IA`) are needed for the
/// transitive flatten; they are derived from the AST here (`iface_links`),
/// keyed by interface id, so no extra `StructTable` field is required.
fn collect_iface_bases(files: &[SourceFile<'_>], t: &mut StructTable) {
    // Per interface id: its direct interface-kind bases (from the AST), so a
    // class implementing `IB` drags in `IA` (`IB : IA`) transitively.
    let mut iface_links: IfaceLinks = HashMap::new();
    for f in files {
        collect_iface_links(&f.unit.items, f.src, t, &mut iface_links);
    }
    for f in files {
        collect_iface_bases_items(&f.unit.items, f.src, t, &iface_links);
    }
}

/// Record each interface's direct interface-kind base ids (`interface IB : IA`).
fn collect_iface_links(items: &[Item], src: &str, t: &StructTable, links: &mut IfaceLinks) {
    for item in items {
        match item {
            Item::Namespace {
                body: Some(body), ..
            } => collect_iface_links(body, src, t, links),
            Item::Type(td) => collect_iface_links_type(td, src, t, links),
            _ => {}
        }
    }
}

fn collect_iface_links_type(td: &TypeDecl, src: &str, t: &StructTable, links: &mut IfaceLinks) {
    if td.kind == TypeKind::Interface
        && td.generic_params.is_empty()
        && let Some(&id) = t.by_name.get(td.name.text(src))
    {
        let mut bvec = Vec::new();
        for b in &td.bases {
            if let IrType::Ref(bid) = lower_ty_env(b, src, t, &[])
                && bid != id
                && is_interface(t, bid)
                && !bvec.contains(&bid)
            {
                bvec.push(bid);
            }
        }
        links.insert(id, bvec);
    }
    for m in &td.members {
        if let Member::Nested(n) = m {
            collect_iface_links_type(n, src, t, links);
        }
    }
}

fn collect_iface_bases_items(items: &[Item], src: &str, t: &mut StructTable, links: &IfaceLinks) {
    for item in items {
        match item {
            Item::Namespace {
                body: Some(body), ..
            } => collect_iface_bases_items(body, src, t, links),
            Item::Type(td) => collect_iface_bases_type(td, src, t, links),
            _ => {}
        }
    }
}

fn collect_iface_bases_type(td: &TypeDecl, src: &str, t: &mut StructTable, links: &IfaceLinks) {
    // Only classes get itable slots: value structs (boxing out of scope) and
    // interfaces themselves are skipped.
    if td.kind == TypeKind::Class
        && td.generic_params.is_empty()
        && let Some(&id) = t.by_name.get(td.name.text(src))
    {
        let mut flat: Vec<StructId> = Vec::new();
        for b in &td.bases {
            if let IrType::Ref(bid) = lower_ty_env(b, src, t, &[])
                && bid != id
                && is_interface(t, bid)
            {
                add_iface_flat(bid, links, &mut flat);
            }
        }
        t.iface_bases[id.0 as usize] = flat;
    }
    for m in &td.members {
        if let Member::Nested(n) = m {
            collect_iface_bases_type(n, src, t, links);
        }
    }
}

/// Add interface `iid` and all its transitive interface bases to `flat`
/// (dedup'd, base-first): transitive bases first, then `iid` itself, so a class
/// implementing `IB : IA` orders `IA` before `IB`. Cycle-safe.
fn add_iface_flat(iid: StructId, links: &IfaceLinks, flat: &mut Vec<StructId>) {
    if flat.contains(&iid) {
        return;
    }
    if let Some(bvec) = links.get(&iid) {
        for &b in bvec {
            add_iface_flat(b, links, flat);
        }
    }
    if !flat.contains(&iid) {
        flat.push(iid);
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
        // The owner of a bare generic-method call inside a generic *template*
        // body is its monomorph — but owner-mono prefix resolution is deferred
        // to GM-B1 (collection order may not have the mono registered), so pass
        // `None` here: a bare call inside a generic body resolves via the
        // retained None bucket exactly as before.
        collect_insts_type(
            decl, decl_src, generics, gmethods, t, seen, monos, &inst_env, None,
        );
        monos.push((id, decl, decl_src, inst_env));
    }
    for a in args {
        use_in_type(a, src, generics, gmethods, t, seen, monos, env);
    }
}

/// Record the monomorph a generic-method call `Name<Args>(...)` demands under a
/// determined `owner`. Dedup is by presence in `gen_method_sigs` (no separate
/// seen-set needed). `owner` selects both the decl bucket and the mangled
/// symbol: `Some(id)` for a same-class/qualified call (decl lives on `id`),
/// `None` for a bare cross-class static (the retained fallback bucket).
fn record_method_inst(
    name: &str,
    targs: &[AstType],
    src: &str,
    gmethods: &GenMethodDecls,
    t: &mut StructTable,
    env: &[(String, IrType)],
    owner: Option<StructId>,
) {
    // Look up the `(owner, name)` bucket and pick the overload whose type-param
    // arity matches the call's type-args (mirrors `pick_overload`'s arity
    // discrimination; a single owner can carry same-named overloads).
    let Some(&(member, mdecl_src)) = gmethods.get(&(owner, name.to_string())).and_then(|v| {
        v.iter().find(|(m, _)| {
            matches!(m, Member::Method { generic_params, .. } if generic_params.len() == targs.len())
        })
    }) else {
        return;
    };
    let Member::Method {
        generic_params,
        params,
        return_ty,
        modifiers,
        attributes,
        ..
    } = member
    else {
        return;
    };
    // GM-A4 collection guards (generic-methods doc §1/§6). v1 deliberately does
    // NOT support certain generic-method shapes; recording a monomorph for them
    // would emit a *wrong* function (a `virtual`/`override` direct call that skips
    // dispatch, or a `[Comptime]` body that runs at runtime un-folded). Refuse to
    // record so no bad symbol is ever emitted — the call site already falls
    // through cleanly to a default value (no dangling call). `virtual`/`override`
    // additionally gets a loud declaration-level diagnostic in the analyze pass
    // (`check_generic_method_guards`); `[Comptime]`+generic stays a clean
    // documented no-garbage fallthrough here (it is legal Beef the corlib relies
    // on — only our v1 lowering can't instantiate-and-fold it).
    if modifiers
        .iter()
        .any(|(mo, _)| matches!(mo, Modifier::Virtual | Modifier::Override))
        || has_comptime_attr(attributes, mdecl_src)
    {
        return;
    }
    // Abstract-type-arg guard (doc §1, last bullet): a self/inner generic call
    // whose type-arg is an *unbound* type-parameter (`M<U>` where `U` is the
    // enclosing template's own parameter, not yet bound by a concrete `env`)
    // cannot be monomorphized here — it has no concrete type. Recording it would
    // mint a bogus `M$ptr` monomorph from the `Ptr` type-fallback. Refuse it; the
    // concrete monomorph (collected when `env` binds the parameter) records the
    // right symbol, and a concrete-arg self-call (`M<int32>`) is unaffected
    // because its arg is a registered/primitive type, never abstract.
    if targs.iter().any(|a| targ_is_abstract(a, src, t, env)) {
        return;
    }
    let argtys: Vec<IrType> = targs.iter().map(|a| lower_ty_env(a, src, t, env)).collect();
    if argtys.len() != generic_params.len() {
        return;
    }
    // The composite key's type-codes component plus the owner-mangled symbol
    // disambiguate same-named methods in different owners (fixes §107).
    let codes = type_codes(&argtys);
    let mangled = mangle_generic_method(owner, name, &argtys, t);
    let key: GenMKey = (owner, name.to_string(), codes);
    if t.gen_method_sigs.contains_key(&key) {
        return;
    }
    // The method's own type-parameter bindings (`R → int32`).
    let method_env: Vec<(String, IrType)> = generic_params
        .iter()
        .zip(&argtys)
        .map(|(gp, ty)| (gp.name.text(mdecl_src).to_string(), *ty))
        .collect();
    // GM-B1: when `owner` is itself a generic *type* monomorph (`List<int32>`),
    // its type-param bindings (`T → i32`) live in `t.monos` (a `MonoRecord`).
    // The method's emission env — and the env used to lower the method's sig
    // (params/return/variadic, e.g. `Map<R>`'s `function R(T) f` needs *both*
    // `T` and `R`) — is the OWNER mono's env followed by the method's own env.
    // A non-mono owner (a concrete class) contributes no bindings, so `env`
    // stays the method-only env (GM-A3a behaviour, byte-identical).
    let env: Vec<(String, IrType)> = match owner {
        Some(oid) => {
            let mut combined: Vec<(String, IrType)> = t
                .monos
                .iter()
                .find(|(id, _, _)| *id == oid)
                .map(|(_, _, e)| e.clone())
                .unwrap_or_default();
            combined.extend(method_env.iter().cloned());
            combined
        }
        None => method_env,
    };
    // GM-A3a: an instance generic method (non-static, declared on a *concrete*
    // owner) takes a leading `this: Ref(owner)` and is dispatched with a real
    // receiver. A `None`-bucket entry (bare cross-class static) and a static
    // method stay receiver-less. The ABI here is the single source of truth the
    // call site (prepend `this` iff `is_instance`) and emission (`this_ty`) read.
    let is_static = modifiers
        .iter()
        .any(|(mo, _)| matches!(mo, Modifier::Static));
    let is_instance = owner.is_some() && !is_static;
    let mut psig: Vec<IrType> = Vec::with_capacity(params.len() + 1);
    if let (true, Some(oid)) = (is_instance, owner) {
        psig.push(IrType::Ref(oid));
    }
    // FV-T3: a `function R(P)` param of a *generic* method (e.g. `Map<T,R>`'s
    // `function R(T) f`) is a closure-carrying position, so it lowers to `$Func`
    // via `param_ir_ty`/`lower_value_ty` (delegate-gated). This is the call-site
    // coercion target that auto-wraps a non-capturing/method-ref `Ptr` arg and
    // no-op-coerces a capturing-lambda `Func$` arg — the §49 fix path.
    psig.extend(params.iter().map(|p| param_ir_ty(p, mdecl_src, t, &env)));
    // A trailing `params T[]` makes the call site pack overflow args into a `T[]`.
    let variadic = params
        .last()
        .filter(|p| matches!(p.modifier, Some((ParamModifier::Params, _))))
        .and_then(|p| pointer_elem_env(&p.ty, mdecl_src, t, &env));
    let ret = lower_value_ty(return_ty, mdecl_src, t, &env);
    // FV-T6b: parallel inner `(ret, ptys)` for each explicit `function R(P)`
    // param of the generic method (e.g. `Map<R>`'s `function R(T) f`), so an
    // inline-lambda arg at `xs.Map<int32>(x => …)` is target-typed from the
    // resolved monomorph sig. Indexed by explicit param (no leading `this`).
    let fn_param_sigs: Vec<Option<(IrType, Vec<IrType>)>> = params
        .iter()
        .map(|p| param_fn_sig(p, mdecl_src, t, &env))
        .collect();
    let fn_param_sigs = if fn_param_sigs.iter().any(|s| s.is_some()) {
        fn_param_sigs
    } else {
        Vec::new()
    };
    t.gen_method_sigs.insert(
        key,
        MethodSig {
            full_name: mangled.clone(),
            ret,
            params: psig,
            is_instance,
            variadic,
            param_fn_sigs: fn_param_sigs,
        },
    );
    t.gen_method_monos.push(GenMethodMono {
        owner,
        sym: mangled,
        name: name.to_string(),
        env,
    });
}

/// Whether a generic-method call's type-argument is an **abstract** (unbound)
/// type parameter — a bare single-segment name that is neither bound by the
/// current monomorph `env`, nor a registered type, nor an int-/payload-enum,
/// nor a primitive keyword. Such an arg can only be the enclosing template's own
/// type parameter (`M<U>` inside `M<T>`), which has no concrete type at this
/// collection point. A concrete arg (`int32`, a registered class, a bound `T`)
/// is never abstract, so the supported concrete-arg self-call keeps working.
/// A compound type (`List<U>`, `U*`, `(U, int)`) is treated as concrete here —
/// it lowers to a registered mono / pointer / tuple, not a bare `Ptr` fallback.
fn targ_is_abstract(targ: &AstType, src: &str, t: &StructTable, env: &[(String, IrType)]) -> bool {
    let AstType::Path { segments, .. } = targ else {
        return false;
    };
    if segments.len() != 1 || !segments[0].args.is_empty() {
        return false;
    }
    let name = segments[0].name.text(src);
    // Bound by the current monomorph env, or a registered/enum/primitive type ⇒
    // concrete. Otherwise it is a free identifier — an unbound type parameter.
    let bound = env.iter().any(|(n, _)| n == name);
    let registered = t.by_name.contains_key(name)
        || t.enums.contains_key(name)
        || t.payload_enums.contains_key(name);
    !bound && !registered && !is_primitive_name(name)
}

/// Whether `name` is a built-in primitive type keyword (mirrors `primitive`).
fn is_primitive_name(name: &str) -> bool {
    matches!(
        name,
        "void"
            | "bool"
            | "int"
            | "int64"
            | "intptr"
            | "int8"
            | "int16"
            | "int32"
            | "uint"
            | "uint64"
            | "uintptr"
            | "uint8"
            | "char8"
            | "uint16"
            | "char16"
            | "uint32"
            | "char32"
            | "float"
            | "double"
    )
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
            Item::Type(td) => {
                // Owner identity for bare same-class generic-method calls inside
                // this type's bodies: the registered id of the enclosing type
                // (`None` for an unregistered type — e.g. a generic template).
                let cur_owner = t.by_name.get(td.name.text(src)).copied();
                collect_insts_type(td, src, generics, gmethods, t, seen, monos, env, cur_owner);
            }
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
    cur_owner: Option<StructId>,
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
                    let mut locals = param_locals(params, src, t, env);
                    collect_insts_stmt(
                        s, src, generics, gmethods, t, seen, monos, env, cur_owner, &mut locals,
                    );
                }
            }
            Member::Constructor { params, body, .. } => {
                for p in params {
                    use_in_type(&p.ty, src, generics, gmethods, t, seen, monos, env);
                }
                if let MethodBody::Block(s) = body {
                    let mut locals = param_locals(params, src, t, env);
                    collect_insts_stmt(
                        s, src, generics, gmethods, t, seen, monos, env, cur_owner, &mut locals,
                    );
                }
            }
            Member::Nested(n) => {
                // A nested type is its own enclosing owner for its bodies.
                let nested_owner = t.by_name.get(n.name.text(src)).copied();
                collect_insts_type(
                    n,
                    src,
                    generics,
                    gmethods,
                    t,
                    seen,
                    monos,
                    env,
                    nested_owner,
                );
            }
            _ => {}
        }
    }
}

/// Seed the collector's `locals` type scope from a method/ctor's parameters
/// (GM-A3a). Each named param contributes `(name, declared IR type)`, resolved
/// through the monomorph `env` exactly as `lower_method` binds it — so the
/// collector resolves an instance-call receiver to the same `StructId` the live
/// `Lowerer` scope will (R4). `ref`/`out` params bind to the pointee value type
/// (mirroring `lower_method`), and unnamed (`this`/discard) params are skipped.
fn param_locals(
    params: &[AstParam],
    src: &str,
    t: &StructTable,
    env: &[(String, IrType)],
) -> Vec<(String, IrType)> {
    params
        .iter()
        .filter_map(|p| {
            let nm = p.name?;
            Some((nm.text(src).to_string(), lower_ty_env(&p.ty, src, t, env)))
        })
        .collect()
}

/// Walk statement bodies for generic instantiations in local-declaration types
/// (`Box<int> b;`). Expression-position instantiations (`new Box<int>()`) arrive
/// with the generic *class* slice.
///
/// `locals` is the GM-A3a type scope (params + explicitly-typed `Stmt::Local`s)
/// used to resolve an instance generic-method call's receiver owner; blocks
/// truncate it back to entry length on exit (lexical scoping).
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
    cur_owner: Option<StructId>,
    locals: &mut Vec<(String, IrType)>,
) {
    match stmt {
        Stmt::Block { stmts, .. } => {
            // A nested block introduces its own scope: record its locals, then
            // truncate them off on exit so they don't leak to siblings.
            let mark = locals.len();
            for s in stmts {
                collect_insts_stmt(
                    s, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
                );
            }
            locals.truncate(mark);
        }
        Stmt::Local { ty, name, init, .. } => {
            if let Some(ty) = ty {
                use_in_type(ty, src, generics, gmethods, t, seen, monos, env);
                // Record an explicitly-typed local so an instance generic call on
                // it resolves its owner (GM-A3a). `var`/inferred locals (no `ty`)
                // are deliberately *not* recorded — a receiver they back is a
                // diagnosed shape, never silently mis-owned (doc §3.4).
                locals.push((name.text(src).to_string(), lower_ty_env(ty, src, t, env)));
            }
            if let Some(e) = init {
                collect_insts_expr(
                    e, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
                );
            }
        }
        Stmt::Locals { decls, .. } => {
            for d in decls {
                collect_insts_stmt(
                    d, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
                );
            }
        }
        Stmt::Expr { expr, .. } => collect_insts_expr(
            expr, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
        ),
        Stmt::Return { value: Some(e), .. } => collect_insts_expr(
            e, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
        ),
        Stmt::If {
            cond, then, els, ..
        } => {
            collect_insts_expr(
                cond, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
            );
            collect_insts_stmt(
                then, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
            );
            if let Some(e) = els {
                collect_insts_stmt(
                    e, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
                );
            }
        }
        Stmt::While { cond, body, .. } | Stmt::DoWhile { body, cond, .. } => {
            collect_insts_expr(
                cond, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
            );
            collect_insts_stmt(
                body, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
            );
        }
        Stmt::ForEach { iter, body, .. } => {
            collect_insts_expr(
                iter, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
            );
            collect_insts_stmt(
                body, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
            );
        }
        Stmt::Defer { body, .. } => collect_insts_stmt(
            body, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
        ),
        Stmt::For {
            init,
            init_extra,
            cond,
            update,
            update_extra,
            body,
            ..
        } => {
            let mark = locals.len();
            if let Some(i) = init {
                collect_insts_stmt(
                    i, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
                );
            }
            for s in init_extra {
                collect_insts_stmt(
                    s, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
                );
            }
            if let Some(c) = cond {
                collect_insts_expr(
                    c, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
                );
            }
            if let Some(u) = update {
                collect_insts_expr(
                    u, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
                );
            }
            for u in update_extra {
                collect_insts_expr(
                    u, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
                );
            }
            collect_insts_stmt(
                body, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
            );
            locals.truncate(mark);
        }
        _ => {}
    }
}

/// Record the generic-type/method monomorphs a `base<args>` call form demands
/// (factored out of `collect_insts_expr`'s `Expr::Generic` arm so the MX-T1
/// `Expr::MixinCall { type_args, .. }` form — which carries the same `(base,
/// args)` shape — reproduces the identical recording). `base` is the callee
/// (`Ident` for a bare/qualified-static call, `Member` for an instance/qualified
/// call); `args` are the generic type args.
#[allow(clippy::too_many_arguments)] // recursive collector: threaded visitor state
fn collect_insts_gen_call<'a>(
    base: &Expr,
    args: &[AstType],
    src: &'a str,
    generics: &GenericDecls<'a>,
    gmethods: &GenMethodDecls<'a>,
    t: &mut StructTable,
    seen: &mut Vec<String>,
    monos: &mut MonoList<'a>,
    env: &[(String, IrType)],
    cur_owner: Option<StructId>,
    locals: &mut Vec<(String, IrType)>,
) {
    if let Expr::Ident(s) = base {
        let name = s.text(src);
        record_inst(name, args, src, generics, gmethods, t, seen, monos, env);
        // Bare `M<T>(x)`: owner = the enclosing type if it declares `M`,
        // else `None` (the retained bare-cross-class static bucket — e.g.
        // `list_hof.bf`'s `Map`/`Filter`/`Fold`). Must match the call
        // site's rule exactly (see `bare_gen_owner`).
        let owner = bare_gen_owner(cur_owner, name, gmethods);
        record_method_inst(name, args, src, gmethods, t, env, owner);
    } else if let Expr::Member { name, base: mbase, .. } = base {
        let mname = name.text(src);
        // First try a *qualified static* call `Type.Method<Args>(…)`: the
        // receiver names a registered type. Otherwise treat it as an
        // *instance* call `recv.Method<Args>(…)` and resolve the receiver's
        // concrete-owner `StructId` from the same shapes the call site's
        // `struct_base` resolves (declared local/param, `this`,
        // `this`-field, `new T()`) — R4: identical owner rule both passes.
        let owner = if let Some(id) = qualified_gen_owner(mbase, src, t) {
            Some(id)
        } else {
            let lookup = |n: &str| locals.iter().find(|(ln, _)| ln == n).map(|(_, ty)| *ty);
            instance_recv_owner(mbase, src, &lookup, cur_owner, env, t)
        };
        // A `Member`-base call is *qualified-static* or *instance* — it is
        // never a bare-cross-class `None`-bucket call. So record ONLY when
        // the owner resolves to a concrete type; an unsupported instance
        // receiver (`var` local, call-return, …) records NOTHING — a clean
        // diagnosis (no `None`-bucket static mono, no dangling symbol),
        // matching the call site, which emits no call for that shape.
        if let Some(owner) = owner {
            record_method_inst(mname, args, src, gmethods, t, env, Some(owner));
        }
        collect_insts_expr(
            mbase, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
        );
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
    cur_owner: Option<StructId>,
    locals: &mut Vec<(String, IrType)>,
) {
    match e {
        Expr::Generic { base, args, .. } => {
            collect_insts_gen_call(
                base, args, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
            );
        }
        // A mixin call `Name!(args)` / `Name!<T>(args)` (MX-T1). MX-T1 does NOT
        // expand mixins, so for the collector this must behave EXACTLY as the
        // pre-MX-T1 shapes it replaced: `Name!(args)` was an `Expr::Call`
        // (recurse callee + args); `Name!<T>(args)` was a
        // `Call { callee: Generic{base, args: T}, args }` (the generic-call
        // recording on `(base, T)` plus the arg walk). Reproduce both so the
        // same generic-type/method monomorphs are demanded and no symbol dangles
        // (R7 — keeps `Mixins.bf`/`VarArgs.bf` verify-clean).
        Expr::MixinCall {
            callee,
            type_args,
            args,
            ..
        } => {
            if type_args.is_empty() {
                collect_insts_expr(
                    callee, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
                );
            } else {
                collect_insts_gen_call(
                    callee, type_args, src, generics, gmethods, t, seen, monos, env, cur_owner,
                    locals,
                );
            }
            for a in args {
                collect_insts_expr(
                    a, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
                );
            }
        }
        Expr::Paren { inner, .. } => collect_insts_expr(
            inner, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
        ),
        // `sizeof(List<int>)` instantiates the type it names.
        Expr::SizeOf { ty, .. } => use_in_type(ty, src, generics, gmethods, t, seen, monos, env),
        Expr::Unary { operand, .. }
        | Expr::PostInc { operand, .. }
        | Expr::PostDec { operand, .. }
        | Expr::Prefix { operand, .. } => collect_insts_expr(
            operand, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
        ),
        Expr::Member { base, .. } => collect_insts_expr(
            base, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
        ),
        Expr::Binary { lhs, rhs, .. } => {
            collect_insts_expr(
                lhs, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
            );
            collect_insts_expr(
                rhs, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
            );
        }
        Expr::Assign { target, value, .. } => {
            collect_insts_expr(
                target, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
            );
            collect_insts_expr(
                value, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
            );
        }
        Expr::Ternary {
            cond, then, els, ..
        } => {
            collect_insts_expr(
                cond, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
            );
            collect_insts_expr(
                then, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
            );
            collect_insts_expr(
                els, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
            );
        }
        Expr::Call { callee, args, .. }
        | Expr::Index {
            base: callee, args, ..
        } => {
            collect_insts_expr(
                callee, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
            );
            for a in args {
                collect_insts_expr(
                    a, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
                );
            }
        }
        // TA-7 collection recursion (§6 monomorphization, R4 collector/lowering
        // lockstep): a target-typed dot-form ARGUMENT body can name a generic use
        // referenced nowhere else (e.g. `M(.{ items = new List<float>() })` or a
        // `.( Identity<int>(3) )` — though the latter's `.(args)` is a
        // `Call(DotIdent)` already walked by the arm above). Because TA-7's
        // `lower_generic_call` (and the other call sites) will LOWER those
        // sub-expressions when it back-fills the pending arg, the collector MUST
        // walk them too, or the mono is never emitted (a dangling symbol → a
        // verify/link error). The collector is independent of the two-phase split,
        // so we just add the missing structural arms:
        //   - `.{ … }` (`Initializer`): walk the base AND every entry expression.
        //   - `(a, b)` (`Tuple`): walk every element.
        // (`.Case` is a bare `DotIdent` with no sub-exprs; `.(args)`/`.Case(args)`
        // are `Call(DotIdent)`, already covered above.)
        Expr::Initializer { base, entries, .. } => {
            collect_insts_expr(
                base, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
            );
            for entry in entries {
                collect_insts_expr(
                    entry, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
                );
            }
        }
        Expr::Tuple { elems, .. } => {
            for elem in elems {
                collect_insts_expr(
                    elem, src, generics, gmethods, t, seen, monos, env, cur_owner, locals,
                );
            }
        }
        _ => {}
    }
}

/// Owner of a *bare* generic-method call `M<…>(…)` named `name`, evaluated
/// inside a type whose id is `cur_owner`. Returns `Some(cur_owner)` iff that
/// type declares a generic method `name` (a same-class call); otherwise `None`
/// — the retained bare-cross-class static bucket (e.g. `Map`/`Filter`/`Fold`
/// called bare from `Program.Main`). This rule MUST be identical at the call
/// site (`Lowerer`'s `Expr::Generic` branch) so collection and lowering resolve
/// the same owner and never produce a dangling symbol.
fn bare_gen_owner(
    cur_owner: Option<StructId>,
    name: &str,
    gmethods: &GenMethodDecls,
) -> Option<StructId> {
    match cur_owner {
        Some(id) if gmethods.contains_key(&(Some(id), name.to_string())) => Some(id),
        _ => None,
    }
}

/// Owner of a *qualified* generic-method call `Base.M<…>(…)`: `Some(id)` when
/// `base` is a bare identifier naming a registered type (a static call), else
/// `None`. Instance receivers (`obj.M<…>`) are GM-A3 territory and fall back to
/// the `None` bucket here. Kept in lockstep with the call site.
fn qualified_gen_owner(base: &Expr, src: &str, t: &StructTable) -> Option<StructId> {
    match base {
        Expr::Ident(s) => t.by_name.get(s.text(src)).copied(),
        _ => None,
    }
}

/// Resolve the *static-type* owner `StructId` of an **instance** generic-method
/// call receiver `recv` — for exactly the v1-supported receiver shapes (doc
/// §3.3/§3.4): a declared-typed local/param, `this`, a simple field of a
/// resolvable receiver, `new T()`/`new T<Args>()`, and `(parenthesized)` forms.
///
/// **R4 — owner-skew avoidance:** this single pure resolver is the authoritative
/// owner rule, used by BOTH the collector (`collect_insts_expr`, via a closure
/// over its explicit `locals` scope) AND the call site (the `Expr::Generic`
/// branch, via a closure over the live `Lowerer` scope). Because both passes run
/// the identical match here, they always resolve the same owner; any receiver
/// this returns `None` for is a *diagnosed* shape (the call site emits no
/// dangling call, the collector records nothing), never a divergence.
///
/// `lookup_local` resolves a bare name to its IR type (the scope's view of
/// locals/params); `this_owner` is the enclosing type's id; `env` resolves a
/// generic `new T<Args>()` through the current monomorph bindings.
fn instance_recv_owner(
    recv: &Expr,
    src: &str,
    lookup_local: &dyn Fn(&str) -> Option<IrType>,
    this_owner: Option<StructId>,
    env: &[(String, IrType)],
    t: &StructTable,
) -> Option<StructId> {
    let id_of = |ty: IrType| match ty {
        IrType::Struct(id) | IrType::Ref(id) => Some(id),
        _ => None,
    };
    match recv {
        Expr::Paren { inner, .. } => {
            instance_recv_owner(inner, src, lookup_local, this_owner, env, t)
        }
        // `this` → the enclosing type.
        Expr::This(_) => this_owner,
        // A declared-typed local/param naming a class/struct.
        Expr::Ident(s) => id_of(lookup_local(s.text(src))?),
        // A simple field of a resolvable receiver (`this.f`, `local.f`, …).
        Expr::Member { base, name, .. } => {
            let owner = instance_recv_owner(base, src, lookup_local, this_owner, env, t)?;
            let fname = name.text(src);
            let fidx = t.defs[owner.0 as usize]
                .fields
                .iter()
                .position(|f| f.name == fname)?;
            id_of(t.defs[owner.0 as usize].fields[fidx].ty)
        }
        // `new T()` / `new T<Args>()` — resolve the constructed class.
        Expr::Prefix {
            kw: PrefixKw::New | PrefixKw::Scope,
            operand,
            ..
        } => {
            if let Some((name, args)) = generic_new_parts(operand, src) {
                let argtys: Vec<IrType> =
                    args.iter().map(|a| lower_ty_env(a, src, t, env)).collect();
                let mangled = mangle_generic(name, &argtys);
                if let Some(ty) = t.ty_of(&mangled) {
                    return id_of(ty);
                }
            }
            let name = ctor_class_name(operand, src)?;
            id_of(t.ty_of(name)?)
        }
        _ => None,
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
    // Register non-generic value `struct`s (inline), `class`es (referenced), and
    // `interface`s (pointer-like, no layout — itables.md §4/§5 T1); generics
    // await monomorphization, enums are separate. An interface gets an EMPTY
    // `StructDef` (no `$header`, no fields; it is never instantiated) — IT-T2
    // fills its method slots into `imethods`, not into `defs`/`methods`.
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
            // itables (IT-T1): keep the per-id Vec fields in lockstep.
            t.iface_bases.push(Vec::new());
            t.imethods.push(Vec::new());
            t.idefaults.push(Vec::new());
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
            // itables (IT-T1): keep the per-id Vec fields in lockstep.
            t.iface_bases.push(Vec::new());
            t.imethods.push(Vec::new());
            t.idefaults.push(Vec::new());
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

        // Single inheritance: record the first base that resolves to a *class*.
        // `apply_inheritance` later composes its fields/methods into this type.
        // BASE-ROUTING GUARD (itables.md §5 T1 / R6): an interface is now a
        // registered type, so `lower_ty_env` resolves an interface base to
        // `Ref(iface_id)`. Without the `matches!(kinds[bid], Ref)` guard, a
        // class listing an interface base (`class X : IFace, Base`) would record
        // the INTERFACE as its single inheritance base — corrupting
        // `apply_inheritance`. Only a class-kind base may be the inheritance
        // base here; interface-kind bases are routed into `iface_bases` by
        // IT-T2's `collect_iface_bases`. This guard ships atomically with the
        // type-flip so registration is safe.
        if matches!(kind, StructKind::Ref) {
            for b in &td.bases {
                if let IrType::Ref(bid) = lower_ty_env(b, src, t, env)
                    && bid != id
                    && matches!(t.kinds[bid.0 as usize], StructKind::Ref)
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
                        param_fn_sigs: Vec::new(),
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
                explicit_iface,
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
                // FV-T6b: parallel inner `(ret, ptys)` for any `function R(P)`
                // param, so an inline-lambda arg at this method can be
                // target-typed from the resolved sig (kept only if some param is
                // a function type — otherwise an empty vec, no cost).
                let fn_param_sigs: Vec<Option<(IrType, Vec<IrType>)>> =
                    params.iter().map(|p| param_fn_sig(p, src, t, env)).collect();
                let fn_param_sigs = if fn_param_sigs.iter().any(|s| s.is_some()) {
                    fn_param_sigs
                } else {
                    Vec::new()
                };
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
                    // FV-T3: a `function R(P)` return type lowers to `$Func`
                    // (closure-carrying position), so a call-site that consumes
                    // the result sees the value-struct, not a bare `Ptr`.
                    ret: lower_value_ty(return_ty, src, t, env),
                    params: ps,
                    is_instance,
                    variadic,
                    param_fn_sigs: fn_param_sigs,
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
                // IT-T2: an explicit interface implementation
                // (`Ret IFace.Member(…)`) registers under its bare name in
                // `methods[id]` as usual, but is ALSO recorded in `explicit_impls`
                // keyed by `(class, iface, name)` so `apply_itables` (IT-T3) can
                // disambiguate the itable slot when the class has a same-named
                // regular method. Only an interface-resolving qualifier is kept.
                if let Some(iface_ty) = explicit_iface
                    && let IrType::Ref(iface_id) = lower_ty_env(iface_ty, src, t, env)
                    && is_interface(t, iface_id)
                {
                    t.explicit_impls
                        .insert((id, iface_id, nm.clone()), sig.clone());
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
                            param_fn_sigs: Vec::new(),
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
                            param_fn_sigs: Vec::new(),
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

/// CB-T0 — Merge a non-generic `extension Foo { … }` into the already-registered
/// type `id` (the reopened `class`/`struct Foo`), APPENDING members rather than
/// rebuilding the type. Distinct from [`fill_members_at`]'s full-type fill, which
/// OWNS the type's field list (`t.defs[id].fields = …`, clobbering it) and records
/// the inheritance base. Here:
///
/// * **Fields ADD, never replace.** Each extension instance field (and any
///   auto-property backing field) is *appended* after the base type's existing
///   fields/`$header`; the original field list and its per-field pointer-element
///   types are untouched. A `$header` is **not** re-added — the base class already
///   carries one at offset 0.
/// * **Field defaults survive.** New defaults are pushed onto the *existing*
///   `field_inits[id]` Vec (via `entry(id).or_default().push`), so the base type's
///   recorded defaults are preserved and the extension's are added.
/// * **Ctors/methods/virtuals/properties APPEND** through `fill_members_at` with
///   `fill_layout = false` (the same path the payload-enum reclassification uses
///   to avoid re-laying-out fields). Its existing per-signature dedup guards
///   (`ctors` arity check, `methods` `params`-equality check) mean a duplicate
///   member signature is **not** double-added; a legitimate `override`/`new` (a
///   distinct symbol/slot) still composes normally.
fn fill_extension_at(td: &TypeDecl, id: StructId, kind: StructKind, src: &str, t: &mut StructTable) {
    append_extension_fields(td, id, kind, src, t);
    // Append ctors / methods / virtuals / property accessors. `fill_layout =
    // false` skips `fill_fields_at` (we just appended the layout) and the
    // base-recording (the base type already set `t.bases[id]`).
    fill_members_at(td, id, kind, &[], src, t, false);
}

/// Append `td`'s instance fields (and auto-property backing fields) onto the
/// existing layout at `id`, preserving everything already there. Mirrors the
/// field/static/property handling of [`fill_fields_at`] but **never** writes
/// `$header` and **never** assigns `t.defs[id].fields` wholesale.
fn append_extension_fields(
    td: &TypeDecl,
    id: StructId,
    _kind: StructKind,
    src: &str,
    t: &mut StructTable,
) {
    let env: TyEnv = &[];
    for m in &td.members {
        if let Member::Field {
            ty,
            name,
            modifiers,
            init,
            ..
        } = m
        {
            // Static fields become module globals (not layout); consts hold no
            // storage. Mirror `fill_fields_at` exactly so an extension `static`
            // field registers its global, then skip the instance layout.
            if modifiers
                .iter()
                .any(|(mo, _)| matches!(mo, Modifier::Static | Modifier::Const))
            {
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
            let elem = pointer_elem_env(ty, src, t, env);
            t.field_elems[id.0 as usize].push(elem);
            t.defs[id.0 as usize].fields.push(FieldDef {
                name: name.text(src).to_string(),
                ty: fty,
            });
            // ADD (don't reset) this id's field defaults: the base type's recorded
            // initializers stay, the extension's are appended.
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
    // Auto-property (a body-less accessor) backing field `{Name}$prop`, same rule
    // as `fill_fields_at`: skip statics and all-computed properties.
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
            let pelem = pointer_elem_env(ty, src, t, env);
            t.field_elems[id.0 as usize].push(pelem);
            t.defs[id.0 as usize].fields.push(FieldDef {
                name: format!("{}$prop", name.text(src)),
                ty: pty,
            });
        }
    }
}

fn fill_type_struct(td: &TypeDecl, src: &str, t: &mut StructTable) {
    // Interfaces are registered (empty `StructDef`) in IT-T1 but their members
    // are filled separately by IT-T2's `fill_iface_members` (into `imethods`,
    // NOT `methods`/`defs`), so skip the ordinary member-fill here: an interface
    // has no `$header`, no instance fields, and its default-bodied methods must
    // NOT land in `methods[iface]` (itables.md §5 T2). Excluding `Interface`
    // keeps T1's StructDef genuinely empty.
    // CB-T0: a non-generic `extension Foo { … }` REOPENS the already-declared
    // `class`/`struct Foo` rather than declaring a new type. Resolve the existing
    // id via `by_name` (never allocate a new one) and merge the extension's
    // members into it — APPENDING ctors/methods/virtuals and ADDING (never
    // replacing) fields, so the base type's fields and their defaults survive.
    // A generic extension (`extension Foo<T>`) follows the monomorph path and is
    // not handled here; an extension whose base type isn't registered in this
    // (standalone) compilation — e.g. `extension LibClassA` lowered without LibA —
    // resolves to `None` and is skipped, exactly as before this task.
    if td.kind == TypeKind::Extension {
        if td.generic_params.is_empty()
            && let Some(&id) = t.by_name.get(td.name.text(src))
        {
            let kind = t.kinds[id.0 as usize];
            if !matches!(kind, StructKind::Interface) {
                fill_extension_at(td, id, kind, src, t);
            }
        }
        for m in &td.members {
            if let Member::Nested(n) = m {
                fill_type_struct(n, src, t);
            }
        }
        return;
    }
    let kind = struct_kind(td)
        .filter(|k| !matches!(k, StructKind::Interface))
        .filter(|_| td.generic_params.is_empty());
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

/// FV-T6a: an INLINE-arg lambda queued for emission. Unlike [`LambdaEmit`] the
/// param TYPES (and the return type) are unknown at collection — they come from
/// the callee param resolved at the call site (recorded in
/// `StructTable::inline_lambda_sigs`). So this carries only the `$lambdaN`
/// symbol, the lambda's param NAME spans, its body, and the source: the emit
/// pass zips the param names with the recorded `(ret, ptys)` to build the final
/// `(name, ty)` pairs. `(symbol, param name spans, body, source)`.
type InlineLambdaEmit<'a> = (String, &'a [Span], &'a Stmt, &'a str);

/// Collect anonymous lambdas to emit as free functions. Minimal slice:
/// paramless lambdas assigned to a `function R()` local (`function R() f =
/// () => …;`) — the target type gives the signature (no inference/capture).
/// Each gets a `$lambdaN` symbol recorded by span; its body is queued to emit.
fn collect_lambdas<'a>(
    items: &'a [Item],
    src: &'a str,
    structs: &mut StructTable,
    emits: &mut Vec<LambdaEmit<'a>>,
    inline: &mut Vec<InlineLambdaEmit<'a>>,
) {
    for item in items {
        match item {
            Item::Namespace {
                body: Some(body), ..
            } => collect_lambdas(body, src, structs, emits, inline),
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
                        collect_lambdas_stmt(s, src, structs, emits, inline);
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
    inline: &mut Vec<InlineLambdaEmit<'a>>,
) {
    match stmt {
        Stmt::Block { stmts, .. } => {
            for s in stmts {
                collect_lambdas_stmt(s, src, structs, emits, inline);
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
            // The local-init lambda's BODY may itself contain inline-arg lambdas
            // (`function … f = x => xs.Map(y => y + x)`); walk it too.
            collect_lambdas_in_body(body, src, structs, inline);
        }
        Stmt::If { then, els, .. } => {
            collect_lambdas_stmt(then, src, structs, emits, inline);
            if let Some(e) = els {
                collect_lambdas_stmt(e, src, structs, emits, inline);
            }
        }
        Stmt::While { body, .. }
        | Stmt::DoWhile { body, .. }
        | Stmt::For { body, .. }
        | Stmt::ForEach { body, .. }
        | Stmt::Defer { body, .. } => collect_lambdas_stmt(body, src, structs, emits, inline),
        Stmt::Locals { decls, .. } => {
            for d in decls {
                collect_lambdas_stmt(d, src, structs, emits, inline);
            }
        }
        // A statement-scope mixin declaration (MX-T1). Its body is NOT walked
        // for lambdas/local-fns: mixin expansion is MX-T3, and a lambda inside a
        // mixin body is a GATED shape there (`has_lambda_or_localfn`, mixins.md
        // §6). This matches the pre-MX-T1 world exactly — a local mixin was a
        // `Stmt::LocalFunction`, which this walker also never descended into
        // (it fell to the wildcard). Intentional no-op for behavior preservation.
        Stmt::MixinDecl { .. } => {}
        _ => {}
    }
    // FV-T6a: after the statement's own (local-init) lambda handling and nested
    // recursion, walk every expression the statement carries for INLINE lambdas
    // in call-arg (or any nested) position. The local-init arm above already
    // inserted its lambda's symbol into `lambda_names`, so the walker's
    // already-collected guard skips it (no double collection).
    for_each_stmt_expr(stmt, &mut |e| collect_lambdas_expr(e, src, structs, inline));
}

/// Walk a lambda BODY (`=> e` expression-body or `=> { … }` block-body) for
/// nested INLINE lambdas (a curried `x => ys.Map(y => x + y)` or a block body
/// whose statements call a HOF with an inline lambda). An expression body is
/// handed straight to [`collect_lambdas_expr`]; a block (or any other) body is
/// driven through [`for_each_stmt_expr`], reaching every expression position.
fn collect_lambdas_in_body<'a>(
    body: &'a Stmt,
    src: &'a str,
    structs: &mut StructTable,
    inline: &mut Vec<InlineLambdaEmit<'a>>,
) {
    match body {
        Stmt::Expr { expr, .. } => collect_lambdas_expr(expr, src, structs, inline),
        other => for_each_stmt_expr(other, &mut |e| collect_lambdas_expr(e, src, structs, inline)),
    }
}

/// Invoke `f` on each *direct* top-level expression a statement carries (its
/// init/cond/value/call exprs), and recurse into nested statements so the whole
/// method body is covered. This is the driver behind FV-T6a's inline-lambda
/// collection: it reaches every expression position where a lambda could appear
/// as a (possibly nested) call argument. Statements that only contain other
/// statements (blocks, loops, if) recurse; statements with expressions feed
/// them to `f` (which then walks each expression tree).
fn for_each_stmt_expr<'a>(stmt: &'a Stmt, f: &mut dyn FnMut(&'a Expr)) {
    match stmt {
        Stmt::Block { stmts, .. } => {
            for s in stmts {
                for_each_stmt_expr(s, f);
            }
        }
        Stmt::Locals { decls, .. } => {
            for d in decls {
                for_each_stmt_expr(d, f);
            }
        }
        Stmt::Local { init: Some(e), .. } => f(e),
        Stmt::Expr { expr, .. } => f(expr),
        Stmt::Return { value: Some(e), .. } => f(e),
        Stmt::If {
            cond, then, els, ..
        } => {
            f(cond);
            for_each_stmt_expr(then, f);
            if let Some(e) = els {
                for_each_stmt_expr(e, f);
            }
        }
        Stmt::While { cond, body, .. } | Stmt::DoWhile { cond, body, .. } => {
            f(cond);
            for_each_stmt_expr(body, f);
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
                for_each_stmt_expr(i, f);
            }
            for s in init_extra {
                for_each_stmt_expr(s, f);
            }
            if let Some(c) = cond {
                f(c);
            }
            if let Some(u) = update {
                f(u);
            }
            for u in update_extra {
                f(u);
            }
            for_each_stmt_expr(body, f);
        }
        Stmt::ForEach { iter, body, .. } => {
            f(iter);
            for_each_stmt_expr(body, f);
        }
        Stmt::Defer { body, .. } => for_each_stmt_expr(body, f),
        // A statement-scope mixin (MX-T1): its body's expressions are NOT fed to
        // `f`. Matches the pre-MX-T1 local mixin (`Stmt::LocalFunction`), which
        // this driver also skipped. Mixin-body walking is MX-T3. Intentional.
        Stmt::MixinDecl { .. } => {}
        _ => {}
    }
}

/// FV-T6a: walk an expression tree and assign a `$lambdaN` symbol to every
/// INLINE `Expr::Lambda` found in a (possibly nested) call-argument or any other
/// sub-expression position — recording it for emission with its param types
/// filled in later (T6b, from the resolved callee sig). An `Expr::Lambda` that
/// ALREADY has a symbol (the local-init shape handled by the `Stmt::Local` arm)
/// is skipped via the `lambda_names` guard, so there is no double collection.
/// The walk descends into both `Expr::Call` and `Expr::Generic` argument lists
/// (and through every other expression form), so a lambda arg anywhere — to a
/// bare/qualified/instance generic method (`xs.Map<int32>(x => …)`) or a plain
/// method — is found.
fn collect_lambdas_expr<'a>(
    e: &'a Expr,
    src: &'a str,
    structs: &mut StructTable,
    inline: &mut Vec<InlineLambdaEmit<'a>>,
) {
    match e {
        Expr::Lambda {
            span, params, body, ..
        } => {
            // Skip a lambda already collected by the local-init pre-pass (its
            // span is in `lambda_names`); only an as-yet-unknown inline lambda
            // gets a fresh symbol here.
            if !structs.lambda_names.contains_key(span) {
                let name = format!("$lambda{}", structs.lambda_names.len());
                structs.lambda_names.insert(*span, name.clone());
                inline.push((name, params.as_slice(), &**body, src));
            }
            // A lambda body may nest further inline lambdas (e.g. a curried
            // `x => ys.Map(y => x + y)`); walk it.
            collect_lambdas_in_body(body, src, structs, inline);
        }
        Expr::Call { callee, args, .. } => {
            collect_lambdas_expr(callee, src, structs, inline);
            for a in args {
                collect_lambdas_expr(a, src, structs, inline);
            }
        }
        // A mixin call (MX-T1) mirrors the old `Call`/`Generic` it replaced:
        // walk the callee and every arg so an inline lambda in a mixin-call arg
        // position (`Foo!(x => …)`) is still collected — behavior-preserving.
        // `type_args` are TYPES, never expressions (like `Generic.args`), so
        // they carry no lambda. (A lambda *inside a mixin body* is a separate,
        // GATED concern handled in MX-T3 — mixins.md §6.)
        Expr::MixinCall { callee, args, .. } => {
            collect_lambdas_expr(callee, src, structs, inline);
            for a in args {
                collect_lambdas_expr(a, src, structs, inline);
            }
        }
        // `Generic.args` are TYPES, never expressions — only the base can carry a
        // lambda (it won't, but stay uniform). The lambda lives in the enclosing
        // `Call`'s arg list, walked above.
        Expr::Generic { base, .. } => {
            collect_lambdas_expr(base, src, structs, inline);
        }
        Expr::Paren { inner, .. } => collect_lambdas_expr(inner, src, structs, inline),
        Expr::Member { base, .. } => collect_lambdas_expr(base, src, structs, inline),
        Expr::Unary { operand, .. }
        | Expr::Prefix { operand, .. }
        | Expr::Cast { operand, .. }
        | Expr::PostInc { operand, .. }
        | Expr::PostDec { operand, .. } => collect_lambdas_expr(operand, src, structs, inline),
        Expr::Binary { lhs, rhs, .. } => {
            collect_lambdas_expr(lhs, src, structs, inline);
            collect_lambdas_expr(rhs, src, structs, inline);
        }
        Expr::Assign { target, value, .. } => {
            collect_lambdas_expr(target, src, structs, inline);
            collect_lambdas_expr(value, src, structs, inline);
        }
        Expr::Ternary {
            cond, then, els, ..
        } => {
            collect_lambdas_expr(cond, src, structs, inline);
            collect_lambdas_expr(then, src, structs, inline);
            collect_lambdas_expr(els, src, structs, inline);
        }
        Expr::Index { base, args, .. } => {
            collect_lambdas_expr(base, src, structs, inline);
            for a in args {
                collect_lambdas_expr(a, src, structs, inline);
            }
        }
        Expr::Initializer { base, entries, .. } => {
            collect_lambdas_expr(base, src, structs, inline);
            for ent in entries {
                collect_lambdas_expr(ent, src, structs, inline);
            }
        }
        Expr::Tuple { elems, .. } => {
            for el in elems {
                collect_lambdas_expr(el, src, structs, inline);
            }
        }
        Expr::Named { value, .. } => collect_lambdas_expr(value, src, structs, inline),
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
        // A statement-scope mixin (MX-T1). Its body is NOT collected for local
        // functions: mixin expansion is MX-T3, where a local-fn inside a mixin
        // body is a GATED shape (`has_lambda_or_localfn`, mixins.md §6). The
        // pre-MX-T1 local mixin was a `Stmt::LocalFunction` this walker also
        // never recursed into (it fell to the wildcard). Intentional no-op.
        Stmt::MixinDecl { .. } => {}
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
    // FV-T6a: inline-arg lambdas collected separately — their param TYPES are
    // unknown here (they come from the resolved callee sig during lowering), so
    // each carries only its symbol + param-name spans + body; the emit pass
    // below fills in the types from `structs.inline_lambda_sigs`.
    let mut inline_lambda_emits: Vec<InlineLambdaEmit> = Vec::new();
    for f in &all {
        collect_lambdas(
            &f.unit.items,
            f.src,
            &mut structs,
            &mut lambda_emits,
            &mut inline_lambda_emits,
        );
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
        // FV-T2: route *every* lambda through `emit_closure` so each `$lambdaN`
        // is uniformly `$self`-leading (param 0 = `$self: Ptr`). A non-capturing
        // body gets an empty `caps` list and simply ignores `$self` (the uniform
        // call passes a `null` target). This is the one callee ABI behind the
        // single uniform calling convention (`code(target, args…)`).
        if let Some(func) = emit_closure(name, *ret, params, &caps, &mb, lsrc, &structs) {
            m.add_function(func);
        }
    }
    // FV-T6b: emit each INLINE-arg lambda. Its `(ret, ptys)` were recorded by the
    // call site into `inline_lambda_sigs` when the callee param was resolved; zip
    // those param types with the lambda's param NAME spans to build the `(name,
    // ty)` pairs, then emit through the SAME `emit_closure` path as a declared
    // lambda (so captures and the `$self`-leading ABI are identical). An inline
    // lambda that was never reached as a typed call arg has no recorded sig — it
    // emits paramless (a degenerate but well-typed body; it is never called).
    let inline_sigs = structs.inline_lambda_sigs.borrow().clone();
    for (name, pspans, body, lsrc) in &inline_lambda_emits {
        let mb = match *body {
            Stmt::Expr { expr, .. } => MethodBody::Expr(expr.clone()),
            other => MethodBody::Block(other.clone()),
        };
        let (ret, ptys) = inline_sigs.get(name).cloned().unwrap_or((IrType::Void, vec![]));
        // Pair each lambda param name with the resolved callee param type. If the
        // arity disagrees (a malformed call), zip stops at the shorter — the call
        // site won't have produced a usable value either way.
        let param_pairs: Vec<(String, IrType)> = pspans
            .iter()
            .zip(ptys.iter())
            .map(|(nspan, t)| (nspan.text(lsrc).to_string(), *t))
            .collect();
        let caps = structs
            .lambda_captures
            .borrow()
            .get(name)
            .cloned()
            .unwrap_or_default();
        if let Some(func) = emit_closure(name, ret, &param_pairs, &caps, &mb, lsrc, &structs) {
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
            None,
            None,
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
    // Emit each generic-*method* monomorph: re-find its decl via `(owner, name)`
    // and lower its body with the instantiation's type-param env, so a `T`
    // resolves concretely. A `None`-owner / static monomorph is a receiver-less
    // free function; a `Some(owner)` *instance* monomorph (GM-A3b) is emitted as
    // a real instance method — `this_ty = Ref(owner)` (so `lower_method` spills a
    // leading `this` and `this.field` / bare sibling calls in the body resolve)
    // and the owner's method table is its `sigs`.
    let mut gmethods: GenMethodDecls = HashMap::new();
    for f in &all {
        index_generic_methods(&f.unit.items, f.src, &structs, &mut gmethods);
    }
    // GM-B1: index each type monomorph's generic methods under
    // `(Some(mono_id), name)` (from the template decl) so a generic-method
    // monomorph on a generic owner (`List<int32>.Map<R>`) re-finds its decl
    // below. Mirrors the same augmentation in `StructTable::build` (step 4c-bis).
    index_gmethods_on_monos(&structs.monos, &generics, &mut gmethods);
    let empty: HashMap<String, Vec<MethodSig>> = HashMap::new();
    for mono in &structs.gen_method_monos {
        // The number of the method's OWN type-parameters: the monomorph's `env`
        // is `owner-mono bindings ++ method bindings` (GM-B1), so for an owner
        // that is itself a type monomorph the leading entries are the owner's
        // `T…` and must be excluded when matching the decl's `generic_params`.
        let owner_tparams = match mono.owner {
            Some(oid) => structs
                .monos
                .iter()
                .find(|(id, _, _)| *id == oid)
                .map(|(_, _, e)| e.len())
                .unwrap_or(0),
            None => 0,
        };
        let method_tparams = mono.env.len() - owner_tparams;
        // Re-find the decl via `(owner, name)`, picking the overload whose
        // type-param arity matches the method's OWN type-params (mirrors the
        // collector's overload selection, combined-env aware).
        if let Some(&(member, mdecl_src)) = gmethods.get(&(mono.owner, mono.name.clone())).and_then(
            |v| {
                v.iter().find(|(m, _)| {
                    matches!(m, Member::Method { generic_params, .. }
                        if generic_params.len() == method_tparams)
                })
            },
        ) && let Member::Method {
            return_ty,
            params,
            body,
            modifiers,
            ..
        } = member
        {
            // FV-T3: keep the emitted return type in lockstep with the recorded
            // generic-method sig (`record_method_inst` uses `lower_value_ty`), so
            // a `function`-returning generic method's ret is `$Func` on both sides.
            let ret = lower_value_ty(return_ty, mdecl_src, &structs, &mono.env);
            // Instance monomorph iff non-static AND owned by a concrete type —
            // identical to `record_method_inst`'s `is_instance` rule so the ABI
            // (leading `this`) agrees between sig, call site, and definition.
            let is_static = modifiers
                .iter()
                .any(|(mo, _)| matches!(mo, Modifier::Static));
            let this_ty = match mono.owner {
                Some(oid) if !is_static => Some(IrType::Ref(oid)),
                _ => None,
            };
            // For an instance monomorph, resolve `this.field` / bare sibling calls
            // through the owner's method table; otherwise no sibling scope.
            let sigs: &HashMap<String, Vec<MethodSig>> = match (this_ty, mono.owner) {
                (Some(_), Some(oid)) => &structs.methods[oid.0 as usize],
                _ => &empty,
            };
            if let Some(func) = lower_method(
                mono.sym.clone(),
                ret,
                params,
                body,
                mdecl_src,
                sigs,
                &structs,
                this_ty,
                &mono.env,
                &[],
                mono.owner,
                ret_fn_sig_of_ty(return_ty, mdecl_src, &structs, &mono.env),
            ) {
                m.add_function(func);
            }
        }
    }
    // FV-T4: emit each collected static method-ref thunk once. This runs LAST so
    // every reference site (regular method bodies, lambda/local-fn bodies, and
    // generic monomorph bodies above) has already populated the de-dup set.
    let thunks: Vec<MethodRefThunk> = structs
        .method_ref_thunks
        .borrow()
        .values()
        .cloned()
        .collect();
    for thunk in &thunks {
        m.add_function(emit_method_ref_thunk(thunk));
    }
    m
}

/// Emit a method-ref thunk `<sym>($self, P…) -> ret` absorbing the uniform
/// convention's hidden `$self` (param 0) so a method (which has either no `$self`
/// or a `this`-typed leading param) is callable through the same `code(target,
/// args…)` shape as a lambda/closure.
///   - **static** (FV-T4): ignore `$self` and tail-call `<full>(P…)`.
///   - **bound** (FV-T5): forward `$self` as the receiver `this` and tail-call
///     `<full>($self, P…)`. The receiver is a class body `Ptr` (target); in the
///     opaque-pointer IR it is ABI-identical to the method's `Ref(owner)` `this`
///     param, so the `((T)$self)` cast is implicit (no IR cast needed).
fn emit_method_ref_thunk(thunk: &MethodRefThunk) -> Function {
    let mut ir_params: Vec<IrParam> = vec![IrParam {
        name: Some("$self".to_string()),
        ty: IrType::Ptr,
    }];
    ir_params.extend(thunk.params.iter().enumerate().map(|(i, t)| IrParam {
        name: Some(format!("p{i}")),
        ty: *t,
    }));
    let mut fb = FunctionBuilder::new(thunk.thunk_sym.clone(), ir_params, thunk.ret);
    // For a bound ref, `$self` (Param 0) is the receiver and leads the call
    // (the method's `this`); for a static ref it is dropped. The explicit params
    // always start at `Param(1)` (after `$self`).
    let mut args: Vec<Value> = Vec::with_capacity(thunk.params.len() + 1);
    if thunk.bound {
        args.push(Value::Param(0));
    }
    args.extend((0..thunk.params.len()).map(|i| Value::Param((i + 1) as u32)));
    let r = fb.call(thunk.callee.clone(), args, thunk.ret);
    if thunk.ret == IrType::Void {
        fb.ret(None);
    } else {
        fb.ret(Some(r));
    }
    fb.finish()
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
    /// FV-T6b: per-EXPLICIT-parameter inner function signature `(ret, ptys)` for
    /// a `function R(P)` param, `None` otherwise. Indexed by *explicit* parameter
    /// position (parallel to the explicit args at a call site — the leading
    /// `this` of an instance method is NOT included). Lets the call site
    /// target-type an inline-lambda argument's params from the resolved callee
    /// signature (`xs.Map<int32>(x => x*10)`), since `params[i]` only records the
    /// erased `$Func` value-struct. Empty when no explicit param is a function
    /// type (the common case), so non-HOF signatures pay nothing.
    param_fn_sigs: Vec<Option<(IrType, Vec<IrType>)>>,
}

/// Whether `e` is a *target-typed pending* expression: a leading-dot dot-form
/// whose `IrType` is not known without a target type to construct it against.
/// Pure, no `self`, no emission, O(1) — a syntactic shape check only. This is
/// the single classification point for the targeted-call-args feature (§3.1).
///
/// Returns `true` for the three pending shapes:
///   - `Expr::DotIdent` — a bare `.Case`.
///   - `Expr::Call { callee: Expr::DotIdent, .. }` — covers *both* `.(args)`
///     (the ctor shorthand, callee name `"."`) *and* `.Case(args)` (an enum
///     case); we match the `DotIdent` callee regardless of its name, so an
///     *ambiguous* `.Case(payload)` is classified pending and can be resolved
///     against the now-known param type (§3.1 classification note).
///   - `Expr::Initializer { base: Expr::DotIdent, .. }` — the `.{ … }` form.
///
/// Returns `false` for everything else, including: ordinary expressions; a bare
/// `Expr::Tuple` (concrete for the first slice — it evaluates via `build_tuple`,
/// §3.6); `ref`/`out` (a `Prefix`, which never wraps a pending form since a
/// dot-form is never an lvalue); and a *qualified* `Enum.Case(…)` / `Type.{ … }`
/// (those have a concrete base and resolve without a target).
///
/// `src` is unused in the first slice but kept in the signature: when
/// `Expr::Named` (named call args) lands, this is the one place to look through
/// `Named` to its value (§5.1 / §10).
// TA-1 lands this classifier as the feature foundation; TA-3 wires its first
// production caller — the `has_pending` fork at the head of `lower_method_call`.
fn arg_is_pending(e: &Expr, _src: &str) -> bool {
    match e {
        // bare `.Case`
        Expr::DotIdent { .. } => true,
        // `.{ … }` (a `DotIdent`-based initializer; `new T() { … }` / `Type { … }`
        // have a concrete base and stay concrete).
        Expr::Initializer { base, .. } => matches!(&**base, Expr::DotIdent { .. }),
        // BOTH `.(args)` (callee name `"."`) and `.Case(args)` (callee a case
        // name): match the `DotIdent` callee regardless of its name. A qualified
        // `Enum.Case(args)` has a `Member`/`Ident` callee, not a `DotIdent`, so
        // it stays concrete.
        Expr::Call { callee, .. } => matches!(&**callee, Expr::DotIdent { .. }),
        _ => false,
    }
}

/// The *syntactic kind* of a pending (target-typed dot-form) argument, carrying
/// only what the shape gate (§3.2) needs to decide compatibility with a formal
/// param type — never an `IrType` (pending-ness is syntactic; the type is the
/// formal's, decided at resolution). Sema-local; never leaks into IR.
///
///  - `Ctor` — `.(args)`: compatible only with a value-struct `Struct(id)`.
///  - `Initializer` — `.{ … }`: compatible only with `Struct(id)` for the first
///    slice (a `Ref(id)` class-init is the §10 follow-up).
///  - `EnumCase(case)` — bare `.Case` / `.Case(payload)`: compatible only with a
///    payload-enum `Struct(id)` whose case set contains `case`. The borrowed name
///    is read from `src` at classification time and lives as long as the AST.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingKind<'a> {
    Ctor,
    Initializer,
    EnumCase(&'a str),
}

/// A classified argument slot for shape-gated resolution (§3.1): a `Concrete`
/// slot already lowered to a known `IrType` (scored by [`type_affinity`]), or a
/// `Pending` dot-form whose type is the formal's, gated by [`PendingKind`].
/// Sema-local and stack-lived during one call's resolution; never stored in
/// `StructTable` and never an IR type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArgShape<'a> {
    Concrete(IrType),
    Pending(PendingKind<'a>),
}

/// Classify a *pending* argument expression into its [`PendingKind`] (the
/// companion to [`arg_is_pending`] for the shape gate). Returns `None` for a
/// concrete expression — exactly the cases [`arg_is_pending`] returns `false`
/// for, so the two stay in lockstep. Pure, syntactic, O(1): the `.(args)` ctor
/// shorthand is the `Call(DotIdent ".")` form; any other `Call(DotIdent name)`
/// or a bare `DotIdent` is a case form (named `name`); a `DotIdent`-based
/// `Initializer` is `.{ … }`.
// Wired into the call sites' `has_pending` fork in TA-3 (via `lower_args_phase1`).
fn pending_kind<'a>(e: &'a Expr, src: &'a str) -> Option<PendingKind<'a>> {
    match e {
        // bare `.Case`
        Expr::DotIdent { name, .. } => Some(PendingKind::EnumCase(name.text(src))),
        // `.{ … }` (DotIdent base only)
        Expr::Initializer { base, .. } if matches!(&**base, Expr::DotIdent { .. }) => {
            Some(PendingKind::Initializer)
        }
        // `.(args)` (callee `"."`) vs `.Case(args)` (callee a case name)
        Expr::Call { callee, .. } => match &**callee {
            Expr::DotIdent { name, .. } => {
                let n = name.text(src);
                if n == "." {
                    Some(PendingKind::Ctor)
                } else {
                    Some(PendingKind::EnumCase(n))
                }
            }
            _ => None,
        },
        _ => None,
    }
}

/// The shape gate (§3.2): can a pending arg of `kind` target-type to formal
/// param `f`? `Compatible` contributes a small +1 bonus (below an exact concrete
/// match of +2); `Incompatible` DISQUALIFIES the whole candidate. This is
/// correctness blocker #2: an incompatible pending slot must remove the
/// candidate, not score 0 and silently pick a wrong overload that back-fills
/// against the wrong param type.
///
/// `structs` is needed only for the enum-case set membership check; the ctor /
/// initializer kinds gate purely on the formal being a value `Struct(_)`.
fn pending_shape_compatible(kind: PendingKind, f: IrType, structs: &StructTable) -> bool {
    match kind {
        // `.(args)`: a value-struct ctor shorthand — only a value `Struct(id)`.
        // A `Ref` (class), int/float/ptr/void all disqualify.
        PendingKind::Ctor => matches!(f, IrType::Struct(_)),
        // `.{ … }`: first slice only constructs against a value `Struct(id)`
        // (the `Ref(id)` class-init is the §10 follow-up; keep `Struct` only).
        PendingKind::Initializer => matches!(f, IrType::Struct(_)),
        // `.Case` / `.Case(payload)`: only a payload-enum `Struct(id)` whose case
        // set actually contains `case`. A non-enum struct, a class, or a primitive
        // disqualifies; so does an enum that lacks this case.
        PendingKind::EnumCase(case) => match f {
            IrType::Struct(id) => structs
                .enum_cases
                .get(&id)
                .is_some_and(|cases| cases.iter().any(|(n, _, _)| n == case)),
            _ => false,
        },
    }
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
    // The all-concrete case of the shape-gated resolver. With every slot
    // `Concrete`, `pick_overload_partial` scores by `type_affinity`, applies the
    // identical variadic penalty + tie-break, and never disqualifies (the shape
    // gate only fires on `Pending` slots), so it is byte-for-byte the old
    // behavior. `structs` is unused on the all-concrete path (the enum-case gate
    // only reads it for `Pending(EnumCase)` slots), so a default table is sound.
    let shapes: Vec<ArgShape> = arg_tys.iter().map(|t| ArgShape::Concrete(*t)).collect();
    pick_overload_partial(cands, &shapes, members, &StructTable::default())
}

/// Shape-gated generalization of [`pick_overload`] over a *sparse* shape vector
/// (§3.1/§3.2): each slot is either `Concrete(IrType)` (scored by
/// [`type_affinity`], exactly as `pick_overload`) or `Pending(kind)` (a
/// target-typed dot-form whose type isn't known yet). A pending slot is run
/// through the SHAPE GATE against its formal param type
/// ([`pending_shape_compatible`]): **compatible → +1 bonus** (below an exact
/// concrete match of +2, so it breaks ties toward the candidate it can target
/// without outranking a better concrete match elsewhere); **incompatible →
/// DISQUALIFY the candidate entirely** (correctness blocker #2 — never score 0
/// and silently pick a wrong overload).
///
/// Arity rules are unchanged from `pick_overload`: a `params T[]` matches any
/// count at/above its fixed params (flat penalty 1); a normal method needs an
/// exact count. Only the fixed leading params are scored/gated (the variadic
/// tail is back-filled against `elem` in `finish_args`). Ties keep the first
/// registered. `pick_overload` delegates here with all-`Concrete` shapes.
fn pick_overload_partial<'s>(
    cands: &'s [MethodSig],
    arg_shapes: &[ArgShape],
    members: bool,
    structs: &StructTable,
) -> Option<&'s MethodSig> {
    let mut best: Option<(&MethodSig, u32)> = None;
    'cand: for c in cands {
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
            Some(_) if arg_shapes.len() + 1 >= formal.len() => (formal.len() - 1, 1),
            Some(_) => continue,
            None if formal.len() == arg_shapes.len() => (formal.len(), 0),
            None => continue,
        };
        // Score the fixed leading params zipped with the slots (truncating to the
        // shorter, exactly as the old `formal[..fixed].zip(arg_tys)`): a concrete
        // slot scores by `type_affinity`; a pending slot applies the shape gate —
        // +1 if it can target-type to `f`, otherwise the candidate is disqualified.
        let mut raw: u32 = 0;
        for (f, s) in formal[..fixed].iter().zip(arg_shapes) {
            raw += match s {
                ArgShape::Concrete(a) => type_affinity(*f, *a),
                ArgShape::Pending(kind) => {
                    if pending_shape_compatible(*kind, *f, structs) {
                        1
                    } else {
                        continue 'cand; // incompatible pending slot → disqualify
                    }
                }
            };
        }
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
    // An interface is a registered type (IT-T1). IT-T6 now resolves its id as
    // the owner so a DEFAULT-bodied interface method (`int32 D() { … }`) lowers
    // through `lower_method` with `this : Ref(iface_id)`, emitted under the
    // symbol `{IFace.prefix}{Method}` (e.g. `I.D`) — exactly the symbol IT-T3
    // wrote into the itable slot via `idefaults`, so the slot resolves to a real
    // function. An ABSTRACT (body-less) interface method has `MethodBody::None`,
    // so `lower_method` returns `None` and emits nothing — only defaults emit.
    // (Interfaces have no ctors/dtors/fields, and `methods[iface]` is empty —
    // defaults are deliberately kept out of it — so `lower_type_at`'s symbol
    // lookup falls back to the `{prefix}{name}` form, reconciling with the slot
    // symbol. A bare sibling call inside a default body routes to interface
    // dispatch on `this`, handled in the bare-call path.)
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
                // FV-T3: a `function R(P)` *return type* is a closure-carrying
                // position, so it lowers to `$Func` (`lower_value_ty`) — letting
                // `Return` coerce a produced function value to a `Func$` ret (a
                // no-op) rather than `undef` it. Delegate-gated.
                let ret = lower_value_ty(return_ty, src, structs, env);
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
                    owner_id,
                    ret_fn_sig_of_ty(return_ty, src, structs, env),
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
                        owner_id,
                        None,
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
                        owner_id,
                        None,
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
                            owner_id,
                            None,
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
                            owner_id,
                            None,
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
#[allow(clippy::too_many_arguments)] // lowering entry: threaded method context
#[allow(clippy::too_many_arguments)] // the per-method lowering entry point
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
    cur_type: Option<StructId>,
    // FV-T7: the inner `(ret, ptys)` of a `function R(P)` return type (`None`
    // otherwise), so a lambda in a `return` position can be target-typed. Set on
    // the `Lowerer` below; see `Lowerer.ret_fn_sig`.
    ret_fn_sig: Option<(IrType, Vec<IrType>)>,
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
    let mut lw = Lowerer::new(fb, ret, methods, structs, env, cur_type);
    lw.ret_fn_sig = ret_fn_sig;

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
            // FV-T3: the spill slot's type is the *param* IR type — a by-value
            // `function R(P)` param is the `$Func` value-struct, so the slot and
            // the `Value::Param` it stores agree (storing a `Func$` into a `Ptr`
            // slot would be an ABI mismatch).
            let ty = param_ir_ty(p, src, structs, env);
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
            // lowers to the uniform indirect call (§5.3). This is what lets a
            // higher-order method like `Map(self, f)` actually call its `f`. The
            // slot now holds a `$Func` value (FV-T3), so the call site loads
            // `code`/`target` from it; a `delegate`-typed param stays bare `Ptr`
            // (gated below) and is not callable through this path.
            if let AstType::Function {
                return_ty,
                params: fps,
                is_delegate: false,
                ..
            } = &p.ty
            {
                let fret = lower_value_ty(return_ty, src, structs, env);
                let fptys: Vec<IrType> = fps
                    .iter()
                    .map(|t| lower_value_ty(t, src, structs, env))
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

/// Emit a lambda as a `$self`-leading function `$lambdaN($self, params…) -> ret`.
/// `$self` (param 0) is the function value's `target` — for a *capturing* lambda
/// it's the env pointer `[cap0 | cap1 …]` (FV-T3: the env holds ONLY captures, no
/// leading code-pointer slot), so each capture binds to `$self[i]`; the lambda's
/// own params follow (`Param(i+1)`). A *non-capturing* lambda passes an empty
/// `caps` list, so the capture loop is a no-op and `$self` (a `null` target) is
/// simply ignored — the one uniform callee ABI for all function values.
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
    let mut lw = Lowerer::new(fb, ret, &empty, structs, &[], None);

    // Captures: bind each name to its env address `$self[i]`. The env (the
    // `target`) now holds ONLY captures — slot 0 is the first capture, not a
    // code pointer (the code pointer lives in the `Func$.code` field at the
    // producer, FV-T3). A non-capturing lambda has an empty `caps`, so this loop
    // is a no-op and `$self` (a `null` target) is never dereferenced.
    let self_env = Value::Param(0);
    for (i, (n, t)) in caps.iter().enumerate() {
        let addr = lw.fb.elem_addr(
            self_env.clone(),
            IrType::Ptr,
            Value::int(i as i128, IrType::I64),
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
    /// The enclosing type being lowered (the owner of the current method/ctor/
    /// dtor), for *both* static and instance contexts — distinct from
    /// `this_slot`, which governs whether to prepend a `this`. Threaded from
    /// `lower_type_at`'s `owner_id`; read by the `Expr::Generic` call branch to
    /// resolve the owner of a bare same-class generic-method call (GM-A2).
    cur_type: Option<StructId>,
    /// Generic type-parameter env when lowering a monomorph's body (so a `T`
    /// local declaration resolves to its concrete type). Empty otherwise.
    env: TyEnv<'a>,
    /// Function-value locals/params: name → (return type, parameter types). A
    /// `function R(P)` local/param holds a `$Func` value-struct (`code` +
    /// `target`); a call `f(args)` through it loads both and emits the uniform
    /// indirect call `code(target, args…)` with this signature (FV-T3, §5.3).
    fn_sigs: HashMap<String, (IrType, Vec<IrType>)>,
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
    /// FV-T7: the *inner* `(ret, ptys)` of the method's `function R(P)` return
    /// type, `None` for a non-function return. `ret_ty` only carries the erased
    /// `$Func` value-struct; this preserves the signature so a lambda written
    /// directly in a `return` position (`return x => x + n;`) target-types its
    /// untyped params from the declared return sig (recorded into
    /// `inline_lambda_sigs` so the emit pass binds the lambda body's params).
    ret_fn_sig: Option<(IrType, Vec<IrType>)>,
}

impl<'a> Lowerer<'a> {
    fn new(
        fb: FunctionBuilder,
        ret_ty: IrType,
        methods: &'a HashMap<String, Vec<MethodSig>>,
        structs: &'a StructTable,
        env: TyEnv<'a>,
        cur_type: Option<StructId>,
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
            cur_type,
            env,
            fn_sigs: HashMap::new(),
            array_locals: std::collections::HashSet::new(),
            defers: vec![Vec::new()],
            local_fns: HashMap::new(),
            scope_allocs: vec![(BlockId(0), Vec::new())],
            enum_locals: HashMap::new(),
            ret_fn_sig: None,
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
                // FV-T3: a `function R(P)` *local* is a closure-carrying position,
                // so its slot is the `$Func` value-struct (via `lower_value_ty`).
                // The lambda/method-ref initializer's value (`Func$` or bare
                // `Ptr`) is coerced into it below (`coerce` auto-wraps a `Ptr`).
                let declared = ty
                    .as_ref()
                    .map(|t| lower_value_ty(t, src, self.structs, self.env));
                let (init_val, init_ty) = match init {
                    Some(e) => {
                        let (v, t) = declared
                            .and_then(|target| self.lower_arg_targeted(target, e, src))
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
                // A `function R(P)` local holds a `$Func` value (slot type set
                // above via `lower_value_ty`); record its signature so a later
                // `name(args)` lowers to the uniform indirect call (load
                // `code`/`target`, §5.3). A `delegate`-typed local stays bare
                // `Ptr` and is not callable through this path (gated).
                if let Some(AstType::Function {
                    return_ty,
                    params,
                    is_delegate: false,
                    ..
                }) = ty
                {
                    let ret = lower_value_ty(return_ty, src, self.structs, self.env);
                    let ptys: Vec<IrType> = params
                        .iter()
                        .map(|p| lower_value_ty(p, src, self.structs, self.env))
                        .collect();
                    self.fn_sigs.insert(name.text(src).to_string(), (ret, ptys));
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
                        // FV-T7: a lambda written directly in a `return` position
                        // (`return x => x + n;`) is collected as an INLINE lambda
                        // (T6a) but has no call-arg target to type its params from.
                        // Seed its `(ret, ptys)` from the method's declared return
                        // function signature (`ret_fn_sig`) so the emit pass binds
                        // the lambda body's params at the right IR types — the
                        // return-position analogue of T6b's call-arg target-typing.
                        self.record_return_lambda_sig(e);
                        // Target-type a `.Some(x)` / `.(args)` / `(a,b)` / `.{ }`
                        // return against the function's return type before falling
                        // back to a plain eval — via the one canonical try-order.
                        let ret_ty = self.ret_ty;
                        let (v, t) = self
                            .lower_arg_targeted(ret_ty, e, src)
                            .unwrap_or_else(|| self.expr(e, src));
                        self.coerce(v, t, ret_ty)
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
            // A statement-scope mixin declaration (MX-T1). It emits NO IR — a
            // mixin is spliced at its call sites, not lowered as a function.
            // Mixin EXPANSION (splicing) is MX-T3; until then `Stmt::MixinDecl`
            // is an intentional no-op. This matches the pre-MX-T1 verifiable
            // behavior closely enough that `Mixins.bf` stays verify-clean: the
            // declaration produces nothing, and each `Name!(args)` call site
            // lowers via the `Expr::MixinCall` arm to the old verifiable path.
            Stmt::MixinDecl { .. } => {}
            // local-function — not in the kernel yet. Skipped (no IR
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
            // A statement-scope mixin (MX-T1). Capture analysis (for a lambda
            // closing over caller locals) does NOT descend into a mixin body:
            // a lambda inside a mixin body is GATED in MX-T3 (mixins.md §6), and
            // the pre-MX-T1 local mixin (`Stmt::LocalFunction`) was likewise not
            // descended into here. Intentional no-op for behavior preservation.
            Stmt::MixinDecl { .. } => {}
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
            }
            // A mixin call (MX-T1) mirrors the old `Call` it replaced for
            // capture analysis: walk callee + args so a lambda closing over a
            // caller local used in a `Foo!(localVar)` arg still records the
            // capture. `type_args` are types, not captured values. (MX-T3 may
            // refine this when mixin bodies become first-class.)
            | Expr::MixinCall {
                callee, args, ..
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
                    // Non-capturing: a bare code `Ptr`. It coerces to
                    // `Func${code, target=null}` only when it crosses a
                    // `Func$`-typed slot/param/return boundary (`coerce`, §5.4).
                    return (code, IrType::Ptr);
                }
                // Capturing: build a `Func$` value. The env (`target`) holds ONLY
                // the captures (no leading code-pointer slot, FV-T3); each capture
                // is stored at index `i`. `emit_closure` reads them back at
                // `$self[i]`.
                let words = caps.len() as i128;
                let env = self.fb.call(
                    "malloc",
                    vec![Value::int(words * 8, IrType::I64)],
                    IrType::Ptr,
                );
                for (i, (_n, slot, ty)) in caps.iter().enumerate() {
                    let dst = self.fb.elem_addr(
                        env.clone(),
                        IrType::Ptr,
                        Value::int(i as i128, IrType::I64),
                    );
                    let val = self.fb.load(slot.clone(), *ty);
                    self.fb.store(dst, val);
                }
                // `Func$ {code = global_addr($lambdaN), target = env}`, built in a
                // fresh alloca co-located with the captures (SSA-dominance safe,
                // §5.6) and returned as the loaded value-struct.
                let fv = self.build_func_value(code, env);
                (fv, IrType::Struct(self.structs.func_struct))
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
                    && let Some(mname) = generic_callee_name(base, src)
                {
                    if let Some(r) = self.lower_generic_call(base, mname, targs, args, src) {
                        return r;
                    }
                    // No generic-method monomorph resolved (an unsupported instance
                    // receiver or an absent key): fall through so a same-named
                    // *non-generic* member call still has a chance, and otherwise
                    // the ordinary unresolved-call path yields a clean default —
                    // never a dangling call to a symbol that was never emitted.
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
                // TA-4 fork (§3.7, mirrors the TA-3 `lower_method_call` fork): a
                // bare-name / free-fn / local-fn / fn-value call whose callee is an
                // `Ident` and that has a target-typed dot-form arg (`.(…)` / `.{ }`
                // / `.Case[(…)]`) diverts to the pending-aware lowerer. The hot path
                // (no pending args, or a non-`Ident` callee) runs the eager
                // `arg_vals` loop below verbatim — byte-identical to pre-TA-4.
                if let Expr::Ident(s) = &**callee
                    && args.iter().any(|a| arg_is_pending(a, src))
                {
                    return self.lower_ident_call_pending(s.text(src), args, src);
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
                    // A function-value local/param (`function R(P) f`): `f(args)`
                    // is the ONE uniform call shape (§5.3). `f` holds a `$Func`
                    // value-struct; load its `code` and `target`, then
                    // `call_indirect(code, [target, args…])`. No branch on
                    // closure-ness — `target` (env or `null`) is always param 0;
                    // the callee's `$self` ignores a `null` target.
                    if let Some((ret, ptys)) = self.fn_sigs.get(name).cloned()
                        && let Some((slot, _)) = self.lookup(name)
                    {
                        let fid = self.structs.func_struct;
                        let code = {
                            let a = self.fb.field_addr(slot.clone(), fid, 0);
                            self.fb.load(a, IrType::Ptr)
                        };
                        let target = {
                            let a = self.fb.field_addr(slot, fid, 1);
                            self.fb.load(a, IrType::Ptr)
                        };
                        let mut call_args: Vec<Value> = vec![target];
                        for (i, (v, t)) in arg_vals.into_iter().enumerate() {
                            let pt = ptys.get(i).copied().unwrap_or(t);
                            call_args.push(self.coerce(v, t, pt));
                        }
                        // LLVM builds the indirect-call type from the *args*, not
                        // the callee signature (§1), so an arity drift is
                        // verify-clean — assert it here instead.
                        debug_assert_eq!(
                            call_args.len(),
                            ptys.len() + 1,
                            "function-value call arity drift for `{name}`: {} args vs {} params + $self",
                            call_args.len(),
                            ptys.len()
                        );
                        return (self.fb.call_indirect(code, call_args, ret), ret);
                    }
                    // Sibling interface dispatch inside a DEFAULT interface-method
                    // body (IT-T6, itables.md §5 T6 / §7). When the enclosing
                    // `this` is an interface value (`Ref(iface_id)`, since IT-T6
                    // lowers a default body with `this : Ref(iface_id)`), a bare
                    // call `A(args)` to another interface method (`A` in
                    // `imethods[iface]`) must dispatch through `this`'s interface
                    // vtable — NOT a direct call, since an abstract sibling has no
                    // direct symbol. Route it to the same interface-dispatch path
                    // as `this.A(args)` would take. (`this` loads as the body
                    // pointer; `emit_iface_dispatch` returns `None` if `name`
                    // isn't an interface slot, so a non-interface bare call inside
                    // such a body falls through unchanged.)
                    if let Some((slot, ty @ IrType::Ref(id))) = self.this_slot.clone()
                        && matches!(self.structs.kinds[id.0 as usize], StructKind::Interface)
                    {
                        let this_v = self.fb.load(slot, ty);
                        if let Some(r) = self.emit_iface_dispatch(this_v, id, name, arg_vals.clone())
                        {
                            return r;
                        }
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
            // A mixin call `Name!(args)` / `scope::Name!(args)` /
            // `Name!<T>(args)` (MX-T1). Mixin EXPANSION is MX-T3; here we only
            // introduce the shape and IGNORE the mixin-ness, lowering to EXACTLY
            // the verifiable path the pre-MX-T1 parse took. Before MX-T1 a
            // `Name!(args)` was an `Expr::Call { callee, args }` and a
            // `Name!<T>(args)` was a `Call { callee: Generic { base, args: T },
            // args }` (the `!`/`::` were dropped). We reconstruct that exact old
            // shape and delegate to the `Expr::Call` lowering, so the emitted IR
            // is byte-identical to before — keeping `Mixins.bf`/`VarArgs.bf`
            // verify-clean (R7). The `scope_qualifier` (`::`) was discarded by
            // the old parser, so it is discarded here too. (MX-T3 replaces this
            // arm with real `expand_mixin` before the unresolved-default.)
            Expr::MixinCall {
                span,
                callee,
                type_args,
                args,
                ..
            } => {
                let synthetic_callee: Expr = if type_args.is_empty() {
                    (**callee).clone()
                } else {
                    Expr::Generic {
                        span: *span,
                        base: callee.clone(),
                        args: type_args.clone(),
                    }
                };
                let synthetic = Expr::Call {
                    span: *span,
                    callee: Box::new(synthetic_callee),
                    args: args.clone(),
                };
                self.expr(&synthetic, src)
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
                            } else if let Some(r) =
                                self.try_bound_method_ref(base, name.text(src), src)
                            {
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
        // FV-T3 (§5.4): `f == null` / `f != null` on a `$Func` value lowers to a
        // single compare of its `code` field (`f.code == null`), not a struct
        // compare. Either operand may be the `Func$` (the other being `null`,
        // i.e. a `Ptr` `Const::Null`).
        if matches!(op, AstBin::Eq | AstBin::Ne) {
            let fid = self.structs.func_struct;
            let func = IrType::Struct(fid);
            let code = if lt == func && rt.is_pointer() {
                Some(self.func_code_field(l.clone()))
            } else if rt == func && lt.is_pointer() {
                Some(self.func_code_field(r.clone()))
            } else {
                None
            };
            if let Some(code) = code {
                let pred = if matches!(op, AstBin::Ne) {
                    CmpPred::Ne
                } else {
                    CmpPred::Eq
                };
                let res = self.fb.cmp(pred, code, Value::Const(Const::Null));
                return (res, IrType::Bool);
            }
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
            let want = ptys.get(i).copied();
            // TA-8 (§3.8 #6): back-fill a PENDING payload arg against the case's
            // declared payload type, so `Enum.Case(.(1,2))` / `.Case(.(1,2))` with
            // a value-struct payload constructs the inner `.(…)` against `ptys[i]`
            // instead of lowering it to `undef` via the plain `self.expr` path. A
            // concrete payload arg, or a pending one with no declared payload slot,
            // takes the eager `self.expr` path verbatim — byte-identical to pre-TA-8.
            let (v, t) = match want {
                Some(pty) if arg_is_pending(a, src) => self
                    .lower_arg_targeted(pty, a, src)
                    // A pending payload that can't target-type to its declared
                    // payload type recovers with `undef(pty)` (the payload type,
                    // not a silent `undef(I64)` mis-coerced into the field), §3.4.
                    .unwrap_or_else(|| (undef(pty), pty)),
                _ => self.expr(a, src),
            };
            let want = want.unwrap_or(t);
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

    /// Lower `e` *target-typed against `target`* — the single canonical path for
    /// every target-typed dot-form construction site (local-init, assignment RHS,
    /// `return`, and — from TA-2 on — call arguments). Tries the four
    /// `try_target_typed_*` constructors in the canonical order
    /// **enum → tuple → ctor → initializer** and returns the first that fires;
    /// `None` if `e` is not a recognized dot-form for `target` (the caller then
    /// falls back to a plain `self.expr(e, src)`).
    ///
    /// The order is immaterial for correctness (§3.5): the four guards are
    /// pairwise disjoint on the `Expr` shape — `try_target_typed_enum` fires only
    /// on `DotIdent` / `Call(DotIdent)`, `try_target_typed_tuple` only on
    /// `Expr::Tuple`, `try_target_typed_ctor` only on `Call(DotIdent ".")`, and
    /// `try_target_typed_initializer` only on `Expr::Initializer` — so no single
    /// expression can match two of them. (The enum and ctor guards both look at
    /// `Call(DotIdent)`, but enum requires `target` be a payload-enum struct while
    /// ctor requires the callee name be exactly `"."`; for a `.(args)` against a
    /// non-enum struct only ctor fires, and for a `.Case(args)` only enum fires.)
    /// This is the single source of truth for the try-order; the three existing
    /// sites now delegate here (§9 Task 1).
    fn lower_arg_targeted(
        &mut self,
        target: IrType,
        e: &Expr,
        src: &str,
    ) -> Option<(Value, IrType)> {
        self.try_target_typed_enum(target, e, src)
            .or_else(|| self.try_target_typed_tuple(target, e, src))
            .or_else(|| self.try_target_typed_ctor(target, e, src))
            .or_else(|| self.try_target_typed_initializer(target, e, src))
    }

    /// Phase 1 of two-phase target-typed arg resolution (§3.1): walk `args`
    /// left-to-right; lower each **concrete** arg eagerly via [`Self::arg_value`]
    /// (same side-effect order as the eager path), caching `Some((Value, IrType))`
    /// at its index; leave each **pending** dot-form a `None` hole (it is lowered
    /// in [`Self::finish_args`] against its resolved param type). Returns the
    /// cached partial values (parallel to `args`, `None` at pending slots) and the
    /// sparse `ArgShape` vector resolution scores against
    /// ([`pick_overload_partial`] / [`StructTable::ctor_for_partial`]): a concrete
    /// slot is `Concrete(ty)`, a pending slot is `Pending(kind)`.
    ///
    /// Runs **exactly once** per call site (the resolved sub-path then calls
    /// `finish_args` once), so a pending arg is never lowered during a non-taken
    /// resolution probe. Wired into `lower_method_call`'s `has_pending` fork (TA-3).
    fn lower_args_phase1<'e>(
        &mut self,
        args: &'e [Expr],
        src: &'e str,
    ) -> (Vec<Option<(Value, IrType)>>, Vec<ArgShape<'e>>) {
        let mut partial: Vec<Option<(Value, IrType)>> = Vec::with_capacity(args.len());
        let mut shapes: Vec<ArgShape<'e>> = Vec::with_capacity(args.len());
        for a in args {
            if let Some(kind) = pending_kind(a, src) {
                // Pending: do NOT lower now; record its shape for resolution.
                partial.push(None);
                shapes.push(ArgShape::Pending(kind));
            } else {
                // Concrete: lower eagerly in source order (incl. ref/out via
                // `arg_value`), caching the value and its type.
                let (v, t) = self.arg_value(a, src);
                partial.push(Some((v, t)));
                shapes.push(ArgShape::Concrete(t));
            }
        }
        (partial, shapes)
    }

    /// Phase 2 of two-phase resolution (§3.1/§3.8): the single in-source-order
    /// emission pass. Walks arg indices `0..n` once; a **concrete** slot takes its
    /// cached `partial` value and coerces it to its resolved param type; a
    /// **pending** slot lowers NOW via [`Self::lower_arg_targeted`] against that
    /// param type, then coerces. `formal` is the param list **already sliced to
    /// exclude `this`** (the caller passes `&sig.params[1..]` for an instance call,
    /// `&sig.params[..]` for static/generic/ctor-after-`this`), matching the
    /// [`Self::pack_variadic_args`] contract. `variadic` is `Some(elem)` for a
    /// `params T[]` tail (pending tail slots target `elem`).
    ///
    /// **Arity-bounds safety:** asserts `args.len()` (== `partial.len()`) lies in
    /// `[fixed, fixed + variadic_slack]` of `formal` and recovers gracefully — a
    /// param index past `formal` falls back to the cached/I64 type rather than an
    /// unguarded `formal[i]` OOB. **Diagnostic recovery (§3.4):** a pending slot
    /// whose resolved param can't be target-typed recovers with `undef(param_ty)`
    /// (the *param* type, never a silent `undef(I64)` that would mis-coerce into a
    /// struct slot). Runs **exactly once** per call. Wired into `lower_method_call`'s
    /// `has_pending` fork (TA-3).
    fn finish_args(
        &mut self,
        formal: &[IrType],
        variadic: Option<IrType>,
        partial: Vec<Option<(Value, IrType)>>,
        args: &[Expr],
        src: &str,
    ) -> Vec<Value> {
        debug_assert_eq!(
            partial.len(),
            args.len(),
            "finish_args: partial cache must be parallel to args"
        );
        // The number of fixed (non-variadic-tail) params. For a variadic method
        // the last `formal` entry is the `T[]` slot, so `fixed = formal.len() - 1`
        // and every arg past it targets `elem`. For a normal method every arg
        // targets `formal[i]`. Arity should already have been checked by
        // resolution; assert the lower bound and recover above it.
        let fixed = match variadic {
            Some(_) => formal.len().saturating_sub(1),
            None => formal.len(),
        };
        debug_assert!(
            args.len() >= fixed,
            "finish_args: fewer args ({}) than fixed params ({fixed})",
            args.len()
        );
        // Lower/coerce every slot to a fully-concrete (Value, IrType) in source
        // order, then hand to `pack_variadic_args` (variadic) or emit directly.
        let mut lowered: Vec<(Value, IrType)> = Vec::with_capacity(args.len());
        for (i, a) in args.iter().enumerate() {
            // The param type this slot targets: a fixed param's declared type, or
            // the variadic element for a tail slot. If `i` somehow exceeds the
            // formal arity (arity-recovery), fall back to the cached/I64 type so we
            // never index `formal` out of bounds.
            let param_ty: IrType = if i < fixed {
                formal[i]
            } else if let Some(elem) = variadic {
                elem
            } else {
                // Past the formal arity with no variadic: recover with the cached
                // concrete type (or I64 for a pending hole) — no OOB.
                partial[i].as_ref().map(|(_, t)| *t).unwrap_or(IrType::I64)
            };
            let (v, t) = match &partial[i] {
                // Concrete: take the Phase-1 value, coerce to the resolved param.
                Some((v, t)) => (v.clone(), *t),
                // Pending: lower NOW against the resolved param type, with the
                // §3.4 diagnostic recovery (recover carrying the *param* type).
                None => self.lower_arg_targeted(param_ty, a, src).unwrap_or_else(|| {
                    // No diagnostic sink in this lowerer; recover with the param
                    // type (not a silent undef(I64)) so coercion below is a no-op
                    // and downstream IR stays type-correct.
                    (undef(param_ty), param_ty)
                }),
            };
            lowered.push((self.coerce(v, t, param_ty), param_ty));
        }
        match variadic {
            // Pack the fixed leading args + the tail into a fresh `T[]`. The
            // values are already coerced to their param types; `pack_variadic_args`
            // re-coerces (a no-op) and builds the array.
            Some(elem) => self.pack_variadic_args(formal, elem, lowered),
            None => lowered.into_iter().map(|(v, _)| v).collect(),
        }
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
                // FV-T4: a static method `Type.M` has no `$self` param, so calling
                // it through the uniform `code(target, args…)` convention would
                // shift every argument by one. Wrap it in a de-duplicated
                // `$mref$<full>($self /*ignored*/, P…){ return <full>(P…); }`
                // thunk and return the thunk's bare `Ptr` address (it coerces to
                // `Func${code, target=null}` at the boundary like a non-capturing
                // lambda). The thunk is emitted once in the emit pass.
                let thunk_sym = format!("$mref${}", sig.full_name);
                self.structs
                    .method_ref_thunks
                    .borrow_mut()
                    .entry(sig.full_name.clone())
                    .or_insert_with(|| MethodRefThunk {
                        thunk_sym: thunk_sym.clone(),
                        callee: sig.full_name.clone(),
                        ret: sig.ret,
                        params: sig.params.clone(),
                        bound: false,
                    });
                return Some((self.fb.global_addr(thunk_sym), IrType::Ptr));
            }
        }
        None
    }

    /// FV-T5: a BOUND instance method ref `obj.M` in a value position (e.g. passed
    /// to a HOF). The base is an *instance* (a local/field/`this`/rvalue of a class
    /// type), distinguishing this from the static `Type.M` path (whose base names a
    /// registered type — handled by [`try_method_ref`]). Builds a `Func$ { code =
    /// $mrefb$<full>, target = receiver body pointer }`: the de-duplicated thunk
    /// `$mrefb$<full>($self, P…){ return ((T)$self).M(P…); }` forwards `$self` as
    /// the receiver `this`, so the bound method runs on the right object.
    ///
    /// **Class receivers only** (Risk 7.9): a class `this` is a body `Ptr`, ABI-
    /// identical to the convention's `$self` `Ptr`, so the cast is implicit. A
    /// value-struct / `mut` / `ref` receiver needs a different `$self`-forwarding
    /// mode (pass-by-address with the receiver's mode) — that is declined here
    /// (returns `None`, no miscompile) and deferred. Virtual dispatch through a
    /// bound ref is also deferred: this binds the concrete `full_name` (so the
    /// test uses a non-virtual method).
    fn try_bound_method_ref(
        &mut self,
        base: &Expr,
        name: &str,
        src: &str,
    ) -> Option<(Value, IrType)> {
        // A static `Type.M` ref (base names a registered type) is handled by
        // `try_method_ref` — only take this path when the base is an instance.
        if let Expr::Ident(s) = base
            && self.lookup(s.text(src)).is_none()
            && self.structs.by_name.contains_key(s.text(src))
        {
            return None;
        }
        let (body_ptr, owner) = self.struct_base(base, src)?;
        // Risk 7.9: class receivers only in this slice. A value-struct / mut / ref
        // receiver needs a different `$self` forwarding mode — decline cleanly.
        if !matches!(self.structs.kinds[owner.0 as usize], StructKind::Ref) {
            return None;
        }
        // Resolve a non-static instance method `M` on the receiver's class.
        let sig = self.structs.methods[owner.0 as usize]
            .get(name)
            .and_then(|c| c.iter().find(|s| s.is_instance))?
            .clone();
        // `params[0]` is the leading `this` (a `Ref(owner)`); the thunk's explicit
        // params are the rest. The thunk forwards `$self` (the receiver) as `this`.
        let explicit: Vec<IrType> = sig.params.get(1..).unwrap_or(&[]).to_vec();
        let thunk_sym = format!("$mrefb${}", sig.full_name);
        self.structs
            .method_ref_thunks
            .borrow_mut()
            .entry(thunk_sym.clone())
            .or_insert_with(|| MethodRefThunk {
                thunk_sym: thunk_sym.clone(),
                callee: sig.full_name.clone(),
                ret: sig.ret,
                params: explicit,
                bound: true,
            });
        let code = self.fb.global_addr(thunk_sym);
        let fv = self.build_func_value(code, body_ptr);
        Some((fv, IrType::Struct(self.structs.func_struct)))
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
            // IT-T4: the `Ref` arm returns `(body, id)` for BOTH class and
            // interface receivers (an interface-typed value is `Ref(iface_id)`,
            // IT-T1) — no gating on class-ness. So an interface lvalue's body
            // pointer reaches the interface-dispatch branch in `lower_method_call`.
            Some((place, IrType::Ref(id))) => {
                let body = self.fb.load(place, IrType::Ptr);
                Some((body, id))
            }
            // A non-pointer lvalue (a scalar); an interface lvalue is `Ref`, so
            // it never lands here.
            Some(_) => None,
            None => {
                // Non-lvalue base (e.g. `new C().x`): a reference rvalue is
                // itself the body pointer. Also covers an interface-typed rvalue
                // (e.g. a method returning `IShape`) — its `Ref(iface_id)` flows
                // straight through as the body pointer.
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
            // Run the constructor matching the args; coercion makes each arg its
            // declared param type. TA-5 fork (§3.7, mirrors the TA-3
            // `lower_method_call` fork): the hot path is "no pending args" — a
            // single O(args) syntactic scan gates the two-phase machinery. With no
            // target-typed dot-form arg the EXISTING arity-only `ctor_for` + eager
            // loop runs verbatim (byte-identical to pre-TA-5). When any arg is a
            // pending dot-form, resolve the ctor with the shape gate
            // (`ctor_for_partial`) and emit the final args via `finish_args` against
            // the resolved ctor's params past the leading `this` — pending slots
            // lowered NOW against their formal param type. Exactly one Phase-1 +
            // one `finish_args`, all in the current block (the values dominate the
            // single terminal ctor `call`), so each pending arg is constructed once.
            let args = ctor_args(operand);
            if args.iter().any(|a| arg_is_pending(a, src)) {
                let (partial, shapes) = self.lower_args_phase1(args, src);
                if let Some(ctor) = self.structs.ctor_for_partial(id, &shapes) {
                    // Formal params exclude the leading `this` (the ctor's own
                    // receiver, prepended below). Ctors are not variadic.
                    let formal: Vec<IrType> = ctor.params[1..].to_vec();
                    let mut call_args = vec![p.clone()];
                    call_args.extend(self.finish_args(&formal, None, partial, args, src));
                    self.fb.call(ctor.full_name, call_args, IrType::Void);
                }
            } else if let Some(ctor) = self.structs.ctor_for(id, args.len()) {
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
        // TA-6 fork (§3.7, mirrors TA-5/TA-3): the hot path is "no pending args" —
        // a single O(args) syntactic scan gates the two-phase machinery. With no
        // pending dot-form arg the EXISTING arity-only `ctor_for` + eager loop runs
        // verbatim (byte-identical to pre-TA-6). When any arg is a pending dot-form,
        // resolve the ctor with the shape gate (`ctor_for_partial`) and emit the
        // final args via `finish_args` against the ctor's params past the leading
        // `this`; pending slots are lowered NOW against their formal param type.
        //
        // **Nesting** (`.( .(…), .(…) )`): an inner `.(…)` arg is itself pending, so
        // `finish_args` back-fills it via `lower_arg_targeted(inner_param_ty, …)` →
        // `try_target_typed_ctor` → `construct_value_struct` (REENTRANT). The inner
        // call runs its OWN Phase-1/`finish_args` against the inner ctor's params on
        // its OWN fresh stack-local `partial`/`shapes`/`slot` — no shared-state
        // clobber (these vectors and the alloca are function-local; only `self.fb`,
        // the intended emission target, is shared). All emission stays in the
        // current block, so every value dominates the single terminal ctor `call`.
        if args.iter().any(|a| arg_is_pending(a, src)) {
            let (partial, shapes) = self.lower_args_phase1(args, src);
            if let Some(ctor) = self.structs.ctor_for_partial(id, &shapes) {
                // Formal params exclude the leading `this` (the slot, prepended
                // below). Ctors are not variadic.
                let formal: Vec<IrType> = ctor.params[1..].to_vec();
                let mut call_args = vec![slot.clone()];
                call_args.extend(self.finish_args(&formal, None, partial, args, src));
                self.fb.call(ctor.full_name, call_args, IrType::Void);
            }
        } else if let Some(ctor) = self.structs.ctor_for(id, args.len()) {
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

    /// A target-typed `.{ field = value }` initializer against a known `target`
    /// (a local's declared type, an assignment place, a return type). `None` if
    /// `e` isn't an `Initializer`.
    ///
    /// **TA-1 silent-undef fix (§3.4 / §2 critical asymmetry):** a *leading-dot*
    /// `.{ }` (a `DotIdent` base) only has a meaning the first slice can build for
    /// a **value struct** `Struct(id)`. For any other target — notably a `Ref(id)`
    /// (class) — `lower_initializer` used to return a silent `(undef(Ref(id)),
    /// Ref(id))` that the `.or_else` chain accepted, masking a real hole. We now
    /// **decline** (return `None`) for a `.{ }` whose target isn't `Struct(id)`,
    /// mirroring `try_target_typed_ctor`'s `Struct(id)` gate, so the caller falls
    /// through to its plain path instead of accepting an undef. (`.{ }` against a
    /// class — `new`-allocate + run field inits + assign entries — is a documented
    /// follow-up, §10.) A concrete-base initializer (`new T() { … }` / `Type
    /// { … }`) is unaffected: it never hit the undef arm and still lowers here.
    fn try_target_typed_initializer(
        &mut self,
        target: IrType,
        e: &Expr,
        src: &str,
    ) -> Option<(Value, IrType)> {
        if let Expr::Initializer { base, entries, .. } = e {
            // Decline a leading-dot `.{ }` whose target is not a value struct —
            // the only shape `lower_initializer` would answer with a silent undef.
            if matches!(&**base, Expr::DotIdent { .. }) && !matches!(target, IrType::Struct(_)) {
                return None;
            }
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

    /// FV-T6b: record the resolved callee function-parameter signatures for any
    /// INLINE-lambda arguments of a call, so the emit pass can bind each lambda
    /// body's params at the right IR types. `param_fn_sigs` is the resolved
    /// callee's per-EXPLICIT-param inner `(ret, ptys)` (`None` for a non-function
    /// param), parallel to `args` (no leading `this`). For each arg that is an
    /// `Expr::Lambda` collected with a `$lambdaN` symbol (T6a), key its recorded
    /// signature into `inline_lambda_sigs`. A no-op when the callee has no
    /// function params (`param_fn_sigs` empty) — the common hot path. Must be
    /// called BEFORE lowering the args (so the symbol is bound before
    /// `Expr::Lambda` lowers and `emit_closure` runs in the later emit pass);
    /// calling it twice is idempotent (same key → same value).
    fn record_inline_lambda_sigs(
        &self,
        param_fn_sigs: &[Option<(IrType, Vec<IrType>)>],
        args: &[Expr],
    ) {
        if param_fn_sigs.is_empty() {
            return;
        }
        for (i, a) in args.iter().enumerate() {
            // Look through a parenthesized lambda (`(x => …)`), the only wrapper a
            // lambda arg realistically carries.
            let mut e = a;
            while let Expr::Paren { inner, .. } = e {
                e = inner;
            }
            if let Expr::Lambda { span, .. } = e
                && let Some(sym) = self.structs.lambda_names.get(span)
                && let Some(Some((ret, ptys))) = param_fn_sigs.get(i)
            {
                self.structs
                    .inline_lambda_sigs
                    .borrow_mut()
                    .insert(sym.clone(), (*ret, ptys.clone()));
            }
        }
    }

    /// FV-T7: if `e` is a lambda returned directly from the current method
    /// (`return x => x + n;`), key its recorded `$lambdaN` symbol into
    /// `inline_lambda_sigs` with the method's declared return signature
    /// (`ret_fn_sig`), so the emit pass binds the lambda body's params at the
    /// resolved types. The return-position analogue of [`Self::record_inline_lambda_sigs`]
    /// (which target-types a call-arg lambda from the callee param sig). A no-op
    /// when the return type isn't a function type (`ret_fn_sig` is `None`) or `e`
    /// isn't a collected lambda. Idempotent (same key → same value).
    fn record_return_lambda_sig(&self, e: &Expr) {
        let Some((ret, ptys)) = self.ret_fn_sig.clone() else {
            return;
        };
        // Look through a parenthesized lambda (`(x => …)`).
        let mut e = e;
        while let Expr::Paren { inner, .. } = e {
            e = inner;
        }
        if let Expr::Lambda { span, .. } = e
            && let Some(sym) = self.structs.lambda_names.get(span)
        {
            self.structs
                .inline_lambda_sigs
                .borrow_mut()
                .insert(sym.clone(), (ret, ptys));
        }
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
    ///
    /// `tid` may be a CLASS or an INTERFACE: for a class the target set is the
    /// subtree under `tid` (`is_subtype_of`); for an interface it is every class
    /// whose transitively-flattened `iface_bases` contains `tid` (so `x is IA`
    /// holds for a class implementing only `IB : IA`). The source value `obj`
    /// may itself be interface-typed — its `$header` is read via a RAW
    /// pointer-indexed GEP (offset 0), never `field_addr`, because an interface
    /// id has an empty `StructDef` and would make `field_addr` invalid.
    fn type_test(&mut self, obj: Value, tid: StructId) -> Option<Value> {
        let tid_is_iface = is_interface(&self.structs, tid);
        let targets: Vec<StructId> = (0..self.structs.defs.len() as u32)
            .map(StructId)
            .filter(|&c| {
                if self.structs.vimpls[c.0 as usize].is_empty() {
                    return false;
                }
                if tid_is_iface {
                    self.structs.iface_bases[c.0 as usize].contains(&tid)
                } else {
                    self.is_subtype_of(c, tid)
                }
            })
            .collect();
        if targets.is_empty() {
            return None;
        }
        // Read `$header` (offset 0) via a raw pointer-indexed GEP, so the test
        // works even when the SOURCE value is interface-typed (empty StructDef).
        let hdr_addr = self
            .fb
            .elem_addr(obj, IrType::Ptr, Value::int(0, IrType::I64));
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
        // `ot` may be `Ref(class)` or `Ref(iface)` — both are pointer-like and
        // carry a `$header` at offset 0; `type_test` reads it via a raw GEP.
        if let IrType::Ref(_) = ot
            && let Some(tid) = self.type_id_of(rhs, src)
            && let Some(test) = self.type_test(ov, tid)
        {
            return (test, IrType::Bool);
        }
        (Value::bool(false), IrType::Bool)
    }

    /// `obj as T` → `obj` typed as `T` when the runtime type matches, else `null`.
    fn lower_as(&mut self, lhs: &Expr, rhs: &Expr, src: &str) -> (Value, IrType) {
        let (ov, ot) = self.expr(lhs, src);
        // `ot` may be `Ref(class)` or `Ref(iface)`. `as IFace` returns
        // `Ref(iface_id)` (pointer-like; the typed-null `select` verifies).
        if let IrType::Ref(_) = ot
            && let Some(tid) = self.type_id_of(rhs, src)
            && let Some(test) = self.type_test(ov.clone(), tid)
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

    /// Lower a generic-method call `base<targs>(args)`. Mirrors
    /// `lower_method_call`'s three shapes, but resolves against the
    /// `gen_method_sigs` composite key `(owner, name, type_codes)`:
    /// - **bare `M<T>(x)`** (`base` is `Ident`): owner candidates `[cur_type,
    ///   None]`; an instance method prepends the current `this`.
    /// - **qualified static `Type.M<T>(x)`** (`base` is `Member` whose receiver
    ///   names a type): owner candidates `[by_name[Type], None]`; receiver-less.
    /// - **instance `recv.M<T>(args)`** (`base` is `Member` with a value
    ///   receiver): owner = `instance_recv_owner(recv)` — the SAME rule the
    ///   collector used (R4) — prepend `struct_base(recv)`'s body pointer.
    ///
    /// Returns `None` (no IR emitted) when no key resolves — an unsupported
    /// instance receiver or an absent monomorph — so the caller diagnoses
    /// cleanly instead of emitting a dangling call.
    fn lower_generic_call(
        &mut self,
        base: &Expr,
        mname: &str,
        targs: &[AstType],
        args: &[Expr],
        src: &str,
    ) -> Option<(Value, IrType)> {
        let argtys: Vec<IrType> = targs
            .iter()
            .map(|a| lower_ty_env(a, src, self.structs, self.env))
            .collect();
        let codes = type_codes(&argtys);
        // A non-capturing resolver (takes the table explicitly) so it never holds
        // a `self` borrow across the later `&mut self` IR emission.
        let lookup_sig = |t: &StructTable, owner: Option<StructId>| -> Option<MethodSig> {
            t.gen_method_sigs
                .get(&(owner, mname.to_string(), codes.clone()))
                .cloned()
        };

        // Classify the callee base shape (mirrors `lower_method_call`).
        let (sig, recv): (MethodSig, Option<(Value, StructId)>) = match base {
            // Bare `M<T>(x)`: same-class (`cur_type`) then the retained `None`
            // bucket (bare cross-class static, e.g. `list_hof.bf`'s `Map`).
            Expr::Ident(_) => {
                let sig = [self.cur_type, None]
                    .into_iter()
                    .find_map(|o| lookup_sig(self.structs, o))?;
                // An instance same-class generic call (`Wrap<int32>(x)` inside
                // another instance method) prepends the current `this`.
                let recv = if sig.is_instance {
                    let (slot, ty @ IrType::Ref(id)) = self.this_slot.clone()? else {
                        return None;
                    };
                    Some((self.fb.load(slot, ty), id))
                } else {
                    None
                };
                (sig, recv)
            }
            Expr::Member { base: mbase, .. } => {
                // Qualified static `Type.M<T>(x)`: the receiver names a registered
                // type. `[by_name[Type], None]` — receiver-less.
                let qual_static = qualified_gen_owner(mbase, src, self.structs).and_then(|tid| {
                    [Some(tid), None]
                        .into_iter()
                        .find_map(|o| lookup_sig(self.structs, o))
                        .filter(|s| !s.is_instance)
                });
                if let Some(sig) = qual_static {
                    (sig, None)
                } else {
                    // Instance `recv.M<T>(args)`: resolve the receiver owner with
                    // the SAME rule the collector used (R4), look up the key, then
                    // get a real body pointer via `struct_base`.
                    let owner = {
                        let lookup = |n: &str| self.lookup(n).map(|(_, ty)| ty);
                        instance_recv_owner(
                            mbase,
                            src,
                            &lookup,
                            self.cur_type,
                            self.env,
                            self.structs,
                        )?
                    };
                    let sig = lookup_sig(self.structs, Some(owner))?;
                    if !sig.is_instance {
                        return None;
                    }
                    let (body_ptr, owner_id) = self.struct_base(mbase, src)?;
                    // The collector and `struct_base` must agree on the owner, or
                    // the prepended `this` would mismatch the symbol's ABI.
                    debug_assert_eq!(
                        owner_id, owner,
                        "generic instance-call owner skew: struct_base={owner_id:?} \
                         collector={owner:?} (R4)"
                    );
                    (sig, Some((body_ptr, owner_id)))
                }
            }
            _ => return None,
        };

        // Value-arity guard (mirrors the old `sig.params.len() == args.len()`
        // gate and `pick_overload`'s arity discrimination): the composite key
        // `(owner, name, type_codes)` cannot distinguish two same-type-arg
        // overloads of differing *value* arity (e.g. `Test<T>()` vs
        // `Test<T>(T)`), so the recorded sig may be the wrong overload. Bail out
        // (no IR) on a value-arity mismatch — a clean diagnosis, never a
        // mis-arity call — so a colliding overload falls through gracefully.
        let leading = if recv.is_some() { 1 } else { 0 };
        let formal_len = sig.params.len() - leading;
        let arity_ok = match sig.variadic {
            Some(_) => args.len() + 1 >= formal_len, // fixed params + a (possibly empty) T[]
            None => args.len() == formal_len,
        };
        if !arity_ok {
            return None;
        }

        // FV-T6b: before evaluating the args, record any INLINE-lambda arg's
        // param types from this resolved generic-method sig (`param_fn_sigs` is
        // by explicit param, parallel to `args` — the leading `this` is not in
        // it). This is the headline path: `xs.Map<int32>(x => x*10)` — the `x`
        // binds at the callee's `function R(T) f` param type.
        self.record_inline_lambda_sigs(&sig.param_fn_sigs, args);

        // Build the call args: a leading `this` for an instance method, then the
        // explicit args coerced to the signature's formal params (variadic-aware).
        let mut call_args: Vec<Value> = Vec::with_capacity(args.len() + 1);
        if let Some((body_ptr, _)) = &recv {
            call_args.push(body_ptr.clone());
        }
        // TA-7 fork (§3.7/§3.8 #3, mirrors the TA-3/TA-4 forks): a generic-method
        // call resolves its monomorph by the EXPLICIT type-args (the mangled key),
        // not the value args — so the `sig` is already chosen here, no overload
        // picking. With a pending dot-form value arg, back-fill it against the
        // pre-built `gen_method_sigs` param type, UN-OFFSET past any leading `this`
        // (`formal = &sig.params[leading..]`: `leading = 1` for an instance call's
        // already-prepended receiver, `0` for a static one). The hot path (no
        // pending args) runs the eager `arg_vals` loop below verbatim.
        if args.iter().any(|a| arg_is_pending(a, src)) {
            let (partial, _shapes) = self.lower_args_phase1(args, src);
            let formal: Vec<IrType> = sig.params[leading..].to_vec();
            call_args.extend(self.finish_args(&formal, sig.variadic, partial, args, src));
        } else {
            let arg_vals: Vec<(Value, IrType)> =
                args.iter().map(|a| self.arg_value(a, src)).collect();
            if let Some(elem) = sig.variadic {
                // `params T[]`: pack overflow args into a fresh `T[]` (formal params
                // exclude the leading `this`).
                let formal = &sig.params[leading..];
                let packed = self.pack_variadic_args(formal, elem, arg_vals);
                call_args.extend(packed);
            } else {
                for (i, (v, ty)) in arg_vals.into_iter().enumerate() {
                    let pt = sig.params.get(leading + i).copied().unwrap_or(ty);
                    call_args.push(self.coerce(v, ty, pt));
                }
            }
        }
        // Hard assert (doc §5.4 / §7): the assembled operand count must exactly
        // match the (direct-call) signature's param count. A drift here would be
        // an ABI bug the LLVM verifier wouldn't necessarily catch on a typed
        // direct call, so fail loudly rather than emit a mis-arity call.
        assert_eq!(
            call_args.len(),
            sig.params.len(),
            "generic-method call arity mismatch for {}: {} args vs {} params",
            sig.full_name,
            call_args.len(),
            sig.params.len()
        );
        let r = self.fb.call(sig.full_name.clone(), call_args, sig.ret);
        Some((r, sig.ret))
    }

    /// Emit an interface dispatch on an already-evaluated body pointer
    /// (itables.md §5/§5.6 / §5 T6). `body_ptr` is the object pointer whose
    /// *static* type is the interface `iface_id`; `mname` names a method in
    /// `imethods[iface_id]`. The slot is globally fixed for `(interface, method)`
    /// (`iface_slot_base[iface] + midx`), so the concrete class is never needed:
    /// load the vtable from the object header, index the slot, `call_indirect`.
    /// The whole sequence is emitted inline in the current block (like a virtual
    /// call), so every value dominates its use trivially (no new block/phi).
    ///
    /// Shared by `lower_method_call`'s interface-receiver branch (`obj.M()` where
    /// `obj : Ref(iface)`) and the bare-call path: a sibling unqualified call
    /// inside a DEFAULT interface-method body (`A()` inside `I.D`'s body, where
    /// `this : Ref(iface)`) — the bare `A()` becomes an interface dispatch on
    /// `this`, NOT a direct call (an abstract sibling has no direct symbol).
    /// Returns `None` if `mname` isn't an interface slot of `iface_id`.
    fn emit_iface_dispatch(
        &mut self,
        body_ptr: Value,
        iface_id: StructId,
        mname: &str,
        arg_vals: Vec<(Value, IrType)>,
    ) -> Option<(Value, IrType)> {
        debug_assert!(
            matches!(self.structs.kinds[iface_id.0 as usize], StructKind::Interface),
            "emit_iface_dispatch on non-interface id {}",
            iface_id.0
        );
        let midx = self.structs.imethods[iface_id.0 as usize]
            .iter()
            .position(|(n, _)| n == mname)?;
        let sig = self.structs.imethods[iface_id.0 as usize][midx].1.clone();
        let base_slot = self.structs.iface_slot_base[&iface_id];
        let slot = base_slot + midx;
        // Header is at offset 0. An interface has an EMPTY StructDef (no
        // `$header` field), so use a RAW pointer-indexed GEP (`elem_addr`
        // body_ptr[0] : Ptr), NOT `field_addr` through the interface id.
        let hdr = self
            .fb
            .elem_addr(body_ptr.clone(), IrType::Ptr, Value::int(0, IrType::I64));
        let vtbl = self.fb.load(hdr, IrType::Ptr);
        let slotp = self
            .fb
            .elem_addr(vtbl, IrType::Ptr, Value::int(slot as i128, IrType::I64));
        let fnptr = self.fb.load(slotp, IrType::Ptr);
        // `this`-leading; coerce each arg to the slot sig's param type
        // (params[0] is `this : Ref(iface_id)`; formals start at index 1).
        let mut call_args = vec![body_ptr];
        let mut pidx = 1;
        for (v, t) in arg_vals {
            let pt = sig.params.get(pidx).copied().unwrap_or(t);
            call_args.push(self.coerce(v, t, pt));
            pidx += 1;
        }
        let r = self.fb.call_indirect(fnptr, call_args, sig.ret);
        Some((r, sig.ret))
    }

    fn lower_method_call(
        &mut self,
        base: &Expr,
        mname: &str,
        args: &[Expr],
        src: &str,
    ) -> (Value, IrType) {
        // TA-3 fork (§3.7): the hot path is "no pending args" — a single O(args)
        // syntactic scan gates the whole two-phase machinery. When any arg is a
        // target-typed dot-form (`.(…)` / `.{ }` / `.Case[(…)]`), divert to the
        // pending-aware lowerer (which shares ONE phase-1 + ONE finish_args across
        // the base/static/instance sub-paths). Otherwise the existing eager path
        // below runs verbatim — byte-identical to pre-TA-3 — so the run/verify/
        // parser corpora (no pending args) are behavior-preserved.
        if args.iter().any(|a| arg_is_pending(a, src)) {
            return self.lower_method_call_pending(base, mname, args, src);
        }
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
                // FV-T6b: record inline-lambda arg param types from this
                // qualified-static HOF method's resolved sig.
                self.record_inline_lambda_sigs(&sig.param_fn_sigs, args);
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
        // Interface dispatch (IT-T5, itables.md §5/§5.6). A SEPARATE branch that
        // MUST come BEFORE the methods-keyed block below: an abstract interface
        // method is recorded in `imethods` but NOT in `methods` (it has no body),
        // so the methods-keyed `pick_overload` would fail and the call would fall
        // to the undef catch-all. Here the receiver's *static* type is an
        // interface (`struct_base` yields `(body_ptr, owner_id)` with
        // `kinds[owner_id] == Interface`, since IT-T1 makes interface types
        // `Ref(iface_id)` and `struct_base`'s `Ref` arm returns `(body, id)`
        // regardless of class-ness — see IT-T4). The slot is globally fixed for
        // `(interface, method)` (IT-T3's `iface_slot_base[iface] + midx`), so the
        // concrete class is never needed: load the vtable from the object header,
        // index the slot, and `call_indirect`. The whole sequence is emitted
        // inline in the current block (like the existing virtual call), so every
        // value dominates its use trivially (no new block, no phi — R8-safe).
        if let Some((body_ptr, owner_id)) = self.struct_base(base, src)
            && matches!(self.structs.kinds[owner_id.0 as usize], StructKind::Interface)
            && let Some(r) = self.emit_iface_dispatch(body_ptr, owner_id, mname, arg_vals.clone())
        {
            return r;
        }
        // Instance call `obj.Method(args)` / `this.Method(args)`. `members: true`
        // admits instance overloads (matched past `this`) and statics.
        if let Some((body_ptr, owner_id)) = self.struct_base(base, src)
            && let Some(sig) = self.structs.methods[owner_id.0 as usize]
                .get(mname)
                .and_then(|cands| pick_overload(cands, &arg_tys, true))
                .cloned()
        {
            // FV-T6b: record any inline-lambda arg's param types from the
            // resolved sig (a non-generic HOF instance method, e.g.
            // `scaled.Filter(x => x > 15)`). The lambda's value was already
            // lowered above; this only feeds the emit pass.
            self.record_inline_lambda_sigs(&sig.param_fn_sigs, args);
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

    /// The `has_pending` fork of [`Self::lower_method_call`] (TA-3, §3.1/§3.8 #1):
    /// the call has at least one target-typed dot-form arg (`.(…)` / `.{ }` /
    /// `.Case[(…)]`) whose `IrType` is the formal param's, only known after
    /// overload resolution.
    ///
    /// **Two-phase, single emission (the crux):**
    /// - [`Self::lower_args_phase1`] runs **exactly once** at the top: it lowers
    ///   every *concrete* arg eagerly in source order into a sparse `partial`
    ///   cache (a `None` hole at each pending slot) and builds the sparse `shapes`
    ///   vector. The base/static/instance sub-paths all **share** this one cache +
    ///   shape vector — a pending arg is never lowered during a non-taken
    ///   sub-path's resolution probe (`pick_overload_partial` only reads `shapes`).
    /// - The resolving sub-path then calls [`Self::finish_args`] **exactly once**:
    ///   it walks `0..n` in source order, takes each concrete slot's cached value
    ///   (coerced to its resolved param) and lowers each pending slot NOW against
    ///   its resolved param type, then packs variadics. So each pending arg is
    ///   constructed exactly once (no double-emit) and the receiver `this` is
    ///   prepended exactly as the eager path does.
    ///
    /// **Ordering rule (§3.1, correctness blocker #1):** concrete args emit in
    /// Phase 1 in source order; pending args emit in Phase 2 in source order. The
    /// only observable reorder vs the eager path is that a pending arg's
    /// construction side effects are observed AFTER all concrete args of the same
    /// call (a documented caveat — `targ_eval_order.bf` pins the concrete-args
    /// guarantee). We do NOT claim full eval-order equivalence.
    ///
    /// **SSA dominance (§5.4, blocker #3):** every value (Phase-1 concrete and
    /// Phase-2 pending constructions alike) is emitted into the *current* block —
    /// the constructors (`construct_value_struct` / `build_enum_value` /
    /// `lower_initializer`) only alloca/store/call in place, never branch — so the
    /// fully-assembled `call_args` all dominate the single terminal `call`. For a
    /// null-conditional `a?.M(.(…))` the current block is the `nonnull` block
    /// (`lower_conditional_call` switched into it before calling here), which
    /// dominates the call, so the pending construction stays on the non-null path.
    fn lower_method_call_pending(
        &mut self,
        base: &Expr,
        mname: &str,
        args: &[Expr],
        src: &str,
    ) -> (Value, IrType) {
        // Phase 1 — ONCE. Concrete args lowered in source order into holes; the
        // sparse `shapes` drive resolution across ALL three sub-paths below.
        let (partial, shapes) = self.lower_args_phase1(args, src);
        // `self.structs` is a shared `&'a StructTable` (lifetime independent of
        // `self`), so copying the reference lets resolution read it without
        // holding a `self` borrow across the later `&mut self` `finish_args` call.
        let structs: &StructTable = self.structs;

        // `base.Method(args)`: a direct (non-virtual) call to the nearest base
        // class's `Method`, with the current `this`. Mirrors the eager base path,
        // but resolves with the shared shapes and emits the final args via
        // `finish_args` (pending slots lowered against the resolved param types).
        if let Expr::Base(_) = base
            && let Some((slot, this_ty @ IrType::Ref(cur_id))) = self.this_slot.clone()
        {
            let mut bid = structs.bases[cur_id.0 as usize];
            while let Some(id) = bid {
                let sig = structs.methods[id.0 as usize]
                    .get(mname)
                    .and_then(|cands| pick_overload_partial(cands, &shapes, true, structs))
                    .cloned();
                if let Some(sig) = sig {
                    let this_v = self.fb.load(slot, this_ty);
                    // Instance method: formal params exclude the leading `this`.
                    let formal: Vec<IrType> = sig.params[1..].to_vec();
                    let mut call_args = vec![this_v];
                    call_args.extend(self.finish_args(&formal, sig.variadic, partial, args, src));
                    let r = self.fb.call(sig.full_name, call_args, sig.ret);
                    return (r, sig.ret);
                }
                bid = structs.bases[id.0 as usize];
            }
        }

        // Qualified static call `Type.Method(args)`: the base names a registered
        // type (not a local). `members: false` keeps only static overloads.
        if let Expr::Ident(s) = base {
            let name = s.text(src);
            if self.lookup(name).is_none()
                && let Some(&id) = structs.by_name.get(name)
            {
                let sig = structs.methods[id.0 as usize]
                    .get(mname)
                    .and_then(|cands| pick_overload_partial(cands, &shapes, false, structs))
                    .cloned();
                if let Some(sig) = sig {
                    // Static method: formals are the whole param list (no `this`).
                    let formal: Vec<IrType> = sig.params.clone();
                    let call_args = self.finish_args(&formal, sig.variadic, partial, args, src);
                    let r = self.fb.call(sig.full_name, call_args, sig.ret);
                    return (r, sig.ret);
                }
            }
        }

        // Instance call `obj.Method(args)` / `this.Method(args)`. `members: true`
        // admits instance overloads (matched past `this`) and statics. (Interface
        // dispatch with a pending arg is out of the first slice — an abstract
        // interface method has no body to target against here; such a call falls
        // through to the unresolved diagnostic below rather than mis-dispatching.)
        if let Some((body_ptr, owner_id)) = self.struct_base(base, src) {
            let sig = structs.methods[owner_id.0 as usize]
                .get(mname)
                .and_then(|cands| pick_overload_partial(cands, &shapes, true, structs))
                .cloned();
            if let Some(sig) = sig {
                // Formals exclude `this` for an instance method; a static method
                // reached through an instance receiver takes the whole list.
                let formal: Vec<IrType> = if sig.is_instance {
                    sig.params[1..].to_vec()
                } else {
                    sig.params.clone()
                };
                let mut call_args = Vec::new();
                if sig.is_instance {
                    call_args.push(body_ptr.clone());
                }
                call_args.extend(self.finish_args(&formal, sig.variadic, partial, args, src));
                // Virtual dispatch through the object's vtable when the method
                // occupies a slot on the receiver's static type; else a direct call.
                if sig.is_instance
                    && let Some(&vslot) = structs.vslots[owner_id.0 as usize].get(mname)
                {
                    let hdr = self.fb.field_addr(body_ptr, owner_id, 0);
                    let vtbl = self.fb.load(hdr, IrType::Ptr);
                    let slotp = self.fb.elem_addr(
                        vtbl,
                        IrType::Ptr,
                        Value::int(vslot as i128, IrType::I64),
                    );
                    let fnptr = self.fb.load(slotp, IrType::Ptr);
                    let r = self.fb.call_indirect(fnptr, call_args, sig.ret);
                    return (r, sig.ret);
                }
                let r = self.fb.call(sig.full_name, call_args, sig.ret);
                return (r, sig.ret);
            }
        }
        // Unresolved — concrete args were already evaluated for their effects in
        // Phase 1 (matching the eager path's "args already evaluated" behavior).
        (undef(IrType::I64), IrType::I64)
    }

    /// The `has_pending` fork of the bare-name / free-fn / local-fn / fn-value
    /// call path (TA-4, §3.8 #2): a call `name(args)` (callee an `Ident`) with at
    /// least one target-typed dot-form arg whose `IrType` is the resolved
    /// signature's param type. Mirrors the eager `Expr::Ident`-callee sub-paths but
    /// back-fills each pending arg against the known signature:
    ///
    /// - **Local (nested) fn** (`local_fns`) and **fn-value/fn-ptr local**
    ///   (`fn_sigs`): the param types `ptys[i]` are fully known up front, so
    ///   `finish_args(ptys, …)` lowers each pending slot NOW against `ptys[i]`
    ///   (these forms are never `params T[]`, so `variadic = None`).
    /// - **Same-type overload** (`self.methods`): resolve with the shape gate via
    ///   `pick_overload_partial`, then emit via `finish_args` against the chosen
    ///   sig's params (these are `this`-less candidates — statics / free fns, so
    ///   `formal = &sig.params[..]`; variadic-aware).
    /// - **Unresolved external** (Win32/CRT): there is NO signature to target a
    ///   pending arg against, so a pending arg to such a call cannot be
    ///   target-typed — a DIAGNOSED error (§6 "Unresolved external"). With no
    ///   diagnostic sink in this lowerer we recover cleanly: concrete args were
    ///   already evaluated for effect in Phase 1; each remaining pending arg is
    ///   evaluated FOR EFFECT (its sub-expressions' side effects run, exactly as an
    ///   eager unresolved-external arg would) — never a silent `undef` operand into
    ///   the call. The sibling-interface-dispatch path (a bare call inside an
    ///   interface default body) is intentionally NOT taken here: an abstract
    ///   interface method has no concrete body to target a pending arg against
    ///   (mirroring `lower_method_call_pending`'s interface-out-of-first-slice
    ///   stance), so such a call falls through to this external recovery.
    ///
    /// Single Phase-1 + single `finish_args` (each runs once), all emission in the
    /// current block, so the assembled `call_args` dominate the terminal call.
    fn lower_ident_call_pending(
        &mut self,
        name: &str,
        args: &[Expr],
        src: &str,
    ) -> (Value, IrType) {
        // Phase 1 — ONCE. Concrete args lowered in source order into holes; the
        // sparse `shapes` drive overload resolution (only the overload sub-path
        // reads them; the local-fn / fn-value sub-paths key on `name` directly).
        let (partial, shapes) = self.lower_args_phase1(args, src);

        // A local (nested) function in scope → a direct call to its emitted
        // `$localfn{N}` symbol; pending args back-fill against its known `ptys`.
        if let Some((sym, ret, ptys)) = self.local_fns.get(name).cloned() {
            let call_args = self.finish_args(&ptys, None, partial, args, src);
            return (self.fb.call(sym, call_args, ret), ret);
        }

        // A function-value local/param (`function R(P) f`): `f(args)` loads the
        // `$Func` value's `code`/`target` and `call_indirect`s; pending args
        // back-fill against the fn-value's known `ptys`. `$self` (the env or null)
        // is always operand 0 (no closure-ness branch — see the eager path).
        if let Some((ret, ptys)) = self.fn_sigs.get(name).cloned()
            && let Some((slot, _)) = self.lookup(name)
        {
            let fid = self.structs.func_struct;
            let code = {
                let a = self.fb.field_addr(slot.clone(), fid, 0);
                self.fb.load(a, IrType::Ptr)
            };
            let target = {
                let a = self.fb.field_addr(slot, fid, 1);
                self.fb.load(a, IrType::Ptr)
            };
            let mut call_args: Vec<Value> = vec![target];
            call_args.extend(self.finish_args(&ptys, None, partial, args, src));
            debug_assert_eq!(
                call_args.len(),
                ptys.len() + 1,
                "function-value pending-call arity drift for `{name}`: {} args vs {} params + $self",
                call_args.len(),
                ptys.len()
            );
            return (self.fb.call_indirect(code, call_args, ret), ret);
        }

        // Same-type overload (statics / free fns — `this`-less candidates). Resolve
        // with the shape gate, then emit via `finish_args` against the chosen sig's
        // params (no `this` to slice off; variadic-aware).
        let structs: &StructTable = self.structs;
        let resolved = self
            .methods
            .get(name)
            .and_then(|cands| pick_overload_partial(cands, &shapes, false, structs))
            .cloned();
        if let Some(sig) = resolved {
            let formal: Vec<IrType> = sig.params.clone();
            let call_args = self.finish_args(&formal, sig.variadic, partial, args, src);
            let r = self.fb.call(sig.full_name, call_args, sig.ret);
            return (r, sig.ret);
        }

        // Unresolved external — no signature to target a pending arg against
        // (§6). Concrete args already ran for effect in Phase 1; evaluate each
        // remaining pending arg FOR EFFECT (no target type ⇒ `lower_arg_targeted`
        // would decline anyway), so its side effects still happen, then default the
        // result to i64. This is the clean recovery for a pending-arg-to-external
        // call — never a silent `undef` operand.
        for (a, slot) in args.iter().zip(&partial) {
            if slot.is_none() {
                // A pending dot-form with no resolved param type: evaluate it for
                // its side effects (it can't be target-typed, so the result is
                // discarded — not passed as an operand).
                let _ = self.expr(a, src);
            }
        }
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
                .lower_arg_targeted(ty, value, src)
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
    /// Build a `Func$ { code, target }` value-struct in a fresh alloca (two
    /// stores + a load) and return the loaded struct. Emitted at the producer
    /// site, in the current block, so the alloca/stores/load dominate the use
    /// (no cross-merge production, §5.6). `target` is `null` for a non-capturing
    /// lambda / static method-ref thunk, the env for a capturing closure.
    fn build_func_value(&mut self, code: Value, target: Value) -> Value {
        let fid = self.structs.func_struct;
        let slot = self.fb.alloca(IrType::Struct(fid));
        let code_addr = self.fb.field_addr(slot.clone(), fid, 0);
        self.fb.store(code_addr, code);
        let target_addr = self.fb.field_addr(slot.clone(), fid, 1);
        self.fb.store(target_addr, target);
        self.fb.load(slot, IrType::Struct(fid))
    }

    /// Extract the `code` field (`Ptr`) of a `$Func` value-struct `v`. Spills the
    /// aggregate to a fresh alloca and reads field 0 — used by `f == null`, which
    /// is defined as `f.code == null` (§5.4).
    fn func_code_field(&mut self, v: Value) -> Value {
        let fid = self.structs.func_struct;
        let slot = self.fb.alloca(IrType::Struct(fid));
        self.fb.store(slot.clone(), v);
        let code_addr = self.fb.field_addr(slot, fid, 0);
        self.fb.load(code_addr, IrType::Ptr)
    }

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
        // FV-T3 (§5.4): a bare code `Ptr` (a non-capturing lambda address or a
        // static method-ref `$mref$` thunk) crossing into a `Func$`-typed slot /
        // param / return auto-wraps to `Func${code = v, target = null}`. A
        // `Const::Null` flows through the same arm (its IR type is `Ptr`) to
        // `Func${null, null}`, giving `function R(P) f = null;` a defined value.
        // There is deliberately NO `Func$ → Ptr` path (it would drop `target`).
        if from == IrType::Ptr && to == IrType::Struct(self.structs.func_struct) {
            return self.build_func_value(v, Value::Const(Const::Null));
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
        // A `ref`/`out` param is always a pointer to caller storage — even a
        // `ref function R(P)` stays a bare `Ptr`, never a `Func$`.
        IrType::Ptr
    } else {
        // FV-T3: a by-value `function R(P)` *parameter* is a closure-carrying
        // position, so it lowers to the `$Func` value-struct (delegate-gated in
        // `lower_value_ty`). This makes both the emitted callee param and the
        // recorded `MethodSig.params` (the call-site coercion target) `Func$`.
        lower_value_ty(&p.ty, src, structs, env)
    }
}

/// FV-T6b: the *inner* Beef function signature `(ret, ptys)` of a by-value
/// `function R(P)` parameter, lowered to IR types under `env` — `None` for any
/// other parameter (including a `ref`/`out` function pointer, a `delegate`, or a
/// non-function param). This is the per-param companion to [`param_ir_ty`]: where
/// `param_ir_ty` erases the signature down to a uniform `$Func` value-struct,
/// this preserves the `(ret, ptys)` so the call site can target-type an INLINE
/// lambda argument's params (`xs.Map<int32>(x => x*10)`) — the lambda's `x`
/// binds at `ptys[0]` even though no declared `function`-typed local supplied it.
/// Recorded parallel to `MethodSig.params` and read in `lower_program`'s emit
/// pass. Nested function types inside `P`/`R` lower to `$Func` (one layer).
fn param_fn_sig(
    p: &AstParam,
    src: &str,
    structs: &StructTable,
    env: TyEnv,
) -> Option<(IrType, Vec<IrType>)> {
    if is_by_ref(p) {
        return None;
    }
    match &p.ty {
        AstType::Function {
            return_ty,
            params: fps,
            is_delegate: false,
            ..
        } => {
            let ret = lower_value_ty(return_ty, src, structs, env);
            let ptys: Vec<IrType> = fps
                .iter()
                .map(|t| lower_value_ty(t, src, structs, env))
                .collect();
            Some((ret, ptys))
        }
        _ => None,
    }
}

/// FV-T7: the *inner* Beef function signature `(ret, ptys)` of a `function R(P)`
/// **return type**, lowered under `env` — `None` for any non-function (or
/// `delegate`) return type. The return-type companion to [`param_fn_sig`]: where
/// the method's `ret` IR type erases a `function R(P)` return to the uniform
/// `$Func` value-struct, this preserves `(ret, ptys)` so a lambda written
/// directly in a `return` position (`return x => x + n;`) can target-type its
/// untyped params from the declared return signature — exactly as a call-arg
/// inline lambda is target-typed from the resolved callee param sig (T6b). Used
/// to seed `Lowerer.ret_fn_sig`.
fn ret_fn_sig_of_ty(ty: &AstType, src: &str, structs: &StructTable, env: TyEnv) -> Option<(IrType, Vec<IrType>)> {
    match ty {
        AstType::Function {
            return_ty,
            params: fps,
            is_delegate: false,
            ..
        } => {
            let ret = lower_value_ty(return_ty, src, structs, env);
            let ptys: Vec<IrType> = fps
                .iter()
                .map(|t| lower_value_ty(t, src, structs, env))
                .collect();
            Some((ret, ptys))
        }
        _ => None,
    }
}

/// The monomorphized symbol name of a generic instantiation: `Box<int>` →
/// `Box$i64`, `Pair<int32>` → `Pair$i32` (reusing the overload type codes).
fn mangle_generic(name: &str, args: &[IrType]) -> String {
    format!("{name}${}", type_codes(args))
}

/// The owner-qualified symbol of a generic-*method* monomorph.
/// `Some(id)` → `"{prefixes[id]}{name}${codes}"` (e.g. `"Box.Get$i32"`),
/// `None`    → `"{name}${codes}"` (free / bare-cross-class static — identical to
/// the old name-only `mangle_generic` output). `prefixes[id]` already encodes
/// the owner's full path *and* its monomorph args (`"List$i64."`), and
/// `type_codes` never emits `.`, so the dot is a safe owner separator.
///
/// GM-A1 always passes `None`, so the produced symbol is byte-identical to
/// today's; later tasks pass `Some(owner)` to disambiguate same-named methods
/// in different types.
fn mangle_generic_method(
    owner: Option<StructId>,
    name: &str,
    args: &[IrType],
    t: &StructTable,
) -> String {
    let codes = type_codes(args);
    match owner {
        Some(id) => format!("{}{name}${codes}", t.prefixes[id.0 as usize]),
        None => format!("{name}${codes}"),
    }
}

/// The method name of a generic-call callee base — `Name` for a bare
/// `Name<Args>(…)` and `Type.Name` qualified call alike. Generic methods use
/// global-name mangling (no owner), so both resolve to the same monomorph.
fn generic_callee_name<'b>(base: &'b Expr, src: &'b str) -> Option<&'b str> {
    match base {
        Expr::Ident(s) => Some(s.text(src)),
        Expr::Member { name, .. } => Some(name.text(src)),
        _ => None,
    }
}

/// Like [`lower_ty_env`], but a `function R(P)` in a *closure-carrying* position
/// (a param, a local, or a return type) lowers to the two-word `$Func`
/// value-struct (`Struct(func_struct)`) rather than a bare code pointer, so the
/// function value's representation (`code` + `target`) travels with it under one
/// uniform calling convention. Fields, casts, and extern callback tables
/// (C-ABI function-pointer positions, e.g. `BfRtCallbacks`) must NOT use this —
/// they keep calling `lower_ty_env`, which lowers `AstType::Function` to bare
/// `Ptr`, preserving their layout.
///
/// **Delegate gating (doc §6):** only a `function R(P)` type is swept into
/// `Func$`. A `delegate R(P)` (parsed as the same `AstType::Function` node but
/// with `is_delegate == true`) is Beef's heap GC object and is **not** widened in
/// this slice — it stays a bare `Ptr` (T8 groundwork). Several corlib-slice files
/// (`Array.bf`, `Lazy.bf`, `Platform.bf`, …) have `delegate`-typed params/locals;
/// widening them would change layout and break the verify corpus.
fn lower_value_ty(ty: &AstType, src: &str, structs: &StructTable, env: TyEnv) -> IrType {
    if let AstType::Function {
        is_delegate: false, ..
    } = ty
    {
        return IrType::Struct(structs.func_struct);
    }
    lower_ty_env(ty, src, structs, env)
}

/// Lower a type, resolving generic type-parameters through `env` (so a `T`
/// field of a monomorphized `Box<int>` becomes `i64`) and generic
/// instantiations through the monomorphized symbol table (`Box<int>` → the
/// registered `Box$i64`). An `AstType::Function` (both `function` and
/// `delegate`) lowers to bare `Ptr` here; the closure-carrying `Func$` widening
/// lives only in [`lower_value_ty`].
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::SourceFile;
    use newbf_parser::parse_file;

    /// FV-T1: `$Func` is registered FIRST, so `func_struct == StructId(0)`, it is
    /// named `$Func`, and it has exactly two `Ptr` fields `code`/`target`. This
    /// pins the default-id hazard fix: `func_struct` defaults to `StructId(0)`,
    /// which must genuinely be the `$Func` struct.
    #[test]
    fn func_struct_is_id_zero_with_two_ptr_fields() {
        // `StructTable::build` does not load the corlib prelude itself
        // (`lower_program` does), so an empty file list registers only `$Func`.
        let t = StructTable::build(&[]);
        assert_eq!(t.func_struct, StructId(0), "$Func must be StructId(0)");
        let fd = &t.defs[t.func_struct.0 as usize];
        assert_eq!(fd.name, "$Func");
        assert_eq!(fd.fields.len(), 2, "$Func must have exactly two fields");
        assert_eq!(fd.fields[0].name, "code");
        assert_eq!(fd.fields[0].ty, IrType::Ptr);
        assert_eq!(fd.fields[1].name, "target");
        assert_eq!(fd.fields[1].ty, IrType::Ptr);
        // The well-known name resolves back to the same id.
        assert_eq!(t.by_name.get("$Func").copied(), Some(StructId(0)));
        // It is a value struct.
        assert!(matches!(t.kinds[0], StructKind::Value));
    }

    /// FV-T1 / Risk R5 (C-ABI layout regression): registering `$Func` at id 0 and
    /// adding the (unused) `lower_value_ty` helper must NOT widen C-ABI
    /// function-pointer *fields* to the 16-byte `$Func` struct. `lower_ty_env`'s
    /// `AstType::Function` arm stays bare `Ptr`, so every function-pointer field
    /// of a `BfRtCallbacks`-style table keeps its 8-byte `Ptr` layout. (This
    /// mirrors the real `BfRtCallbacks` in beef-tests/corlib-slice/Runtime.bf,
    /// which is exercised in full by the verify corpus.)
    #[test]
    fn cabi_function_pointer_fields_stay_bare_ptr() {
        let src = r#"
            struct BfRtCallbacksLike {
                function void* (int x) mAlloc;
                function void (void* p) mFree;
                function int (int a, int b) mCombine;
                int32 mTag;
            }
        "#;
        let unit = parse_file(src, FileId(0)).0;
        let files = [SourceFile {
            file: FileId(0),
            src,
            unit: &unit,
        }];
        let t = StructTable::build(&files);
        let id = *t
            .by_name
            .get("BfRtCallbacksLike")
            .expect("test struct must register");
        let def = &t.defs[id.0 as usize];
        // Every `function`-typed field stays a bare `Ptr` (not `Struct(func_struct)`):
        // C-ABI layout unchanged. The trailing `int32` confirms ordinary fields
        // are unaffected.
        let func_fields: Vec<_> = def.fields.iter().filter(|f| f.name != "mTag").collect();
        assert_eq!(func_fields.len(), 3, "three function-pointer fields");
        for f in &func_fields {
            assert_eq!(
                f.ty,
                IrType::Ptr,
                "function-pointer field {} must stay bare Ptr (C-ABI), not $Func",
                f.name
            );
            assert_ne!(
                f.ty,
                IrType::Struct(t.func_struct),
                "field {} must NOT be widened to the $Func value-struct",
                f.name
            );
        }
        let tag = def.fields.iter().find(|f| f.name == "mTag").unwrap();
        assert_eq!(tag.ty, IrType::I32);
    }

    /// FV-T1: the position-gated helper itself. `lower_value_ty` returns the
    /// `$Func` struct for a `function` type, but `lower_ty_env` (used by
    /// fields/casts/externs) still returns bare `Ptr` for the same type.
    #[test]
    fn lower_value_ty_yields_func_struct_only_at_value_positions() {
        let src = "function int (int x) F;";
        // Parse a struct whose single field is a function type, then pull the
        // field's `AstType::Function` back out for the helper comparison.
        let wrapper = "struct W { function int (int x) f; }";
        let unit = parse_file(wrapper, FileId(0)).0;
        let files = [SourceFile {
            file: FileId(0),
            src: wrapper,
            unit: &unit,
        }];
        let t = StructTable::build(&files);
        // Find the `function`-typed field's AST node.
        let fn_ty = unit
            .items
            .iter()
            .find_map(|it| match it {
                Item::Type(td) => td.members.iter().find_map(|m| match m {
                    Member::Field { ty, .. } if matches!(ty, AstType::Function { .. }) => Some(ty),
                    _ => None,
                }),
                _ => None,
            })
            .expect("function-typed field");
        // Value position ⇒ `$Func`; type position (field/cast/extern) ⇒ bare `Ptr`.
        assert_eq!(
            lower_value_ty(fn_ty, wrapper, &t, &[]),
            IrType::Struct(t.func_struct)
        );
        assert_eq!(lower_ty_env(fn_ty, wrapper, &t, &[]), IrType::Ptr);
        let _ = src; // documents the bare type-decl form
    }

    /// FV-T3+T4 / Risk R1 (verify-clean ABI drift): the LLVM backend builds an
    /// indirect call's type from its *args*, so an arity/ABI mismatch through a
    /// function value passes the verifier yet miscompiles. This walks the lowered
    /// `Module.funcs` and asserts the uniform callee ABI invariants the
    /// run-corpus alone otherwise guards:
    ///   - every `$lambda*` and `$mref*` has `param[0].ty == Ptr` (the `$self`),
    ///   - `$Func` is a 2-`Ptr`-field value struct,
    ///   - every `Func$` indirect call (`$mref$` thunk callee) is arity-clean.
    #[test]
    fn lowered_function_values_have_uniform_self_leading_abi() {
        // A program exercising all three producers: a non-capturing lambda, a
        // capturing closure (env), and a static method-ref (thunked), each into a
        // `function`-typed local, plus a generic HOF call through the value.
        let src = r#"
            class Mathx { public static int32 Square(int32 x) { return x * x; } }
            class Program {
                public static int32 Main() {
                    int32 b = 10;
                    function int32(int32) plain = x => x + 1;
                    function int32(int32) capt  = a => a + b;
                    function int32(int32) sref  = Mathx.Square;
                    return plain(1) + capt(2) + sref(3);
                }
            }
        "#;
        let (unit, _pd) = parse_file(src, FileId(0));
        let files = [SourceFile {
            file: FileId(0),
            src,
            unit: &unit,
        }];
        let program = crate::analyze(&files);
        let module = lower_program(&files, &program);

        // `$Func` is a 2-`Ptr`-field value struct.
        let func_def = module
            .structs
            .iter()
            .find(|s| s.name == "$Func")
            .expect("$Func struct present in module");
        assert_eq!(func_def.fields.len(), 2, "$Func has exactly two fields");
        assert!(
            func_def.fields.iter().all(|f| f.ty == IrType::Ptr),
            "$Func fields must both be Ptr"
        );

        // Every emitted lambda/method-ref-thunk leads with a `Ptr` `$self`.
        let mut saw_lambda = false;
        let mut saw_mref = false;
        for f in &module.funcs {
            if f.name.starts_with("$lambda") {
                saw_lambda = true;
                assert!(
                    !f.params.is_empty() && f.params[0].ty == IrType::Ptr,
                    "{}: param[0] must be the Ptr $self",
                    f.name
                );
            }
            if f.name.starts_with("$mref") {
                saw_mref = true;
                assert!(
                    !f.params.is_empty() && f.params[0].ty == IrType::Ptr,
                    "{}: param[0] must be the Ptr $self",
                    f.name
                );
            }
        }
        assert!(saw_lambda, "expected at least one $lambda* function");
        assert!(saw_mref, "expected the $mref$ thunk for Mathx.Square");
    }

    /// FV-T6 (inline lambda in call-arg position): an inline lambda written
    /// directly as a generic-HOF arg (`xs.Map<int32>(x => x + 1)`) with NO
    /// declared `function`-typed local must still (a) be COLLECTED a `$lambdaN`
    /// symbol (T6a's pre-pass walk of call args) and emitted as a free function —
    /// NOT lowered to `undef` — and (b) bind its body param at the RESOLVED
    /// callee param type (T6b: `function int32(int32)`'s `int32`), so the emitted
    /// `$lambdaN($self: Ptr, x: i32)` is `$self`-leading with `x : I32`. The
    /// run-corpus (`lambda_direct_arg.bf`) gates the behavior; this pins the ABI.
    #[test]
    fn fv_t6_inline_arg_lambda_emits_with_targeted_params() {
        let src = r#"
            class Program {
                public static int32 Main() {
                    List<int32> xs = new List<int32>();
                    xs.Add(1);
                    // INLINE lambda — no `function`-typed local supplies its type.
                    List<int32> ys = xs.Map<int32>(x => x + 1);
                    return ys.Fold<int32>(0, (acc, x) => acc + x);
                }
            }
        "#;
        let (unit, _pd) = parse_file(src, FileId(0));
        let files = [SourceFile {
            file: FileId(0),
            src,
            unit: &unit,
        }];
        let program = crate::analyze(&files);
        let module = lower_program(&files, &program);

        // The single-param inline `Map` lambda must be emitted as a `$lambdaN`
        // function whose param[0] is the Ptr `$self` and param[1] is the
        // target-typed `x : i32` — proving T6a collected it and T6b typed it.
        let one_param_lambda = module
            .funcs
            .iter()
            .filter(|f| f.name.starts_with("$lambda"))
            .find(|f| f.params.len() == 2)
            .expect("an inline 1-param ($self + x) lambda was emitted (not undef)");
        assert_eq!(
            one_param_lambda.params[0].ty,
            IrType::Ptr,
            "{}: param[0] must be the Ptr $self",
            one_param_lambda.name
        );
        assert_eq!(
            one_param_lambda.params[1].ty,
            IrType::I32,
            "{}: inline lambda's `x` must be target-typed to the callee param (i32)",
            one_param_lambda.name
        );
    }

    /// IT-T2 data-shape gate (itables.md §8): `fill_iface_members` /
    /// `collect_iface_bases` populate the interface tables correctly.
    ///   - `imethods[IShape]` lists exactly its one instance slot `("Area", _)`;
    ///   - a class implementing `IShape` has `IShape` in `iface_bases`;
    ///   - a VALUE STRUCT listing an interface base has EMPTY `iface_bases`
    ///     (boxing out of scope — it gets no itable slots);
    ///   - `interface IB : IA` yields `imethods[IB]` starting with IA's methods,
    ///     and a class implementing IB has IA flattened into its `iface_bases`;
    ///   - `static` and generic interface methods do NOT consume a slot.
    #[test]
    fn it_t2_iface_tables_shape() {
        let src = r#"
            interface IA { int32 Ay(); }
            interface IShape : IA {
                int32 Area();
                static int32 Sides();
                T Cast<T>();
            }
            interface IB : IA { int32 Bee(); }
            class Square : IShape {
                public int32 Ay() { return 1; }
                public int32 Area() { return 9; }
            }
            class Both : IB {
                public int32 Ay() { return 2; }
                public int32 Bee() { return 3; }
            }
            struct V : IShape {
                public int32 Ay() { return 4; }
                public int32 Area() { return 5; }
            }
        "#;
        let unit = parse_file(src, FileId(0)).0;
        let files = [SourceFile {
            file: FileId(0),
            src,
            unit: &unit,
        }];
        let t = StructTable::build(&files);
        let id = |n: &str| *t.by_name.get(n).unwrap_or_else(|| panic!("{n} must register"));
        let ia = id("IA");
        let ishape = id("IShape");
        let ib = id("IB");
        let square = id("Square");
        let both = id("Both");
        let v = id("V");

        // Interfaces register as `Interface`-kind, classes as `Ref`, value
        // struct as `Value`.
        assert!(matches!(t.kinds[ishape.0 as usize], StructKind::Interface));
        assert!(matches!(t.kinds[square.0 as usize], StructKind::Ref));
        assert!(matches!(t.kinds[v.0 as usize], StructKind::Value));

        // `imethods[IShape]` = base-first: IA's "Ay" then own "Area". The
        // `static Sides()` and generic `Cast<T>()` are filtered out (no slot).
        let ishape_m: Vec<&str> = t.imethods[ishape.0 as usize]
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        assert_eq!(
            ishape_m,
            vec!["Ay", "Area"],
            "IShape slots: base IA.Ay first, then own Area; static/generic excluded"
        );
        // The slot sig is this-leading on the interface id, returning int32.
        let area_sig = &t.imethods[ishape.0 as usize]
            .iter()
            .find(|(n, _)| n == "Area")
            .unwrap()
            .1;
        assert_eq!(area_sig.params.first().copied(), Some(IrType::Ref(ishape)));
        assert!(area_sig.is_instance);
        assert_eq!(area_sig.ret, IrType::I32);

        // `imethods[IA]` is just its own "Ay" slot.
        let ia_m: Vec<&str> = t.imethods[ia.0 as usize]
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        assert_eq!(ia_m, vec!["Ay"]);

        // `imethods[IB]` (IB : IA) starts with IA's methods, then its own.
        let ib_m: Vec<&str> = t.imethods[ib.0 as usize]
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        assert_eq!(ib_m, vec!["Ay", "Bee"], "IB lists base IA.Ay first");

        // All these interface methods are abstract → `idefaults` all `None`.
        assert!(
            t.idefaults[ishape.0 as usize].iter().all(|d| d.is_none()),
            "abstract interface methods have no default symbol"
        );
        assert_eq!(
            t.idefaults[ishape.0 as usize].len(),
            t.imethods[ishape.0 as usize].len(),
            "idefaults parallels imethods"
        );

        // A class implementing IShape has IShape (and its base IA) flattened in.
        assert!(
            t.iface_bases[square.0 as usize].contains(&ishape),
            "Square.iface_bases must contain IShape"
        );
        assert!(
            t.iface_bases[square.0 as usize].contains(&ia),
            "IShape : IA ⇒ IA flattened into Square.iface_bases"
        );
        // Base-first ordering: IA before IShape.
        let sq_pos_ia = t.iface_bases[square.0 as usize]
            .iter()
            .position(|x| *x == ia);
        let sq_pos_ishape = t.iface_bases[square.0 as usize]
            .iter()
            .position(|x| *x == ishape);
        assert!(sq_pos_ia < sq_pos_ishape, "IA flattened before IShape");

        // A class implementing IB (IB : IA) has IA flattened into iface_bases.
        assert!(
            t.iface_bases[both.0 as usize].contains(&ib)
                && t.iface_bases[both.0 as usize].contains(&ia),
            "Both.iface_bases must contain IB and (flattened) IA"
        );

        // A VALUE STRUCT listing an interface base has EMPTY iface_bases.
        assert!(
            t.iface_bases[v.0 as usize].is_empty(),
            "value struct V gets no itable iface_bases (boxing out of scope)"
        );
    }

    /// IT-T2: a default (bodied) interface method is recorded in `imethods` with
    /// a non-`None` `idefaults` symbol, an abstract one with `None`; and defaults
    /// must NOT leak into `methods[iface]` (a class calling a default it overrides
    /// would otherwise resolve to a wrong direct call). Explicit interface impls
    /// land in `explicit_impls`.
    #[test]
    fn it_t2_defaults_and_explicit_impls() {
        let src = r#"
            interface IGreet {
                int32 Abstract();
                int32 Default() { return 100; }
            }
            class C : IGreet {
                public int32 Abstract() { return 1; }
                int32 IGreet.Explicit() { return 7; }
            }
        "#;
        let unit = parse_file(src, FileId(0)).0;
        let files = [SourceFile {
            file: FileId(0),
            src,
            unit: &unit,
        }];
        let t = StructTable::build(&files);
        let igreet = *t.by_name.get("IGreet").expect("IGreet registers");
        let c = *t.by_name.get("C").expect("C registers");

        // Both methods take a slot; abstract → None, default → Some(symbol).
        let names: Vec<&str> = t.imethods[igreet.0 as usize]
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        assert!(names.contains(&"Abstract") && names.contains(&"Default"));
        for (k, (n, _)) in t.imethods[igreet.0 as usize].iter().enumerate() {
            let df = &t.idefaults[igreet.0 as usize][k];
            if n == "Abstract" {
                assert!(df.is_none(), "abstract method has no default symbol");
            } else if n == "Default" {
                assert_eq!(
                    df.as_deref(),
                    Some("IGreet.Default"),
                    "default method records its {{prefix}}{{name}} symbol"
                );
            }
        }

        // Defaults must NOT be in `methods[iface]` (kept empty for interfaces).
        assert!(
            t.methods[igreet.0 as usize].is_empty(),
            "interface methods (incl. defaults) must not enter methods[iface]"
        );

        // The explicit impl `IGreet.Explicit` is recorded for (C, IGreet, name).
        assert!(
            t.explicit_impls
                .contains_key(&(c, igreet, "Explicit".to_string())),
            "explicit interface impl recorded in explicit_impls"
        );
    }

    /// IT-T3 composition gate (itables.md §8): `apply_itables` lays each
    /// implemented interface's methods into the class vtable at the global
    /// per-interface slot base.
    ///   - `Square` (no `virtual` of its own) gets a NON-EMPTY `vimpls` with its
    ///     `Area` impl symbol at `iface_slot_base[IShape]` (so a vtable global is
    ///     emitted for an otherwise-vtableless interface-only class);
    ///   - `iface_slot_base[I] >= N` (the global class-vtable max) — the bounds
    ///     keystone that keeps no interface block overlapping a class block;
    ///   - `class C : Base, IFace` whose `IFace.M` is satisfied PURELY by an
    ///     inherited `Base.M` resolves the slot to Base's symbol;
    ///   - each implementer's `vimpls` is grown to cover its highest used slot.
    #[test]
    fn it_t3_compose_itables() {
        let src = r#"
            interface IShape { int32 Area(); }
            class Square : IShape { public int32 Area() { return 9; } }

            interface IFace { int32 M(); }
            class Base { public int32 M() { return 5; } }
            class Derived : Base, IFace { }

            // A class with REAL virtual slots, so the global class-vtable max N is
            // non-zero — forcing every interface block strictly beyond it.
            class Animal { public virtual int32 Speak() { return 1; } }
        "#;
        let unit = parse_file(src, FileId(0)).0;
        let files = [SourceFile {
            file: FileId(0),
            src,
            unit: &unit,
        }];
        let t = StructTable::build(&files);
        let id = |n: &str| *t.by_name.get(n).unwrap_or_else(|| panic!("{n} must register"));
        let ishape = id("IShape");
        let square = id("Square");
        let iface = id("IFace");
        let derived = id("Derived");

        // The global class-vtable max N is the longest CLASS VIRTUAL block. After
        // `apply_itables` grows `vimpls` with appended interface slots, the
        // pre-itable class block length is recoverable as `vslots[c].len()`
        // (`apply_itables` only ever grows `vimpls`, never `vslots`). Every
        // interface slot base must sit at or beyond this N (the bounds keystone).
        let n = t.vslots.iter().map(|m| m.len()).max().unwrap_or(0);
        assert!(n >= 1, "Animal contributes at least one virtual slot, so N >= 1");
        for (&i, &b) in &t.iface_slot_base {
            assert!(
                b >= n,
                "iface_slot_base[{}] = {b} must be >= N = {n} (class-block bound)",
                i.0
            );
        }

        // Square has NO virtual of its own, yet `apply_itables` gives it a
        // non-empty vimpls — so `emit_vtables` emits a `Square$vtable` global.
        let sq_base = t.iface_slot_base[&ishape];
        assert!(
            !t.vimpls[square.0 as usize].is_empty(),
            "interface-only class Square gets a non-empty vimpls (vtable global emitted)"
        );
        // The `Area` slot is the first (only) `imethods[IShape]` entry, at base+0.
        let area_idx = t.imethods[ishape.0 as usize]
            .iter()
            .position(|(n, _)| n == "Area")
            .expect("IShape has an Area slot");
        assert_eq!(
            t.vimpls[square.0 as usize][sq_base + area_idx],
            "Square.Area",
            "Square.Area impl symbol sits at iface_slot_base[IShape] + Area index"
        );
        // The vimpls is grown to cover that slot (and any null-padded gap below it).
        assert!(
            t.vimpls[square.0 as usize].len() > sq_base + area_idx,
            "Square.vimpls grown to cover its highest used iface slot"
        );

        // `Derived : Base, IFace` — IFace.M is satisfied by INHERITED Base.M
        // (apply_inheritance ran first), so the slot resolves to Base's symbol.
        let if_base = t.iface_slot_base[&iface];
        let m_idx = t.imethods[iface.0 as usize]
            .iter()
            .position(|(n, _)| n == "M")
            .expect("IFace has an M slot");
        // The symbol Base.M lowered to (inherited into Derived's methods table).
        let base_m_sym = t.methods[derived.0 as usize]
            .get("M")
            .and_then(|c| c.first())
            .map(|s| s.full_name.clone())
            .expect("Derived inherits Base.M into its methods table");
        assert_eq!(base_m_sym, "Base.M", "inherited method keeps Base's symbol");
        assert_eq!(
            t.vimpls[derived.0 as usize][if_base + m_idx],
            base_m_sym,
            "Derived's IFace.M slot resolves to the inherited Base.M symbol"
        );

        // No interface block overlaps a class virtual block: every used iface
        // slot index is >= the class's own virtual-slot count (`vslots.len()`),
        // so an interface impl never overwrites a class virtual slot.
        for &cls in &[square, derived] {
            let class_block = t.vslots[cls.0 as usize].len();
            for ifid in &t.iface_bases[cls.0 as usize] {
                assert!(
                    t.iface_slot_base[ifid] >= class_block,
                    "iface block for class {} sits beyond its virtual block",
                    cls.0
                );
            }
        }
    }

    /// TA-1: `arg_is_pending` classifies exactly the target-typed dot-forms whose
    /// `IrType` is unknown without a target. The pending shapes (`.X`, `.{ … }`,
    /// `.(args)`, `.Case(args)`) are `true`; ordinary exprs, `ref x`, a bare
    /// `Expr::Tuple`, and a *qualified* `Enum.Case(args)` are `false`. The check is
    /// purely syntactic, so the `Expr`s are built directly (no parser dependency).
    #[test]
    fn arg_is_pending_classifies_dot_forms() {
        let f = FileId(0);
        // A dummy span; `arg_is_pending` is structural and never reads the text.
        let sp = Span::new(f, 0, 0);
        let dot = || Box::new(Expr::DotIdent { span: sp, name: sp });

        // `.X` — bare dot-case → pending.
        let bare_case = Expr::DotIdent { span: sp, name: sp };
        // `.{ … }` — DotIdent-based initializer → pending.
        let dot_init = Expr::Initializer {
            span: sp,
            base: dot(),
            entries: vec![],
        };
        // `.(args)` — ctor shorthand, callee is a `DotIdent` → pending.
        let dot_ctor = Expr::Call {
            span: sp,
            callee: dot(),
            args: vec![],
        };
        // `.Some(40)` — case call, callee is a `DotIdent` → pending (covers the
        // ambiguous `.Case(payload)` the param type is needed to resolve).
        let dot_case_call = Expr::Call {
            span: sp,
            callee: dot(),
            args: vec![Expr::Int(sp)],
        };

        assert!(arg_is_pending(&bare_case, ""), ".X is pending");
        assert!(arg_is_pending(&dot_init, ""), ".{{ … }} is pending");
        assert!(arg_is_pending(&dot_ctor, ""), ".(args) is pending");
        assert!(arg_is_pending(&dot_case_call, ""), ".Some(40) is pending");

        // An ordinary expression → concrete.
        let ordinary = Expr::Ident(sp);
        // `ref x` — a `Prefix`; never wraps a pending form (no lvalue) → concrete.
        let ref_x = Expr::Prefix {
            span: sp,
            kw: PrefixKw::Ref,
            qualifier: None,
            operand: Box::new(Expr::Ident(sp)),
        };
        // A bare tuple `(a, b)` → concrete for the first slice (§3.6).
        let tuple = Expr::Tuple {
            span: sp,
            elems: vec![Expr::Int(sp), Expr::Int(sp)],
        };
        // A *qualified* `Enum.Case(40)` — callee is a `Member`, not a `DotIdent`;
        // it has a concrete base and resolves without a target → concrete.
        let qualified_enum = Expr::Call {
            span: sp,
            callee: Box::new(Expr::Member {
                span: sp,
                base: Box::new(Expr::Ident(sp)),
                name: sp,
                conditional: false,
            }),
            args: vec![Expr::Int(sp)],
        };

        assert!(!arg_is_pending(&ordinary, ""), "an Ident is concrete");
        assert!(!arg_is_pending(&ref_x, ""), "ref x is concrete");
        assert!(!arg_is_pending(&tuple, ""), "a bare tuple is concrete");
        assert!(
            !arg_is_pending(&qualified_enum, ""),
            "a qualified Enum.Case(args) is concrete"
        );
    }

    // ---- TA-2: shape-gated partial resolution (`pick_overload_partial`) --------

    /// A static `MethodSig` with explicit param types `params` (no `this`),
    /// returning `void`. The symbol doubles as the candidate's identity in the
    /// assertions below.
    fn sig(name: &str, params: &[IrType]) -> MethodSig {
        MethodSig {
            full_name: name.to_string(),
            ret: IrType::Void,
            params: params.to_vec(),
            is_instance: false,
            variadic: None,
            param_fn_sigs: Vec::new(),
        }
    }

    /// (a) `pick_overload` (now a thin wrapper over `pick_overload_partial` with
    /// all-`Concrete` shapes) preserves the old behavior on a representative
    /// spread: exact-type preference, same-category vs unrelated, arity gating,
    /// and the variadic penalty/tie-break. For each case the wrapper's pick must
    /// equal a direct `pick_overload_partial` call with the same shapes, proving
    /// the delegation is identity on concrete args.
    #[test]
    fn pick_overload_wrapper_matches_partial_on_concrete() {
        let st = StructTable::default();
        let concrete = |tys: &[IrType]| -> Vec<ArgShape> {
            tys.iter().map(|t| ArgShape::Concrete(*t)).collect()
        };
        // Exact width beats same-category: an i32 arg prefers the i32 overload.
        let cands = vec![sig("M_i64", &[IrType::I64]), sig("M_i32", &[IrType::I32])];
        let tys = [IrType::I32];
        let picked = pick_overload(&cands, &tys, false).map(|s| s.full_name.as_str());
        assert_eq!(picked, Some("M_i32"), "exact-width overload wins");
        assert_eq!(
            picked,
            pick_overload_partial(&cands, &concrete(&tys), false, &st)
                .map(|s| s.full_name.as_str()),
            "wrapper == partial (exact width)",
        );

        // Same-category (int↔int) beats unrelated (a pointer-ish Ref): an int arg
        // routes to the int overload, not the reference one.
        let cands = vec![sig("M_ref", &[IrType::Ref(StructId(9))]), sig("M_int", &[IrType::I64])];
        let tys = [IrType::I32];
        let picked = pick_overload(&cands, &tys, false).map(|s| s.full_name.as_str());
        assert_eq!(picked, Some("M_int"), "same-category int overload wins");
        assert_eq!(
            picked,
            pick_overload_partial(&cands, &concrete(&tys), false, &st)
                .map(|s| s.full_name.as_str()),
            "wrapper == partial (category)",
        );

        // Arity: a 2-param candidate is ineligible for a 1-arg call.
        let cands = vec![sig("M2", &[IrType::I64, IrType::I64]), sig("M1", &[IrType::I64])];
        let tys = [IrType::I64];
        assert_eq!(
            pick_overload(&cands, &tys, false).map(|s| s.full_name.as_str()),
            Some("M1"),
            "arity selects the 1-param overload",
        );

        // Variadic penalty: an exact non-variadic overload beats a variadic one
        // for the same arg count (tie broken by the variadic's flat penalty).
        let mut variadic = sig("M_var", &[IrType::I64]);
        variadic.variadic = Some(IrType::I64);
        let cands = vec![variadic, sig("M_exact", &[IrType::I64])];
        let tys = [IrType::I64];
        let picked = pick_overload(&cands, &tys, false).map(|s| s.full_name.as_str());
        assert_eq!(picked, Some("M_exact"), "exact overload beats variadic on a tie");
        assert_eq!(
            picked,
            pick_overload_partial(&cands, &concrete(&tys), false, &st)
                .map(|s| s.full_name.as_str()),
            "wrapper == partial (variadic penalty)",
        );

        // Instance candidates are ineligible at a non-member site (`members=false`).
        let mut inst = sig("M_inst", &[IrType::Ref(StructId(1)), IrType::I64]);
        inst.is_instance = true;
        let cands = vec![inst, sig("M_static", &[IrType::I64])];
        let tys = [IrType::I64];
        assert_eq!(
            pick_overload(&cands, &tys, false).map(|s| s.full_name.as_str()),
            Some("M_static"),
            "instance candidate skipped at a this-less site",
        );
    }

    /// (b) A pending `.(args)` (ctor) slot picks the struct-typed overload and
    /// DISQUALIFIES a primitive-typed candidate. `.(…)` is compatible only with a
    /// value `Struct(_)` param; an `int` param incompatible → that candidate is
    /// removed, so the only survivor is the struct overload.
    #[test]
    fn pending_ctor_picks_struct_disqualifies_primitive() {
        let st = StructTable::default();
        let vec2 = IrType::Struct(StructId(7));
        // Registered primitive-first, so a *non*-disqualifying resolver would wrongly
        // keep it; the shape gate must drop it and choose the struct overload.
        let cands = vec![sig("M_int", &[IrType::I64]), sig("M_vec2", &[vec2])];
        let shapes = [ArgShape::Pending(PendingKind::Ctor)];
        assert_eq!(
            pick_overload_partial(&cands, &shapes, false, &st).map(|s| s.full_name.as_str()),
            Some("M_vec2"),
            ".(args) routes to the struct param, never the primitive",
        );

        // With *only* a primitive candidate, the pending `.(…)` disqualifies it and
        // resolution fails (None) — better than silently picking a primitive slot.
        let only_int = vec![sig("M_int", &[IrType::I64])];
        assert_eq!(
            pick_overload_partial(&only_int, &shapes, false, &st).map(|s| s.full_name.as_str()),
            None,
            "a lone primitive candidate is disqualified by a .(…) pending slot",
        );
    }

    /// (c) The wrong-pick guard (correctness blocker #2): `M(Vec2,int)` vs
    /// `M(Vec3,int)` called as `M(.(…), 5)`, plus a decoy `M(int,int)`. The pending
    /// `.(…)` in slot 0 disqualifies `M(int,int)` (primitive first param), so
    /// resolution can NEVER back-fill the construction against a primitive. Between
    /// the two struct candidates the shape gate ties (both `Struct`), so the
    /// first-registered wins — but either way the slot-0 param is a `Struct`, so a
    /// `.(…)` arg is never miscompiled into a primitive slot.
    #[test]
    fn pending_ctor_wrong_pick_guard_backfills_a_struct() {
        let st = StructTable::default();
        let vec2 = IrType::Struct(StructId(3));
        let vec3 = IrType::Struct(StructId(4));
        // Decoy primitive-first candidate registered FIRST (the dangerous case: a
        // loose gate would keep it and miscompile the `.(…)` into an int slot).
        let cands = vec![
            sig("M_int_int", &[IrType::I64, IrType::I64]),
            sig("M_vec2_int", &[vec2, IrType::I64]),
            sig("M_vec3_int", &[vec3, IrType::I64]),
        ];
        let shapes = [
            ArgShape::Pending(PendingKind::Ctor),
            ArgShape::Concrete(IrType::I64),
        ];
        let picked = pick_overload_partial(&cands, &shapes, false, &st)
            .expect("a struct candidate must resolve");
        assert_ne!(
            picked.full_name, "M_int_int",
            "the .(…) slot must NOT resolve to the primitive-first candidate",
        );
        // The slot-0 param of the pick is a struct (never a primitive) — the
        // anti-miscompile invariant.
        assert!(
            matches!(picked.params[0], IrType::Struct(_)),
            "the pending .(…) back-fills against a Struct param, not a primitive",
        );
        // First-registered struct candidate wins the all-struct tie.
        assert_eq!(picked.full_name, "M_vec2_int", "first-registered struct tie");
    }

    /// (d) A pending `.Case` slot is compatible only with the payload enum whose
    /// case set contains the named case. `OptA` owns `Some`; `EitherB` owns `Left`
    /// (not `Some`). `.Some` resolves to the `OptA` param, disqualifying the
    /// `EitherB` overload (case absent) and the `int` overload (not an enum struct).
    #[test]
    fn pending_enum_case_gates_on_case_set() {
        let mut st = StructTable::default();
        let opt_a = StructId(11);
        let either_b = StructId(12);
        // Minimal payload-enum case tables: only the case *names* matter to the gate.
        st.enum_cases.insert(
            opt_a,
            vec![
                ("Some".to_string(), 0, vec![IrType::I32]),
                ("None".to_string(), 1, vec![]),
            ],
        );
        st.enum_cases.insert(
            either_b,
            vec![
                ("Left".to_string(), 0, vec![IrType::I32]),
                ("Right".to_string(), 1, vec![IrType::I32]),
            ],
        );
        let cands = vec![
            sig("M_int", &[IrType::I64]),
            sig("M_either", &[IrType::Struct(either_b)]),
            sig("M_opt", &[IrType::Struct(opt_a)]),
        ];
        let some = [ArgShape::Pending(PendingKind::EnumCase("Some"))];
        assert_eq!(
            pick_overload_partial(&cands, &some, false, &st).map(|s| s.full_name.as_str()),
            Some("M_opt"),
            ".Some routes to the enum whose case set contains Some",
        );
        // A case owned by NEITHER enum disqualifies both enum candidates and the
        // int one → no resolution (never a silent wrong pick).
        let missing = [ArgShape::Pending(PendingKind::EnumCase("Nope"))];
        assert_eq!(
            pick_overload_partial(&cands, &missing, false, &st).map(|s| s.full_name.as_str()),
            None,
            "a case owned by no candidate enum disqualifies every candidate",
        );
    }

    /// `pending_kind` classifies the three pending shapes (the companion to
    /// `arg_is_pending`): `.(args)` → `Ctor`, `.{ … }` → `Initializer`, bare
    /// `.Case` and `.Case(payload)` → `EnumCase(name)`; a concrete expr → `None`.
    /// Pinned because the shape gate keys off these kinds.
    #[test]
    fn pending_kind_classifies_each_shape() {
        let src = ". Vec2 Some";
        let f = FileId(0);
        // Spans into `src`: "." at 0..1, "Vec2" at 2..6 (unused), "Some" at 7..11.
        let dot_name = Span::new(f, 0, 1);
        let some_name = Span::new(f, 7, 11);
        let sp = Span::new(f, 0, 0);

        // `.(args)` — callee DotIdent named "."
        let dot_ctor = Expr::Call {
            span: sp,
            callee: Box::new(Expr::DotIdent { span: sp, name: dot_name }),
            args: vec![],
        };
        assert_eq!(pending_kind(&dot_ctor, src), Some(PendingKind::Ctor));

        // `.{ … }` — DotIdent-based initializer
        let dot_init = Expr::Initializer {
            span: sp,
            base: Box::new(Expr::DotIdent { span: sp, name: dot_name }),
            entries: vec![],
        };
        assert_eq!(pending_kind(&dot_init, src), Some(PendingKind::Initializer));

        // `.Some(payload)` — callee DotIdent named a case
        let dot_case_call = Expr::Call {
            span: sp,
            callee: Box::new(Expr::DotIdent { span: sp, name: some_name }),
            args: vec![Expr::Int(sp)],
        };
        assert_eq!(
            pending_kind(&dot_case_call, src),
            Some(PendingKind::EnumCase("Some")),
        );

        // bare `.Some`
        let bare = Expr::DotIdent { span: sp, name: some_name };
        assert_eq!(pending_kind(&bare, src), Some(PendingKind::EnumCase("Some")));

        // A concrete expression → None (stays in lockstep with arg_is_pending).
        assert_eq!(pending_kind(&Expr::Ident(sp), src), None);
    }
}
