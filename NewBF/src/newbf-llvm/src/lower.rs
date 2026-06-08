//! IR → LLVM IR lowering (Sprint 07).
//!
//! Takes a [`newbf_ir::Module`] (the typed SSA IR produced by
//! `newbf-sema::lower_program`) and emits an `inkwell` module ready for the
//! `dump-llvm` report, the LLVM verifier, and — later in the sprint — the
//! ORC JIT / AOT object emission.
//!
//! The mapping is deliberately mechanical because the IR was designed close
//! to LLVM (MANIFESTO core decision 3): opaque pointers, sized ints/floats,
//! explicit signed/unsigned op selection. Two passes:
//!
//!   1. **Declare** every module function (definitions + externs) so by-name
//!      calls resolve regardless of order. Calls to names not in the module
//!      (FFI / not-yet-lowered) are declared lazily from their argument types.
//!   2. **Lower** each body: pre-create all blocks (so branches and phi
//!      back-edges can target them), lower instructions recording SSA results,
//!      then wire phi incomings in a final pass.
//!
//! Lowering is **total**: an operand that cannot be resolved (an instruction
//! the kernel skipped) degrades to a typed `undef`/`poison` rather than
//! panicking, so the corpus no-panic gate holds even on partial IR. The LLVM
//! verifier is the correctness backstop for well-formed input.

use std::collections::HashMap;

use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module as LlvmModule;
use inkwell::types::{
    BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FloatType, FunctionType, IntType, StructType,
};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValue, BasicValueEnum, FloatValue, FunctionValue, IntValue,
    PhiValue, PointerValue,
};
use inkwell::{AddressSpace, FloatPredicate, IntPredicate};
use newbf_ir::{
    BinOp, BlockId, CastKind, CmpPred, Const, Function as IrFunction, InstKind, IrType,
    Module as IrModule, Param, ReflectPolicy, Terminator, TypeMeta, Value,
};

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

/// Lower an IR module into an `inkwell` module owned by `ctx`. Both must
/// outlive any JIT engine or object emission built from the result.
pub fn emit_module<'ctx>(ctx: &'ctx Context, ir: &IrModule) -> LlvmModule<'ctx> {
    let module = ctx.create_module(&ir.name);
    let builder = ctx.create_builder();
    // RF-T2: the canonical `%ClassVData = { i32 mType, [0 x ptr] }` GEP type.
    let ptr_ty = ctx.ptr_type(AddressSpace::default());
    let classvdata_ty = ctx.opaque_struct_type("ClassVData");
    classvdata_ty.set_body(
        &[
            ctx.i32_type().into(),
            ptr_ty.array_type(0).into(),
        ],
        false,
    );
    // RF-T4: a `TargetData` for concrete struct sizes (the Type global's
    // `mSize` / FieldInfo offsets — const i32s that can't be a `size_of()`
    // const-expr). Built from the host target's data layout when the native
    // target is registered (JIT/AOT init it); a standard 64-bit fallback
    // otherwise (e.g. a `lower_to_string` call before any target init), so
    // size computation never panics.
    let target_data = host_data_layout();
    let mut cg = Codegen {
        ctx,
        module: &module,
        builder: &builder,
        struct_types: Vec::new(),
        classvdata_ty,
        target_data,
    };
    cg.build_struct_types(ir);
    cg.declare_all(ir);
    cg.emit_classvdata(ir);
    cg.emit_metadata(ir);
    cg.emit_alloc_sites(ir);
    cg.emit_globals(ir);
    for f in &ir.funcs {
        if !f.is_extern {
            cg.lower_function(f);
        }
    }
    module
}

/// Lower `ir` for the **AOT** path (MS-T3b): the same module [`emit_module`]
/// produces, plus a `.CRT$XCU` static-initializer **guard bootstrap** that runs
/// the runtime guard's startup sequence BEFORE the program's `main`
/// (memory-safety.md §A7 "the AOT entry stub").
///
/// The JIT path keeps using [`emit_module`] (no bootstrap): each JIT host calls
/// `install_crash_handler` / `set_guard_mode` / `register_alloc_sites_from_jit`
/// itself in Rust (see `guard_runner` / the run-corpus harness). In AOT there is
/// no Rust host around the program, so the bootstrap must live *in the module*.
///
/// **Why a `.CRT$XCU` global ctor and NOT `/ENTRY:newbf_entry`** (the load-bearing
/// CRT-init decision): swapping `/ENTRY` would REPLACE `mainCRTStartup` and skip
/// CRT initialization, leaving the linked Rust runtime's `std` (HashMap, atomics,
/// stderr, VirtualAlloc via extern) running on an uninitialized CRT. Instead we
/// KEEP `/ENTRY:mainCRTStartup` (full CRT init) and register the bootstrap in
/// `@llvm.global_ctors`, which LLVM lowers to a `.CRT$XCU` entry — a C/C++-style
/// global constructor the CRT runs, after CRT init, BEFORE the codegen `main`.
pub fn emit_module_aot<'ctx>(ctx: &'ctx Context, ir: &IrModule) -> LlvmModule<'ctx> {
    let module = emit_module(ctx, ir);
    let builder = ctx.create_builder();
    let target_data = host_data_layout();
    let cg = Codegen {
        ctx,
        module: &module,
        builder: &builder,
        struct_types: Vec::new(),
        classvdata_ty: module
            .get_struct_type("ClassVData")
            .unwrap_or_else(|| ctx.opaque_struct_type("ClassVData")),
        target_data,
    };
    cg.emit_aot_bootstrap(ir);
    module
}

/// RF-T4: the host's `TargetData` (for concrete struct sizes), falling back to a
/// standard x86-64 layout when no native target is registered. The two agree on
/// scalar/pointer layout (the only shapes reflectable structs use), so `mSize`
/// is correct in both the JIT/AOT path (target registered) and the
/// `lower_to_string`/`verify_module` test paths (often not).
fn host_data_layout() -> inkwell::targets::TargetData {
    use inkwell::targets::{InitializationConfig, Target, TargetMachine};
    use inkwell::OptimizationLevel;
    if Target::initialize_native(&InitializationConfig::default()).is_ok() {
        let triple = TargetMachine::get_default_triple();
        if let Ok(target) = Target::from_triple(&triple)
            && let Some(tm) = target.create_target_machine(
                &triple,
                "generic",
                "",
                OptimizationLevel::Default,
                inkwell::targets::RelocMode::Default,
                inkwell::targets::CodeModel::Default,
            )
        {
            return tm.get_target_data();
        }
    }
    inkwell::targets::TargetData::create(
        "e-m:w-p270:32:32-p271:32:32-p272:64:64-i64:64-f80:128-n8:16:32:64-S128",
    )
}

/// Lower an IR module and render it as LLVM IR text — the `dump-llvm` report.
pub fn lower_to_string(ir: &IrModule) -> String {
    let ctx = Context::create();
    let module = emit_module(&ctx, ir);
    module.print_to_string().to_string()
}

/// Lower an IR module and run LLVM's verifier; `Err` carries the verifier's
/// message. Used by tests and the corpus gate.
pub fn verify_module(ir: &IrModule) -> Result<(), String> {
    let ctx = Context::create();
    let module = emit_module(&ctx, ir);
    module.verify().map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Internal state
// ---------------------------------------------------------------------------

struct Codegen<'ctx, 'a> {
    ctx: &'ctx Context,
    module: &'a LlvmModule<'ctx>,
    builder: &'a Builder<'ctx>,
    /// LLVM struct types indexed by `StructId.0`, built up front so any field
    /// can reference any struct (incl. forward/self refs).
    struct_types: Vec<StructType<'ctx>>,
    /// RF-T2: the canonical `%ClassVData = { i32 mType, [0 x ptr] }` struct
    /// type used to GEP an object's `$header`. The `[0 x ptr]` array makes
    /// field 1 (the vtable base) sit at offset 8 (i32 + 4 pad → 8-aligned
    /// array) — the SAME offset as any concrete `{ i32, [N x ptr] }`, since the
    /// pointer array's alignment forces identical padding regardless of N. So a
    /// struct-GEP through this one type reaches the vtable base of every class
    /// (LLVM computes the padded offset; no hand-rolled byte offset). `mType`
    /// (field 0) is read as i32 for `LoadTypeId`.
    classvdata_ty: StructType<'ctx>,
    /// RF-T4: the host data layout, for concrete struct sizes (`%struct.Type`'s
    /// `mSize` is a const i32 — `get_abi_size(struct_type)` truncated to i32).
    target_data: inkwell::targets::TargetData,
}

impl<'ctx> Codegen<'ctx, '_> {
    // ── type mapping ──────────────────────────────────────────────────────

    /// Build the LLVM struct type for every IR struct, up front. Two passes:
    /// opaque shells (so any field can reference any struct, incl. forward and
    /// self), then set each body now that all shells exist.
    fn build_struct_types(&mut self, ir: &IrModule) {
        for i in 0..ir.structs.len() {
            let shell = self.ctx.opaque_struct_type(&format!("s{i}"));
            self.struct_types.push(shell);
        }
        for (i, s) in ir.structs.iter().enumerate() {
            let st = self.struct_types[i];
            let fields: Vec<BasicTypeEnum<'ctx>> =
                s.fields.iter().map(|f| self.basic_type_of(f.ty)).collect();
            st.set_body(&fields, false);
        }
    }

    fn basic_type_of(&self, ty: IrType) -> BasicTypeEnum<'ctx> {
        match ty {
            // `void` is not a BasicType; this only fires defensively (a void
            // value/param should never reach here).
            IrType::Void => self.ctx.i8_type().into(),
            IrType::Bool => self.ctx.bool_type().into(),
            IrType::Int { .. } => self.int_type_of(ty).into(),
            IrType::Float { .. } => self.float_type_of(ty).into(),
            // `Ref` is a typed reference but lowers to the same opaque `ptr`.
            IrType::Ptr | IrType::Ref(_) => self.ctx.ptr_type(AddressSpace::default()).into(),
            IrType::Struct(id) => self.struct_types[id.0 as usize].into(),
        }
    }

    fn int_type_of(&self, ty: IrType) -> IntType<'ctx> {
        let bits = match ty {
            IrType::Bool => return self.ctx.bool_type(),
            IrType::Int { bits, .. } => bits,
            _ => 64,
        };
        match bits {
            1 => self.ctx.bool_type(),
            8 => self.ctx.i8_type(),
            16 => self.ctx.i16_type(),
            32 => self.ctx.i32_type(),
            64 => self.ctx.i64_type(),
            128 => self.ctx.i128_type(),
            other => {
                let nz = std::num::NonZeroU32::new(u32::from(other.max(1))).unwrap();
                self.ctx
                    .custom_width_int_type(nz)
                    .unwrap_or_else(|_| self.ctx.i64_type())
            }
        }
    }

    fn float_type_of(&self, ty: IrType) -> FloatType<'ctx> {
        match ty {
            IrType::Float { bits: 16 } => self.ctx.f16_type(),
            IrType::Float { bits: 32 } => self.ctx.f32_type(),
            IrType::Float { bits: 128 } => self.ctx.f128_type(),
            _ => self.ctx.f64_type(),
        }
    }

    fn fn_type(&self, params: &[Param], ret: IrType) -> FunctionType<'ctx> {
        let ptys: Vec<BasicMetadataTypeEnum<'ctx>> = params
            .iter()
            .map(|p| self.basic_type_of(p.ty).into())
            .collect();
        if ret == IrType::Void {
            self.ctx.void_type().fn_type(&ptys, false)
        } else {
            self.basic_type_of(ret).fn_type(&ptys, false)
        }
    }

    // ── declarations ──────────────────────────────────────────────────────

    fn declare_all(&self, ir: &IrModule) {
        for f in &ir.funcs {
            if self.module.get_function(&f.name).is_none() {
                let fty = self.fn_type(&f.params, f.ret);
                let fv = self.module.add_function(&f.name, fty, None);
                // Defined functions carry async (`2`) unwind tables so LLVM
                // emits `.pdata`/`.xdata` — the JIT memory manager registers
                // these with `RtlAddFunctionTable` so exceptions unwind
                // through JIT'd frames (MANIFESTO core decision 16).
                if !f.is_extern {
                    let kind = inkwell::attributes::Attribute::get_named_enum_kind_id("uwtable");
                    let attr = self.ctx.create_enum_attribute(kind, 2);
                    fv.add_attribute(inkwell::attributes::AttributeLoc::Function, attr);
                }
            }
        }
    }

    /// Look up a callee, declaring it on demand when absent. Module functions
    /// (definitions + externs) are pre-declared with their real signatures by
    /// [`Self::declare_all`]; only **unresolved** direct calls — bare names the
    /// kernel can't yet resolve to a method (resolution lands with the type
    /// sprint) — reach the lazy path. These are declared *variadic with no
    /// fixed parameters* (`<ret> @name(...)`) so every call site, whatever its
    /// arity or argument types, type-checks against the one declaration. Bare
    /// call names never collide with real definitions, which are all prefixed
    /// (`Type.Method`).
    fn get_or_declare(&self, name: &str, ret: IrType) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function(name) {
            return f;
        }
        let fty = if ret == IrType::Void {
            self.ctx.void_type().fn_type(&[], true)
        } else {
            self.basic_type_of(ret).fn_type(&[], true)
        };
        self.module.add_function(name, fty, None)
    }

    /// RF-T2: emit each class's ClassVData as a constant
    /// `%ClassVData.<T> = { i32 mType, [N x ptr] vtbl }` global — the single
    /// canonical per-class header object (replacing the bare `[N x ptr]` vtable
    /// array). `mType` is `VtableDef.type_id` (the dense reflection type-id; 0
    /// at RF-T2 — RF-T3 fills real ids). The `[N x ptr]` slots resolve from the
    /// already-declared functions (a missing entry — an abstract/null slot —
    /// becomes null). `new` stores `&<global>` into every object's `$header`;
    /// virtual/interface dispatch reaches the vtable via a struct-GEP into
    /// field 1 (`load_vtable_base` → `VtableBase`), so the `{i32, pad}` prefix
    /// never causes a physical slot-shift. Every `StructKind::Ref` id has one
    /// (entries empty ⇒ `{ i32, [0 x ptr] }`).
    fn emit_classvdata(&self, ir: &IrModule) {
        let ptr_ty = self.ctx.ptr_type(AddressSpace::default());
        let i32_ty = self.ctx.i32_type();
        for vt in &ir.vtables {
            let entries: Vec<PointerValue<'ctx>> = vt
                .entries
                .iter()
                .map(|name| {
                    self.module
                        .get_function(name)
                        .map(|f| f.as_global_value().as_pointer_value())
                        .unwrap_or_else(|| ptr_ty.const_null())
                })
                .collect();
            let arr_ty = ptr_ty.array_type(entries.len() as u32);
            let arr = ptr_ty.const_array(&entries);
            // The concrete per-class struct type `{ i32, [N x ptr] }`. Field 1
            // sits at offset 8 for any N (i32 + 4 pad → 8-aligned array), so it
            // is GEP-compatible with the canonical `%ClassVData` ({ i32, [0 x
            // ptr] }) `VtableBase`/`LoadTypeId` index through.
            let cvdata_ty = self
                .ctx
                .struct_type(&[i32_ty.into(), arr_ty.into()], false);
            let mtype = i32_ty.const_int(vt.type_id as u64, false);
            let init = cvdata_ty.const_named_struct(&[mtype.into(), arr.into()]);
            let g = self.module.add_global(cvdata_ty, None, &vt.name);
            g.set_initializer(&init);
            g.set_constant(true);
        }
    }

    /// RF-T4: the per-type `%struct.Type` constant global's symbol — mirrors
    /// sema's `type_global_name` (`"Dog."` → `"Dog.$type"`). The sema↔llvm
    /// contract: sema emits a `GlobalAddr` of this exact name from `typeof(T)`;
    /// here we DEFINE it. (newbf-sema ⊥ newbf-llvm — agree only via the symbol.)
    fn type_global_name(prefix: &str) -> String {
        format!("{prefix}$type")
    }

    /// RF-T4: emit a private, NUL-terminated `[N x i8]` string constant and
    /// return a `ptr` to its first byte (a `char8*`). Same shape as `Const::Str`
    /// lowering, but reusable for metadata name strings.
    fn emit_cstr(&self, s: &str) -> PointerValue<'ctx> {
        let i8t = self.ctx.i8_type();
        let bytes: Vec<_> = s
            .bytes()
            .chain(std::iter::once(0u8))
            .map(|b| i8t.const_int(u64::from(b), false))
            .collect();
        let arr = i8t.const_array(&bytes);
        let g = self.module.add_global(arr.get_type(), None, ".str.meta");
        g.set_initializer(&arr);
        g.set_constant(true);
        g.set_linkage(inkwell::module::Linkage::Private);
        g.as_pointer_value()
    }

    /// RF-T4: the reflection metadata pass — modeled on `emit_classvdata`. From
    /// `ir.type_meta` it emits, all into the SAME module the JIT/AOT compiles:
    ///   1. The named `%struct.Type` / `%struct.FieldInfo` / `%struct.MethodInfo`
    ///      aggregate types (ABI-pinned to corlib `Type.bf` — reflection.md §4.2).
    ///   2. Per `TypeMeta`: a name string, the (policy-gated) `[k x %FieldInfo]` /
    ///      `[m x %MethodInfo]` arrays, then the `%struct.Type` constant global
    ///      (`type_global_name(prefix)`), with `mSize` from the DataLayout and
    ///      `mFields`/`mMethods` NULL when the policy strips them (the strip
    ///      differential — an unmarked type emits no FieldInfo array at all).
    ///   3. The dense registry: `@__newbf_type_table` (`[COUNT x ptr]` by type-id),
    ///      `@__newbf_type_count`, and the never-null `@__newbf_type_unknown`
    ///      sentinel.
    ///   4. The IN-MODULE `@__newbf_type_by_id(i32) -> ptr` accessor (a bounds-
    ///      checked index into the table) — NO Rust runtime symbol, so it resolves
    ///      in JIT AND AOT with zero linking work (the design's verified fix,
    ///      reflection.md §3/§4.4).
    ///
    /// A program with no reflectable types still emits a `[0 x ptr]` table +
    /// count 0 + the sentinel + the accessor (so `typeof(non-class)` and the
    /// accessor always resolve), but pays nothing per-type.
    fn emit_metadata(&self, ir: &IrModule) {
        let i32_ty = self.ctx.i32_type();
        let ptr_ty = self.ctx.ptr_type(AddressSpace::default());

        // 1. The metadata aggregate types. `%struct.Type` field order MUST match
        //    corlib `Type.bf` (the layout unit test pins this):
        //      { mSize, mTypeId, mFlags, mFieldCount, mMethodCount,
        //        mName(ptr), mFields(ptr), mMethods(ptr) }
        let type_ty = self.ctx.opaque_struct_type("struct.Type");
        type_ty.set_body(
            &[
                i32_ty.into(), // mSize
                i32_ty.into(), // mTypeId
                i32_ty.into(), // mFlags
                i32_ty.into(), // mFieldCount
                i32_ty.into(), // mMethodCount
                ptr_ty.into(), // mName : char8*
                ptr_ty.into(), // mFields : %FieldInfo*
                ptr_ty.into(), // mMethods : %MethodInfo*
            ],
            false,
        );
        // %struct.FieldInfo  = { name(char8*), offset(i32), typeId(i32) }
        let field_ty = self.ctx.opaque_struct_type("struct.FieldInfo");
        field_ty.set_body(&[ptr_ty.into(), i32_ty.into(), i32_ty.into()], false);
        // %struct.MethodInfo = { name(char8*), symbol(char8*), paramCount(i32) }
        let method_ty = self.ctx.opaque_struct_type("struct.MethodInfo");
        method_ty.set_body(&[ptr_ty.into(), ptr_ty.into(), i32_ty.into()], false);

        // A dense type-id → struct-id map so a FieldInfo's `typeId` can name the
        // field type's own reflection id (0 when the field type isn't reflected).
        let mut dense_struct_to_typeid: HashMap<u32, u32> = HashMap::new();
        for tm in &ir.type_meta {
            dense_struct_to_typeid.insert(tm.struct_id.0, tm.type_id);
        }

        // 2. Per type: name + (gated) field/method arrays + the Type global.
        //    Sort by dense type-id so the table below is built in id order.
        let mut metas: Vec<&TypeMeta> = ir.type_meta.iter().collect();
        metas.sort_by_key(|t| t.type_id);

        // The Type global pointers in dense-id order (for the registry table).
        let mut table_entries: Vec<PointerValue<'ctx>> = Vec::with_capacity(metas.len());

        for tm in &metas {
            let name_ptr = self.emit_cstr(&tm.name);

            // The object instance size (the backend `get_size`), as an i32. The
            // struct id is into `ir.structs`, built 1:1 in `struct_types`.
            let size = self
                .struct_types
                .get(tm.struct_id.0 as usize)
                .map(|st| self.target_data.get_abi_size(st) as u32)
                .unwrap_or(0);

            // Policy-gated FieldInfo array. Emitted ONLY when policy.has(FIELDS);
            // else `mFields` is null + count 0 (the strip differential).
            let (fields_ptr, field_count) = if tm.policy.has(ReflectPolicy::FIELDS)
                && !tm.fields.is_empty()
            {
                let infos: Vec<_> = tm
                    .fields
                    .iter()
                    .map(|f| {
                        let fname = self.emit_cstr(&f.name);
                        // Field byte offset within the object body (from the
                        // DataLayout); 0 if the struct type is somehow absent.
                        let offset = self
                            .struct_types
                            .get(tm.struct_id.0 as usize)
                            .and_then(|st| {
                                self.target_data.offset_of_element(st, f.field_index)
                            })
                            .unwrap_or(0) as u32;
                        // The field type's own reflection type-id (0 if not a
                        // reflected struct/class type).
                        let tyid = match f.ty {
                            IrType::Struct(id) | IrType::Ref(id) => {
                                dense_struct_to_typeid.get(&id.0).copied().unwrap_or(0)
                            }
                            _ => 0,
                        };
                        field_ty.const_named_struct(&[
                            fname.into(),
                            i32_ty.const_int(u64::from(offset), false).into(),
                            i32_ty.const_int(u64::from(tyid), false).into(),
                        ])
                    })
                    .collect();
                let arr = field_ty.const_array(&infos);
                let g = self.module.add_global(arr.get_type(), None, ".fieldinfo");
                g.set_initializer(&arr);
                g.set_constant(true);
                g.set_linkage(inkwell::module::Linkage::Private);
                (g.as_pointer_value(), infos.len() as u32)
            } else {
                (ptr_ty.const_null(), 0)
            };

            // Policy-gated MethodInfo array (RF-T7 consumes it; emitted here so
            // the strip policy is symmetric and the count is observable).
            let (methods_ptr, method_count) = if tm.policy.has(ReflectPolicy::METHODS)
                && !tm.methods.is_empty()
            {
                let infos: Vec<_> = tm
                    .methods
                    .iter()
                    .map(|m| {
                        let mname = self.emit_cstr(&m.name);
                        let msym = self.emit_cstr(&m.symbol);
                        method_ty.const_named_struct(&[
                            mname.into(),
                            msym.into(),
                            i32_ty.const_int(u64::from(m.param_count), false).into(),
                        ])
                    })
                    .collect();
                let arr = method_ty.const_array(&infos);
                let g = self.module.add_global(arr.get_type(), None, ".methodinfo");
                g.set_initializer(&arr);
                g.set_constant(true);
                g.set_linkage(inkwell::module::Linkage::Private);
                (g.as_pointer_value(), infos.len() as u32)
            } else {
                (ptr_ty.const_null(), 0)
            };

            // mFlags: bit0 = is_ref (class). (Reserved for richer flags later.)
            let flags = u64::from(tm.is_ref);
            let init = type_ty.const_named_struct(&[
                i32_ty.const_int(u64::from(size), false).into(),
                i32_ty.const_int(u64::from(tm.type_id), false).into(),
                i32_ty.const_int(flags, false).into(),
                i32_ty.const_int(u64::from(field_count), false).into(),
                i32_ty.const_int(u64::from(method_count), false).into(),
                name_ptr.into(),
                fields_ptr.into(),
                methods_ptr.into(),
            ]);
            // The global name is keyed by the struct PREFIX (== sema's
            // `type_global_name`); recover it from the ClassVData name we emitted
            // (`"{prefix}$cvdata"`), since `TypeMeta` carries only the simple name.
            let prefix = ir
                .vtables
                .iter()
                .find(|v| v.type_id == tm.type_id)
                .and_then(|v| v.name.strip_suffix("$cvdata"))
                .map(str::to_string)
                .unwrap_or_else(|| format!("{}.", tm.name));
            let gname = Self::type_global_name(&prefix);
            let g = self.module.add_global(type_ty, None, &gname);
            g.set_initializer(&init);
            g.set_constant(true);
            table_entries.push(g.as_pointer_value());
        }

        // 3. The never-null sentinel Type (id -1, size 0, name "?"), then the
        //    dense registry table + count.
        let unknown_name = self.emit_cstr("?");
        let unknown_init = type_ty.const_named_struct(&[
            i32_ty.const_zero().into(),                         // mSize
            i32_ty.const_int(u64::from(u32::MAX), false).into(), // mTypeId = -1
            i32_ty.const_zero().into(),                         // mFlags
            i32_ty.const_zero().into(),                         // mFieldCount
            i32_ty.const_zero().into(),                         // mMethodCount
            unknown_name.into(),
            ptr_ty.const_null().into(),
            ptr_ty.const_null().into(),
        ]);
        let unknown = self
            .module
            .add_global(type_ty, None, "__newbf_type_unknown");
        unknown.set_initializer(&unknown_init);
        unknown.set_constant(true);

        let count = table_entries.len() as u32;
        let table_arr = ptr_ty.const_array(&table_entries);
        let table = self
            .module
            .add_global(table_arr.get_type(), None, "__newbf_type_table");
        table.set_initializer(&table_arr);
        table.set_constant(true);

        let count_g = self
            .module
            .add_global(i32_ty, None, "__newbf_type_count");
        count_g.set_initializer(&i32_ty.const_int(u64::from(count), false));
        count_g.set_constant(true);

        // 4. The in-module accessor: `ptr @__newbf_type_by_id(i32 %id)`. A
        //    bounds-checked (unsigned, so negatives are rejected) index into the
        //    table, returning the sentinel out of range — never null. Built with
        //    the inkwell builder; resolves in JIT and AOT with no external symbol.
        let fn_ty = ptr_ty.fn_type(&[i32_ty.into()], false);
        // Reuse an existing declaration if one was already declared (e.g. an IR
        // `extern` or a forward call from `declare_all`) so we DEFINE that symbol
        // rather than emit a name-mangled duplicate; otherwise add it fresh. In
        // the normal pipeline `emit_metadata` runs before `lower_function`, so a
        // sema-emitted call (RF-T5) resolves to this definition.
        let func = self
            .module
            .get_function("__newbf_type_by_id")
            .filter(|f| f.count_basic_blocks() == 0)
            .unwrap_or_else(|| self.module.add_function("__newbf_type_by_id", fn_ty, None));
        let entry = self.ctx.append_basic_block(func, "entry");
        let hit = self.ctx.append_basic_block(func, "hit");
        let miss = self.ctx.append_basic_block(func, "miss");
        self.builder.position_at_end(entry);
        let id = func.get_nth_param(0).unwrap().into_int_value();
        let cnt = i32_ty.const_int(u64::from(count), false);
        let ok = self
            .builder
            .build_int_compare(IntPredicate::ULT, id, cnt, "ok")
            .unwrap();
        self.builder.build_conditional_branch(ok, hit, miss).unwrap();

        self.builder.position_at_end(hit);
        // GEP `[COUNT x ptr] @__newbf_type_table, i32 0, i32 %id`, then load.
        let table_ty = ptr_ty.array_type(count);
        let zero = i32_ty.const_zero();
        // SAFETY: a 2-index GEP into the table global; `id` is bounds-checked
        // above (the `hit` block is only reached when `id < count`).
        let slot = unsafe {
            self.builder
                .build_in_bounds_gep(table_ty, table.as_pointer_value(), &[zero, id], "slot")
                .unwrap()
        };
        let t = self.builder.build_load(ptr_ty, slot, "t").unwrap();
        self.builder.build_return(Some(&t)).unwrap();

        self.builder.position_at_end(miss);
        self.builder
            .build_return(Some(&unknown.as_pointer_value()))
            .unwrap();
    }

    /// MS-T7: emit the heap-allocation **site table** (`Module::alloc_sites`)
    /// the runtime guard registers to resolve a `site_id` to `"<function> @
    /// file:line"` in a UAF / double-free / leak report (memory-safety.md §A7).
    ///
    /// Emits two globals whose layout MUST match `newbf_runtime::guard`'s reader
    /// (`AllocSiteRaw`):
    ///   * `@__newbf_alloc_sites` — a constant `[N x %struct.AllocSite]`, where
    ///     `%struct.AllocSite = { ptr function, ptr file, i32 line }`. The `i`-th
    ///     entry is the site whose `site_id` is `i` (the third `newbf_alloc` arg).
    ///   * `@__newbf_alloc_sites_count` — a constant `i32` = `N`.
    /// The function/file strings are private NUL-terminated `char8*` constants
    /// (`emit_cstr`), exactly like the reflection name strings.
    ///
    /// **Release omits the table** (memory-safety.md §A7): gated on
    /// `debug_assertions`, so a release build of the compiler emits nothing
    /// (zero bloat). The IR the allocations carry is byte-identical either way —
    /// only the (debug-only) lookup table differs; an unresolvable `site_id` in
    /// release just falls back to the bare address in any report.
    ///
    /// An allocation-free program (`alloc_sites` empty) emits a `[0 x ...]` table
    /// + count `0` so the registration symbols always resolve (the host's
    /// `lookup("__newbf_alloc_sites")` is a clean `None`/empty either way).
    #[cfg(debug_assertions)]
    fn emit_alloc_sites(&self, ir: &IrModule) {
        let i32_ty = self.ctx.i32_type();
        let ptr_ty = self.ctx.ptr_type(AddressSpace::default());

        // %struct.AllocSite = { function(char8*), file(char8*), line(i32) }.
        // Field order + types are the runtime `AllocSiteRaw` reader contract.
        let site_ty = self.ctx.opaque_struct_type("struct.AllocSite");
        site_ty.set_body(&[ptr_ty.into(), ptr_ty.into(), i32_ty.into()], false);

        let mut entries = Vec::with_capacity(ir.alloc_sites.len());
        for s in &ir.alloc_sites {
            let func_ptr = self.emit_cstr(&s.function);
            let file_ptr = self.emit_cstr(&s.file);
            entries.push(site_ty.const_named_struct(&[
                func_ptr.into(),
                file_ptr.into(),
                i32_ty.const_int(u64::from(s.line), false).into(),
            ]));
        }

        let count = entries.len() as u32;
        let table_arr = site_ty.const_array(&entries);
        let table = self
            .module
            .add_global(table_arr.get_type(), None, "__newbf_alloc_sites");
        table.set_initializer(&table_arr);
        table.set_constant(true);

        let count_g = self
            .module
            .add_global(i32_ty, None, "__newbf_alloc_sites_count");
        count_g.set_initializer(&i32_ty.const_int(u64::from(count), false));
        count_g.set_constant(true);
    }

    /// Release build: omit the site table entirely (memory-safety.md §A7 — "release
    /// omits the table"). A no-op; an unresolvable `site_id` in a report falls back
    /// to the bare address.
    #[cfg(not(debug_assertions))]
    fn emit_alloc_sites(&self, _ir: &IrModule) {}

    /// MS-T3b: emit the AOT **guard bootstrap** — a `.CRT$XCU` static initializer
    /// that arms the runtime memory guard BEFORE the program's `main` runs
    /// (memory-safety.md §A5 / §A7). It calls, in order:
    ///
    ///   1. `newbf_install_crash_handler()` — arm the SEH crash dump so a UAF
    ///      faults into a dump (not a silent death) and the guard's double-free
    ///      `abort()` is diagnosed.
    ///   2. `newbf_set_guard_mode(MODE)` — `1` (Stomp, the debug guard) when the
    ///      **compiler** is a debug build, `0` (Thunk, plain malloc/free) in a
    ///      release compiler. This matches the per-profile policy of §A5 and the
    ///      debug-only `__newbf_alloc_sites` table emission above: a debug
    ///      toolchain ships the live guard; a release toolchain ships the strip.
    ///   3. `newbf_register_alloc_sites(&__newbf_alloc_sites, count)` — wire the
    ///      module's site table (emitted by `emit_alloc_sites`, debug only) so a
    ///      fault / double-free / leak report names `<function> @ file:line`
    ///      (MS-T7's AOT half). Skipped in a release compiler (no table emitted).
    ///
    /// The three are runtime C-ABI symbols supplied by the linked `newbf-runtime`
    /// **staticlib** (`aot.rs::link_executable`). The bootstrap is registered via
    /// `@llvm.global_ctors`, which LLVM lowers to a `.CRT$XCU` pointer the CRT runs
    /// during startup — so `/ENTRY:mainCRTStartup` (and thus full CRT init) is
    /// preserved.
    fn emit_aot_bootstrap(&self, ir: &IrModule) {
        let void_ty = self.ctx.void_type();
        let i32_ty = self.ctx.i32_type();
        let i64_ty = self.ctx.i64_type();
        let ptr_ty = self.ctx.ptr_type(AddressSpace::default());

        // Runtime C-ABI declarations (resolved from the linked staticlib).
        let install = self.module.get_function("newbf_install_crash_handler").unwrap_or_else(|| {
            self.module.add_function(
                "newbf_install_crash_handler",
                void_ty.fn_type(&[], false),
                None,
            )
        });
        let set_mode = self.module.get_function("newbf_set_guard_mode").unwrap_or_else(|| {
            self.module.add_function(
                "newbf_set_guard_mode",
                void_ty.fn_type(&[i32_ty.into()], false),
                None,
            )
        });

        // The ctor body: install + set mode (+ register the site table in debug).
        let ctor_ty = void_ty.fn_type(&[], false);
        let ctor = self.module.add_function("__newbf_aot_bootstrap", ctor_ty, None);
        let entry = self.ctx.append_basic_block(ctor, "entry");
        self.builder.position_at_end(entry);

        self.builder.build_call(install, &[], "").ok();

        // Mode per the *compiler's* build profile: debug → Stomp(1), release →
        // Thunk(0). Decoupled from the runtime crate's own profile (the runtime is
        // mode-agnostic; the flag is set here).
        let mode_val: u64 = if cfg!(debug_assertions) { 1 } else { 0 };
        self.builder
            .build_call(set_mode, &[i32_ty.const_int(mode_val, false).into()], "")
            .ok();

        // MS-T7 AOT half: register the emitted `__newbf_alloc_sites` table so a
        // report names the site. The table + count globals exist only in a debug
        // compiler (emit_alloc_sites is `#[cfg(debug_assertions)]`); reference them
        // by name to avoid coupling to the emission order.
        if cfg!(debug_assertions)
            && let (Some(table_g), Some(_count_g)) = (
                self.module.get_global("__newbf_alloc_sites"),
                self.module.get_global("__newbf_alloc_sites_count"),
            )
        {
            let register = self
                .module
                .get_function("newbf_register_alloc_sites")
                .unwrap_or_else(|| {
                    self.module.add_function(
                        "newbf_register_alloc_sites",
                        void_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false),
                        None,
                    )
                });
            let table_ptr = table_g.as_pointer_value();
            let count = i64_ty.const_int(ir.alloc_sites.len() as u64, false);
            self.builder
                .build_call(register, &[table_ptr.into(), count.into()], "")
                .ok();
        }

        self.builder.build_return(None).ok();

        // Register the ctor in `@llvm.global_ctors` so the CRT runs it (via
        // `.CRT$XCU`) before `main`. The element type is the canonical
        // `{ i32 priority, ptr ctor, ptr data }`; priority 65535 (default), null
        // associated-data.
        let ctor_entry_ty = self
            .ctx
            .struct_type(&[i32_ty.into(), ptr_ty.into(), ptr_ty.into()], false);
        let entry_val = ctor_entry_ty.const_named_struct(&[
            i32_ty.const_int(65535, false).into(),
            ctor.as_global_value().as_pointer_value().into(),
            ptr_ty.const_null().into(),
        ]);
        let arr = ctor_entry_ty.const_array(&[entry_val]);
        let global_ctors = self
            .module
            .add_global(arr.get_type(), None, "llvm.global_ctors");
        global_ctors.set_initializer(&arr);
        global_ctors.set_linkage(inkwell::module::Linkage::Appending);
    }

    /// Emit each `static` field as a mutable, zero-initialized module global.
    /// Only scalar globals (int/float/bool/ptr/ref) are emitted; an aggregate
    /// (`Struct`) or `Void` is skipped so a real-corpus static of such a type
    /// can never break emission (it simply has no backing global, matching the
    /// sema-side narrowing). A scalar `BasicTypeEnum::const_zero()` is the
    /// initializer; the global is left non-constant (mutable).
    fn emit_globals(&self, ir: &IrModule) {
        for g in &ir.globals {
            if matches!(g.ty, IrType::Void | IrType::Struct(_)) {
                continue;
            }
            let ty = self.basic_type_of(g.ty);
            let global = self.module.add_global(ty, None, &g.name);
            global.set_initializer(&ty.const_zero());
        }
    }

    // ── constants & operands ──────────────────────────────────────────────

    fn const_value(&self, c: &Const) -> BasicValueEnum<'ctx> {
        match c {
            Const::Int(v, ty) => {
                if *ty == IrType::Bool {
                    self.ctx
                        .bool_type()
                        .const_int(u64::from(*v != 0), false)
                        .into()
                } else {
                    self.int_type_of(*ty)
                        .const_int(*v as u64, ty.is_signed())
                        .into()
                }
            }
            Const::Float(v, ty) => self.float_type_of(*ty).const_float(*v).into(),
            Const::Bool(b) => self.ctx.bool_type().const_int(u64::from(*b), false).into(),
            Const::Null => self
                .ctx
                .ptr_type(AddressSpace::default())
                .const_null()
                .into(),
            Const::Undef(ty) => self.undef_of(*ty),
            Const::Str(s) => {
                // A private, NUL-terminated `[N x i8]` constant; the value is
                // a `ptr` to its first byte (a C `char*`).
                let i8t = self.ctx.i8_type();
                let bytes: Vec<_> = s
                    .bytes()
                    .chain(std::iter::once(0u8))
                    .map(|b| i8t.const_int(u64::from(b), false))
                    .collect();
                let arr = i8t.const_array(&bytes);
                let g = self.module.add_global(arr.get_type(), None, ".str");
                g.set_initializer(&arr);
                g.set_constant(true);
                g.set_linkage(inkwell::module::Linkage::Private);
                g.as_pointer_value().into()
            }
        }
    }

    fn undef_of(&self, ty: IrType) -> BasicValueEnum<'ctx> {
        match ty {
            IrType::Void => self.ctx.i8_type().get_undef().into(),
            IrType::Bool => self.ctx.bool_type().get_undef().into(),
            IrType::Int { .. } => self.int_type_of(ty).get_undef().into(),
            IrType::Float { .. } => self.float_type_of(ty).get_undef().into(),
            IrType::Ptr | IrType::Ref(_) => self
                .ctx
                .ptr_type(AddressSpace::default())
                .get_undef()
                .into(),
            IrType::Struct(id) => self.struct_types[id.0 as usize].get_undef().into(),
        }
    }

    /// Resolve an IR operand to an LLVM value. `None` means the producing
    /// instruction was skipped; callers degrade gracefully.
    fn value_of(
        &self,
        v: &Value,
        results: &HashMap<u32, BasicValueEnum<'ctx>>,
        llvm_fn: FunctionValue<'ctx>,
    ) -> Option<BasicValueEnum<'ctx>> {
        match v {
            Value::Const(c) => Some(self.const_value(c)),
            Value::Param(i) => llvm_fn.get_nth_param(*i),
            Value::Inst(id) => results.get(&id.0).copied(),
        }
    }

    // Coercions to the concrete value classes the builders require. A wrong
    // class (only possible on ill-typed IR) degrades to a typed undef.
    fn as_int(&self, v: BasicValueEnum<'ctx>) -> IntValue<'ctx> {
        if v.is_int_value() {
            v.into_int_value()
        } else if v.is_pointer_value() {
            // A pointer/reference compared or cast as an integer is its address
            // (`ptrtoint`) — this is what makes `ref == null` and pointer
            // equality work, not the bogus `undef` they used to fold to.
            self.builder
                .build_ptr_to_int(v.into_pointer_value(), self.ctx.i64_type(), "p2i")
                .unwrap()
        } else {
            self.ctx.i64_type().get_undef()
        }
    }

    fn as_float(&self, v: BasicValueEnum<'ctx>) -> FloatValue<'ctx> {
        if v.is_float_value() {
            v.into_float_value()
        } else {
            self.ctx.f64_type().get_undef()
        }
    }

    fn as_ptr(&self, v: BasicValueEnum<'ctx>) -> PointerValue<'ctx> {
        if v.is_pointer_value() {
            v.into_pointer_value()
        } else {
            self.ctx.ptr_type(AddressSpace::default()).get_undef()
        }
    }

    /// Reconcile a value whose actual LLVM type differs from the type the IR
    /// expects, coercing it to `ty`. The skew arises at external calls: a
    /// symbol's signature is fixed by its *first* declaration, so overloaded
    /// externs that share a C symbol (`Abs(float)`/`Abs(double)` → `@abs`) or a
    /// bare `malloc` vs corlib's `void* Malloc` make the call yield one width
    /// while sema expects another. Reconciling at the call site keeps every
    /// downstream use well-typed (so comparisons/arithmetic don't see a skew).
    fn reconcile_to(&self, v: BasicValueEnum<'ctx>, ty: IrType) -> BasicValueEnum<'ctx> {
        if v.get_type() == self.basic_type_of(ty) {
            return v;
        }
        let b = self.builder;
        match ty {
            IrType::Float { bits } if v.is_float_value() => {
                let fv = v.into_float_value();
                if bits >= 64 {
                    b.build_float_ext(fv, self.ctx.f64_type(), "fpext")
                        .unwrap()
                        .into()
                } else {
                    b.build_float_trunc(fv, self.ctx.f32_type(), "fptrunc")
                        .unwrap()
                        .into()
                }
            }
            IrType::Int { .. } | IrType::Bool if v.is_int_value() => {
                let iv = v.into_int_value();
                let want = self.int_type_of(ty);
                if iv.get_type().get_bit_width() < want.get_bit_width() {
                    if ty.is_signed() {
                        b.build_int_s_extend(iv, want, "sext").unwrap().into()
                    } else {
                        b.build_int_z_extend(iv, want, "zext").unwrap().into()
                    }
                } else {
                    b.build_int_truncate(iv, want, "trunc").unwrap().into()
                }
            }
            IrType::Int { .. } | IrType::Bool if v.is_pointer_value() => b
                .build_ptr_to_int(v.into_pointer_value(), self.int_type_of(ty), "ptrtoint")
                .unwrap()
                .into(),
            IrType::Ptr | IrType::Ref(_) if v.is_int_value() => b
                .build_int_to_ptr(
                    v.into_int_value(),
                    self.ctx.ptr_type(AddressSpace::default()),
                    "inttoptr",
                )
                .unwrap()
                .into(),
            _ => v,
        }
    }

    // ── function bodies ───────────────────────────────────────────────────

    fn lower_function(&self, func: &IrFunction) {
        let Some(llvm_fn) = self.module.get_function(&func.name) else {
            return;
        };
        // A same-named function already lowered (e.g. an un-mangled overload
        // collision) — don't append a second body.
        if llvm_fn.count_basic_blocks() > 0 || func.blocks.is_empty() {
            return;
        }

        // Pass A: materialize every block so branches/phis can target them.
        let blocks: Vec<BasicBlock<'ctx>> = func
            .blocks
            .iter()
            .map(|b| self.ctx.append_basic_block(llvm_fn, &b.label))
            .collect();

        let mut results: HashMap<u32, BasicValueEnum<'ctx>> = HashMap::new();
        let mut pending_phis: Vec<(PhiValue<'ctx>, Vec<(BlockId, Value)>)> = Vec::new();

        // Pass B: lower instructions block by block.
        for (bi, block) in func.blocks.iter().enumerate() {
            self.builder.position_at_end(blocks[bi]);
            for inst_id in &block.insts {
                let inst = &func.insts[inst_id.0 as usize];
                if let InstKind::Phi { incomings } = &inst.kind {
                    let phi = self
                        .builder
                        .build_phi(self.basic_type_of(inst.ty), "phi")
                        .unwrap();
                    results.insert(inst_id.0, phi.as_basic_value());
                    pending_phis.push((phi, incomings.clone()));
                } else if let Some(val) = self.lower_inst(&inst.kind, inst.ty, &results, llvm_fn) {
                    results.insert(inst_id.0, val);
                }
            }
            self.lower_term(&block.term, func.ret, &results, llvm_fn, &blocks);
        }

        // Pass C: wire phi incomings (forward refs / back-edges now resolved).
        for (phi, incomings) in pending_phis {
            let owned: Vec<(BasicValueEnum<'ctx>, BasicBlock<'ctx>)> = incomings
                .iter()
                .filter_map(|(bid, val)| {
                    self.value_of(val, &results, llvm_fn)
                        .map(|v| (v, blocks[bid.0 as usize]))
                })
                .collect();
            let refs: Vec<(&dyn BasicValue<'ctx>, BasicBlock<'ctx>)> = owned
                .iter()
                .map(|(v, b)| (v as &dyn BasicValue<'ctx>, *b))
                .collect();
            if !refs.is_empty() {
                phi.add_incoming(&refs);
            }
        }
    }

    fn lower_inst(
        &self,
        kind: &InstKind,
        ty: IrType,
        results: &HashMap<u32, BasicValueEnum<'ctx>>,
        llvm_fn: FunctionValue<'ctx>,
    ) -> Option<BasicValueEnum<'ctx>> {
        match kind {
            InstKind::Bin { op, lhs, rhs } => {
                let l = self.value_of(lhs, results, llvm_fn)?;
                let r = self.value_of(rhs, results, llvm_fn)?;
                Some(self.lower_bin(*op, l, r))
            }
            InstKind::Cmp { pred, lhs, rhs } => {
                let l = self.value_of(lhs, results, llvm_fn)?;
                let r = self.value_of(rhs, results, llvm_fn)?;
                Some(self.lower_cmp(*pred, l, r))
            }
            InstKind::Cast { kind, val } => {
                let v = self.value_of(val, results, llvm_fn)?;
                Some(self.lower_cast(*kind, v, ty))
            }
            InstKind::Alloca { elem } => Some(
                self.builder
                    .build_alloca(self.basic_type_of(*elem), "slot")
                    .unwrap()
                    .into(),
            ),
            InstKind::Load { ptr } => {
                let p = self.as_ptr(self.value_of(ptr, results, llvm_fn)?);
                Some(
                    self.builder
                        .build_load(self.basic_type_of(ty), p, "load")
                        .unwrap(),
                )
            }
            InstKind::Store { ptr, val } => {
                let p = self.as_ptr(self.value_of(ptr, results, llvm_fn)?);
                let v = self.value_of(val, results, llvm_fn)?;
                self.builder.build_store(p, v).unwrap();
                None
            }
            InstKind::FieldAddr {
                base,
                struct_id,
                field,
            } => {
                let p = self.as_ptr(self.value_of(base, results, llvm_fn)?);
                let sty = self.struct_types[struct_id.0 as usize];
                // Degrade to a skipped value on a bad index rather than panic.
                self.builder
                    .build_struct_gep(sty, p, *field, "field")
                    .ok()
                    .map(Into::into)
            }
            InstKind::SizeOf { struct_id } => self.struct_types[struct_id.0 as usize]
                .size_of()
                .map(Into::into),
            // RF-T2: the vtable base from a loaded `$header` (a `%ClassVData*`).
            // A struct-GEP into `%ClassVData` field 1 — LLVM computes the padded
            // offset (8: i32 + 4 pad → 8-aligned `[N x ptr]`), the SAME for any
            // N, so this canonical-type GEP reaches every class's vtable base.
            // Never a hand-rolled byte offset.
            InstKind::VtableBase { hdr } => {
                let h = self.as_ptr(self.value_of(hdr, results, llvm_fn)?);
                self.builder
                    .build_struct_gep(self.classvdata_ty, h, 1, "vtbl")
                    .ok()
                    .map(Into::into)
            }
            // RF-T2: the runtime type-id from an object's `$header` — load the
            // `$header` ptr from `obj` field 0 (raw offset-0 GEP, so it also
            // works for interface-typed `obj` with an empty StructDef), then
            // struct-GEP `%ClassVData` field 0 and `load i32` (the mType word).
            // Result is i32 (the registry index width — never i64). Wired by
            // sema for `obj.GetType()` in RF-T5; lowered here now so the inst is
            // complete and the helper is exercisable.
            InstKind::LoadTypeId { obj } => {
                let o = self.as_ptr(self.value_of(obj, results, llvm_fn)?);
                let hdr = self
                    .builder
                    .build_load(self.ctx.ptr_type(AddressSpace::default()), o, "hdr")
                    .unwrap();
                let hdrp = self.as_ptr(hdr);
                let slot = self
                    .builder
                    .build_struct_gep(self.classvdata_ty, hdrp, 0, "mtypep")
                    .ok()?;
                Some(
                    self.builder
                        .build_load(self.ctx.i32_type(), slot, "mtype")
                        .unwrap(),
                )
            }
            InstKind::ElemAddr { base, elem, index } => {
                let basep = self.as_ptr(self.value_of(base, results, llvm_fn)?);
                let idx = self.as_int(self.value_of(index, results, llvm_fn)?);
                let ety = self.basic_type_of(*elem);
                // SAFETY: a single-index GEP from a base pointer (pointer +
                // index scaled by sizeof(elem)). Degrades on a builder error.
                let gep = unsafe { self.builder.build_in_bounds_gep(ety, basep, &[idx], "elem") };
                gep.ok().map(Into::into)
            }
            InstKind::Call { callee, args } => {
                let argv: Vec<BasicValueEnum<'ctx>> = args
                    .iter()
                    .filter_map(|a| self.value_of(a, results, llvm_fn))
                    .collect();
                let f = self.get_or_declare(&callee.name, ty);
                let meta: Vec<BasicMetadataValueEnum<'ctx>> =
                    argv.iter().map(|v| (*v).into()).collect();
                let cs = self.builder.build_call(f, &meta, "call").unwrap();
                if ty == IrType::Void {
                    None
                } else {
                    cs.try_as_basic_value()
                        .basic()
                        .map(|v| self.reconcile_to(v, ty))
                }
            }
            InstKind::GlobalAddr { name } => self
                .module
                .get_global(name)
                .map(|g| g.as_pointer_value().into())
                // A `GlobalAddr` may name a *function* (a method reference taken
                // as a function pointer), not a global variable — fall back to
                // the function's code address.
                .or_else(|| {
                    self.module
                        .get_function(name)
                        .map(|f| f.as_global_value().as_pointer_value().into())
                }),
            InstKind::CallIndirect { callee, args } => {
                let fp = self.as_ptr(self.value_of(callee, results, llvm_fn)?);
                let argv: Vec<BasicValueEnum<'ctx>> = args
                    .iter()
                    .filter_map(|a| self.value_of(a, results, llvm_fn))
                    .collect();
                let param_tys: Vec<BasicMetadataTypeEnum<'ctx>> =
                    argv.iter().map(|v| v.get_type().into()).collect();
                let meta: Vec<BasicMetadataValueEnum<'ctx>> =
                    argv.iter().map(|v| (*v).into()).collect();
                let fty = if ty == IrType::Void {
                    self.ctx.void_type().fn_type(&param_tys, false)
                } else {
                    self.basic_type_of(ty).fn_type(&param_tys, false)
                };
                let cs = self
                    .builder
                    .build_indirect_call(fty, fp, &meta, "vcall")
                    .unwrap();
                if ty == IrType::Void {
                    None
                } else {
                    cs.try_as_basic_value()
                        .basic()
                        .map(|v| self.reconcile_to(v, ty))
                }
            }
            // Phis are created in `lower_function` so their results exist
            // before their incomings are wired.
            InstKind::Phi { .. } => None,
            InstKind::Select { cond, a, b } => {
                let c = self.as_int(self.value_of(cond, results, llvm_fn)?);
                let av = self.value_of(a, results, llvm_fn)?;
                let bv = self.value_of(b, results, llvm_fn)?;
                Some(self.builder.build_select(c, av, bv, "sel").unwrap())
            }
            InstKind::Trap { debug } => {
                // `@llvm.debugtrap` → int3 (resumable breakpoint);
                // `@llvm.trap` → ud2 (fatal illegal instruction). Both are
                // `void ()`; LLVM recognizes them as intrinsics by name.
                let name = if *debug {
                    "llvm.debugtrap"
                } else {
                    "llvm.trap"
                };
                let f = self.module.get_function(name).unwrap_or_else(|| {
                    let fty = self.ctx.void_type().fn_type(&[], false);
                    self.module.add_function(name, fty, None)
                });
                self.builder.build_call(f, &[], "").unwrap();
                None
            }
        }
    }

    fn lower_bin(
        &self,
        op: BinOp,
        l: BasicValueEnum<'ctx>,
        r: BasicValueEnum<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let b = self.builder;
        match op {
            BinOp::FAdd | BinOp::FSub | BinOp::FMul | BinOp::FDiv | BinOp::FRem => {
                let l = self.as_float(l);
                let r = self.as_float(r);
                let v = match op {
                    BinOp::FAdd => b.build_float_add(l, r, "fadd"),
                    BinOp::FSub => b.build_float_sub(l, r, "fsub"),
                    BinOp::FMul => b.build_float_mul(l, r, "fmul"),
                    BinOp::FDiv => b.build_float_div(l, r, "fdiv"),
                    BinOp::FRem => b.build_float_rem(l, r, "frem"),
                    _ => unreachable!(),
                };
                v.unwrap().into()
            }
            _ => {
                let l = self.as_int(l);
                let r = self.as_int(r);
                let v = match op {
                    BinOp::Add => b.build_int_add(l, r, "add"),
                    BinOp::Sub => b.build_int_sub(l, r, "sub"),
                    BinOp::Mul => b.build_int_mul(l, r, "mul"),
                    BinOp::SDiv => b.build_int_signed_div(l, r, "sdiv"),
                    BinOp::UDiv => b.build_int_unsigned_div(l, r, "udiv"),
                    BinOp::SRem => b.build_int_signed_rem(l, r, "srem"),
                    BinOp::URem => b.build_int_unsigned_rem(l, r, "urem"),
                    BinOp::And => b.build_and(l, r, "and"),
                    BinOp::Or => b.build_or(l, r, "or"),
                    BinOp::Xor => b.build_xor(l, r, "xor"),
                    BinOp::Shl => b.build_left_shift(l, r, "shl"),
                    BinOp::LShr => b.build_right_shift(l, r, false, "lshr"),
                    BinOp::AShr => b.build_right_shift(l, r, true, "ashr"),
                    _ => unreachable!(),
                };
                v.unwrap().into()
            }
        }
    }

    fn lower_cmp(
        &self,
        pred: CmpPred,
        l: BasicValueEnum<'ctx>,
        r: BasicValueEnum<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let b = self.builder;
        if pred.is_float() {
            let l = self.as_float(l);
            let r = self.as_float(r);
            let p = match pred {
                CmpPred::FOeq => FloatPredicate::OEQ,
                CmpPred::FOne => FloatPredicate::ONE,
                CmpPred::FOlt => FloatPredicate::OLT,
                CmpPred::FOle => FloatPredicate::OLE,
                CmpPred::FOgt => FloatPredicate::OGT,
                CmpPred::FOge => FloatPredicate::OGE,
                _ => unreachable!(),
            };
            b.build_float_compare(p, l, r, "fcmp").unwrap().into()
        } else {
            let l = self.as_int(l);
            let r = self.as_int(r);
            let p = match pred {
                CmpPred::Eq => IntPredicate::EQ,
                CmpPred::Ne => IntPredicate::NE,
                CmpPred::Slt => IntPredicate::SLT,
                CmpPred::Sle => IntPredicate::SLE,
                CmpPred::Sgt => IntPredicate::SGT,
                CmpPred::Sge => IntPredicate::SGE,
                CmpPred::Ult => IntPredicate::ULT,
                CmpPred::Ule => IntPredicate::ULE,
                CmpPred::Ugt => IntPredicate::UGT,
                CmpPred::Uge => IntPredicate::UGE,
                _ => unreachable!(),
            };
            b.build_int_compare(p, l, r, "icmp").unwrap().into()
        }
    }

    fn lower_cast(
        &self,
        kind: CastKind,
        v: BasicValueEnum<'ctx>,
        to: IrType,
    ) -> BasicValueEnum<'ctx> {
        let b = self.builder;
        match kind {
            CastKind::Trunc => b
                .build_int_truncate(self.as_int(v), self.int_type_of(to), "trunc")
                .unwrap()
                .into(),
            CastKind::ZExt => b
                .build_int_z_extend(self.as_int(v), self.int_type_of(to), "zext")
                .unwrap()
                .into(),
            CastKind::SExt => b
                .build_int_s_extend(self.as_int(v), self.int_type_of(to), "sext")
                .unwrap()
                .into(),
            CastKind::FpTrunc => b
                .build_float_trunc(self.as_float(v), self.float_type_of(to), "fptrunc")
                .unwrap()
                .into(),
            CastKind::FpExt => b
                .build_float_ext(self.as_float(v), self.float_type_of(to), "fpext")
                .unwrap()
                .into(),
            CastKind::FpToSi => b
                .build_float_to_signed_int(self.as_float(v), self.int_type_of(to), "fptosi")
                .unwrap()
                .into(),
            CastKind::FpToUi => b
                .build_float_to_unsigned_int(self.as_float(v), self.int_type_of(to), "fptoui")
                .unwrap()
                .into(),
            CastKind::SiToFp => b
                .build_signed_int_to_float(self.as_int(v), self.float_type_of(to), "sitofp")
                .unwrap()
                .into(),
            CastKind::UiToFp => b
                .build_unsigned_int_to_float(self.as_int(v), self.float_type_of(to), "uitofp")
                .unwrap()
                .into(),
            CastKind::Bitcast => b
                .build_bit_cast(v, self.basic_type_of(to), "bitcast")
                .unwrap(),
            // int↔ptr reinterprets. The IR types are advisory for external
            // calls: a symbol's *actual* LLVM signature is fixed by its first
            // declaration, so a value the IR believes is an integer can already
            // be a pointer (e.g. a bare `malloc` call whose `@malloc` was
            // declared `ptr` by corlib's `Internal.Malloc`). When the operand is
            // already in the target representation the conversion is the
            // identity — pass it through rather than feeding `inttoptr`/`ptrtoint`
            // an `undef` from `as_int`/`as_ptr`.
            CastKind::IntToPtr if v.is_pointer_value() => v,
            CastKind::IntToPtr => b
                .build_int_to_ptr(
                    self.as_int(v),
                    self.ctx.ptr_type(AddressSpace::default()),
                    "inttoptr",
                )
                .unwrap()
                .into(),
            CastKind::PtrToInt if v.is_int_value() => v,
            CastKind::PtrToInt => b
                .build_ptr_to_int(self.as_ptr(v), self.int_type_of(to), "ptrtoint")
                .unwrap()
                .into(),
        }
    }

    fn lower_term(
        &self,
        term: &Terminator,
        ret_ty: IrType,
        results: &HashMap<u32, BasicValueEnum<'ctx>>,
        llvm_fn: FunctionValue<'ctx>,
        blocks: &[BasicBlock<'ctx>],
    ) {
        let b = self.builder;
        match term {
            Terminator::Ret(v) => {
                if ret_ty == IrType::Void {
                    b.build_return(None).unwrap();
                } else {
                    let val = v
                        .as_ref()
                        .and_then(|val| self.value_of(val, results, llvm_fn))
                        .unwrap_or_else(|| self.undef_of(ret_ty));
                    b.build_return(Some(&val)).unwrap();
                }
            }
            Terminator::Br(target) => {
                b.build_unconditional_branch(blocks[target.0 as usize])
                    .unwrap();
            }
            Terminator::CondBr { cond, then, els } => match self.value_of(cond, results, llvm_fn) {
                Some(c) => {
                    b.build_conditional_branch(
                        self.as_int(c),
                        blocks[then.0 as usize],
                        blocks[els.0 as usize],
                    )
                    .unwrap();
                }
                None => {
                    b.build_unconditional_branch(blocks[then.0 as usize])
                        .unwrap();
                }
            },
            Terminator::Unreachable => {
                b.build_unreachable().unwrap();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{lower_to_string, verify_module};
    use newbf_ir::{
        BinOp, CmpPred, Const, FunctionBuilder, IrType, Module as IrModule, Param, Value,
    };

    fn module_with(f: newbf_ir::Function) -> IrModule {
        let mut m = IrModule::new("t");
        m.add_function(f);
        m
    }

    #[test]
    fn add_lowers_and_verifies() {
        // int add(int a, int b) => a + b;
        let mut f = FunctionBuilder::new(
            "add",
            vec![
                Param {
                    name: Some("a".into()),
                    ty: IrType::I64,
                },
                Param {
                    name: Some("b".into()),
                    ty: IrType::I64,
                },
            ],
            IrType::I64,
        );
        let (a, b) = (f.param(0), f.param(1));
        let s = f.bin(BinOp::Add, a, b, IrType::I64);
        f.ret(Some(s));
        let m = module_with(f.finish());

        verify_module(&m).expect("add verifies");
        let ir = lower_to_string(&m);
        assert!(ir.contains("define i64 @add(i64 %0, i64 %1)"), "{ir}");
        assert!(ir.contains("add i64 %0, %1"), "{ir}");
        assert!(ir.contains("ret i64"), "{ir}");
    }

    #[test]
    fn struct_alloca_and_field_gep_verifies() {
        use newbf_ir::{FieldDef, StructDef};
        // struct Point { i32 x; i32 y; }
        // i32 sum_xy() { Point p; p.x = 3; p.y = 4; return p.x + p.y; }
        let mut m = IrModule::new("t");
        let point = m.add_struct(StructDef {
            name: "Point".into(),
            fields: vec![
                FieldDef {
                    name: "x".into(),
                    ty: IrType::I32,
                },
                FieldDef {
                    name: "y".into(),
                    ty: IrType::I32,
                },
            ],
        });
        let mut f = FunctionBuilder::new("sum_xy", vec![], IrType::I32);
        let slot = f.alloca(IrType::Struct(point));
        let xp = f.field_addr(slot.clone(), point, 0);
        f.store(xp, Value::int(3, IrType::I32));
        let yp = f.field_addr(slot.clone(), point, 1);
        f.store(yp, Value::int(4, IrType::I32));
        let xp2 = f.field_addr(slot.clone(), point, 0);
        let x = f.load(xp2, IrType::I32);
        let yp2 = f.field_addr(slot, point, 1);
        let y = f.load(yp2, IrType::I32);
        let s = f.bin(BinOp::Add, x, y, IrType::I32);
        f.ret(Some(s));
        m.add_function(f.finish());

        verify_module(&m).expect("struct program verifies");
        let ir = lower_to_string(&m);
        assert!(ir.contains("%s0 = type { i32, i32 }"), "{ir}");
        assert!(ir.contains("alloca %s0"), "{ir}");
        assert!(ir.contains("getelementptr"), "{ir}");
    }

    #[test]
    fn new_shape_ref_and_sizeof_verifies() {
        use newbf_ir::{FieldDef, StructDef};
        // class C { int64 $hdr; i32 x; }
        // ref<C> mk() { p = malloc(sizeof C); p.$hdr = 0; p.x = 42; return p; }
        let mut m = IrModule::new("t");
        let c = m.add_struct(StructDef {
            name: "C".into(),
            fields: vec![
                FieldDef {
                    name: "$hdr".into(),
                    ty: IrType::I64,
                },
                FieldDef {
                    name: "x".into(),
                    ty: IrType::I32,
                },
            ],
        });
        m.declare_extern(
            "malloc",
            vec![Param {
                name: None,
                ty: IrType::I64,
            }],
            IrType::Ptr,
        );
        let mut f = FunctionBuilder::new("mk", vec![], IrType::Ref(c));
        let sz = f.size_of(c);
        let p = f.call("malloc", vec![sz], IrType::Ref(c));
        let hdr = f.field_addr(p.clone(), c, 0);
        f.store(hdr, Value::int(0, IrType::I64));
        let xp = f.field_addr(p.clone(), c, 1);
        f.store(xp, Value::int(42, IrType::I32));
        f.ret(Some(p));
        m.add_function(f.finish());

        verify_module(&m).expect("new-shape verifies");
        let ir = lower_to_string(&m);
        // `Ref` lowers to a plain pointer in the signature.
        assert!(ir.contains("define ptr @mk()"), "{ir}");
        assert!(ir.contains("call ptr @malloc"), "{ir}");
    }

    #[test]
    fn elem_addr_indexing_verifies() {
        // i32 at(i32* p, int i) { return p[i]; }
        let mut f = FunctionBuilder::new(
            "at",
            vec![
                Param {
                    name: Some("p".into()),
                    ty: IrType::Ptr,
                },
                Param {
                    name: Some("i".into()),
                    ty: IrType::I64,
                },
            ],
            IrType::I32,
        );
        let p = f.param(0);
        let i = f.param(1);
        let addr = f.elem_addr(p, IrType::I32, i);
        let v = f.load(addr, IrType::I32);
        f.ret(Some(v));
        let mut m = IrModule::new("t");
        m.add_function(f.finish());

        verify_module(&m).expect("indexing verifies");
        let ir = lower_to_string(&m);
        assert!(ir.contains("getelementptr"), "{ir}");
    }

    #[test]
    fn local_alloca_load_store_verifies() {
        // int x = 5; x = x + 1; return x;
        let mut f = FunctionBuilder::new("local", vec![], IrType::I64);
        let slot = f.alloca(IrType::I64);
        f.store(slot.clone(), Value::int(5, IrType::I64));
        let cur = f.load(slot.clone(), IrType::I64);
        let inc = f.bin(BinOp::Add, cur, Value::int(1, IrType::I64), IrType::I64);
        f.store(slot.clone(), inc);
        let out = f.load(slot, IrType::I64);
        f.ret(Some(out));
        let m = module_with(f.finish());

        verify_module(&m).expect("local verifies");
        let ir = lower_to_string(&m);
        assert!(ir.contains("alloca i64"), "{ir}");
        assert!(ir.contains("store i64"), "{ir}");
        assert!(ir.contains("load i64"), "{ir}");
    }

    #[test]
    fn if_diamond_with_phi_verifies() {
        // int max(int a, int b) via control flow + phi.
        let mut f = FunctionBuilder::new(
            "max",
            vec![
                Param {
                    name: None,
                    ty: IrType::I64,
                },
                Param {
                    name: None,
                    ty: IrType::I64,
                },
            ],
            IrType::I64,
        );
        let (a, b) = (f.param(0), f.param(1));
        let then_b = f.create_block("then");
        let else_b = f.create_block("else");
        let join = f.create_block("join");
        let c = f.cmp(CmpPred::Sgt, a.clone(), b.clone());
        f.cond_br(c, then_b, else_b);
        f.switch_to(then_b);
        f.br(join);
        f.switch_to(else_b);
        f.br(join);
        f.switch_to(join);
        let m = f.phi(vec![(then_b, a), (else_b, b)], IrType::I64);
        f.ret(Some(m));
        let module = module_with(f.finish());

        verify_module(&module).expect("max verifies");
        let ir = lower_to_string(&module);
        assert!(ir.contains("icmp sgt i64"), "{ir}");
        assert!(ir.contains("br i1"), "{ir}");
        assert!(ir.contains("phi i64"), "{ir}");
    }

    #[test]
    fn extern_and_call_verifies() {
        let mut m = IrModule::new("t");
        m.declare_extern(
            "puts",
            vec![Param {
                name: None,
                ty: IrType::Ptr,
            }],
            IrType::I32,
        );
        let mut f = FunctionBuilder::new("main", vec![], IrType::I32);
        let r = f.call("puts", vec![Value::Const(Const::Null)], IrType::I32);
        f.ret(Some(r));
        m.add_function(f.finish());

        verify_module(&m).expect("extern+call verifies");
        let ir = lower_to_string(&m);
        assert!(ir.contains("declare i32 @puts(ptr"), "{ir}");
        assert!(ir.contains("call i32 @puts(ptr null)"), "{ir}");
    }

    #[test]
    fn string_constant_lowers_to_global() {
        // void greet() { puts("hi"); }
        let mut m = IrModule::new("t");
        let mut f = FunctionBuilder::new("greet", vec![], IrType::Void);
        f.call("puts", vec![Value::str("hi")], IrType::I32);
        f.ret(None);
        m.add_function(f.finish());

        verify_module(&m).expect("string program verifies");
        let ir = lower_to_string(&m);
        assert!(ir.contains("private constant [3 x i8]"), "{ir}"); // 'h','i','\0'
        assert!(ir.contains("@puts(ptr "), "{ir}");
    }

    #[test]
    fn trap_lowers_to_intrinsic_and_verifies() {
        // void crash() { debugtrap; return; }
        let mut f = FunctionBuilder::new("crash", vec![], IrType::Void);
        f.trap(true);
        f.ret(None);
        let m = module_with(f.finish());

        verify_module(&m).expect("trap verifies");
        let ir = lower_to_string(&m);
        assert!(ir.contains("call void @llvm.debugtrap()"), "{ir}");
    }

    #[test]
    fn floats_lower_and_verify() {
        // double fma(double x, double y) => x * y + 1.5;
        let mut f = FunctionBuilder::new(
            "fma",
            vec![
                Param {
                    name: None,
                    ty: IrType::F64,
                },
                Param {
                    name: None,
                    ty: IrType::F64,
                },
            ],
            IrType::F64,
        );
        let (x, y) = (f.param(0), f.param(1));
        let p = f.bin(BinOp::FMul, x, y, IrType::F64);
        let r = f.bin(BinOp::FAdd, p, Value::float(1.5, IrType::F64), IrType::F64);
        f.ret(Some(r));
        let m = module_with(f.finish());

        verify_module(&m).expect("fma verifies");
        let ir = lower_to_string(&m);
        assert!(ir.contains("fmul double"), "{ir}");
        assert!(ir.contains("fadd double"), "{ir}");
    }

    /// RF-T2 SLOT-SHIFT DETECTOR (R2). The ONLY IR-level guard against a
    /// verify-clean physical vtable slot-shift: the verify corpus and the itable
    /// invariant harness operate on the logical slot model and CANNOT see the
    /// `{ i32 mType, pad }` ClassVData prefix shifting every slot. This pins
    /// both halves of the ABI:
    ///   (a) the emitted ClassVData global is `%ClassVData = { i32, [N x ptr] }`
    ///       — `i32 mType` FIRST, then the `[N x ptr]` vtable array;
    ///   (b) a virtual/interface dispatch GEPs **field 1** (`i32 0, i32 1`) of
    ///       the canonical `%ClassVData` type to reach the vtable base — NOT
    ///       field 0 (which would dispatch through the type-id word).
    /// A regression that drops the `i32` prefix, GEPs field 0, or emits a bare
    /// `[N x ptr]` array trips this even when the run-corpus happens to miss the
    /// affected class shape.
    #[test]
    fn classvdata_shape_and_field1_dispatch_emit() {
        use newbf_ir::{FieldDef, StructDef, VtableDef};

        let mut m = IrModule::new("t");
        // class C { ptr $header; i32 x; } — field 0 is the $header (a
        // ClassVData*), exactly as sema lays out a `StructKind::Ref`.
        let c = m.add_struct(StructDef {
            name: "C".into(),
            fields: vec![
                FieldDef {
                    name: "$header".into(),
                    ty: IrType::Ptr,
                },
                FieldDef {
                    name: "x".into(),
                    ty: IrType::I32,
                },
            ],
        });
        // Two virtual-slot impls so N = 2 ⇒ the vtable array is `[2 x ptr]`.
        for sym in ["C.M0", "C.M1"] {
            let g = FunctionBuilder::new(sym, vec![], IrType::I32);
            m.add_function(g.finish());
        }
        // The ClassVData for C: `{ i32 mType (=0), [2 x ptr] }`.
        m.add_vtable(VtableDef {
            name: "C.$cvdata".into(),
            entries: vec!["C.M0".into(), "C.M1".into()],
            type_id: 0,
        });

        // A function doing a virtual dispatch on a `C*` receiver, exactly as
        // sema's `load_vtable_base`: load `$header` (offset-0 ptr GEP), VtableBase
        // (struct-GEP %ClassVData field 1), index slot 1, load + call_indirect.
        let mut f = FunctionBuilder::new(
            "dispatch",
            vec![Param {
                name: Some("obj".into()),
                ty: IrType::Ref(c),
            }],
            IrType::I32,
        );
        let obj = f.param(0);
        let hdr_addr = f.elem_addr(obj.clone(), IrType::Ptr, Value::int(0, IrType::I64));
        let hdr = f.load(hdr_addr, IrType::Ptr);
        let vtbl = f.vtable_base(hdr);
        let slotp = f.elem_addr(vtbl, IrType::Ptr, Value::int(1, IrType::I64));
        let fnptr = f.load(slotp, IrType::Ptr);
        let r = f.call_indirect(fnptr, vec![obj], IrType::I32);
        f.ret(Some(r));
        m.add_function(f.finish());

        verify_module(&m).expect("ClassVData dispatch module verifies");
        let ir = lower_to_string(&m);

        // (a) The canonical %ClassVData GEP type leads with i32, then the
        // pointer array — the type-id word FIRST.
        assert!(
            ir.contains("%ClassVData = type { i32, [0 x ptr] }"),
            "canonical ClassVData GEP type must be {{ i32, [0 x ptr] }}:\n{ir}"
        );
        // (a) The emitted per-class global is `{ i32, [2 x ptr] }` (N = 2).
        assert!(
            ir.contains("@\"C.$cvdata\" = constant { i32, [2 x ptr] }"),
            "ClassVData global must have shape {{ i32, [2 x ptr] }}:\n{ir}"
        );
        // (b) Dispatch GEPs FIELD 1 of %ClassVData (i32 0, i32 1) — the vtable
        // base, past the mType word. A field-0 GEP here would be the slot-shift.
        // (LLVM may print the GEP `inbounds`/`inbounds nuw`; match on the type +
        // the field-1 index pair, which together pin the struct-GEP into field 1.)
        assert!(
            ir.contains("%ClassVData, ptr %load, i32 0, i32 1"),
            "dispatch must struct-GEP %ClassVData field 1 (i32 0, i32 1):\n{ir}"
        );
    }

    /// RF-T2 companion: `LoadTypeId` lowers to a load of `%ClassVData` field 0
    /// (`i32 mType`) — the type-id read GEPs field 0, the COMPLEMENT of the
    /// dispatch field-1 GEP. (Sema wires `obj.GetType()` to this in RF-T5; the
    /// lowering is complete now so the helper is exercisable.)
    #[test]
    fn load_type_id_reads_classvdata_field0_i32() {
        use newbf_ir::{FieldDef, StructDef};

        let mut m = IrModule::new("t");
        let c = m.add_struct(StructDef {
            name: "C".into(),
            fields: vec![
                FieldDef {
                    name: "$header".into(),
                    ty: IrType::Ptr,
                },
                FieldDef {
                    name: "x".into(),
                    ty: IrType::I32,
                },
            ],
        });
        let mut f = FunctionBuilder::new(
            "tid",
            vec![Param {
                name: Some("obj".into()),
                ty: IrType::Ref(c),
            }],
            IrType::I32,
        );
        let obj = f.param(0);
        let id = f.load_type_id(obj);
        f.ret(Some(id));
        m.add_function(f.finish());

        verify_module(&m).expect("LoadTypeId module verifies");
        let ir = lower_to_string(&m);
        // field 0 GEP (i32 0, i32 0) then a 32-bit load — the mType word.
        assert!(
            ir.contains("%ClassVData, ptr %hdr, i32 0, i32 0"),
            "LoadTypeId must struct-GEP %ClassVData field 0 (i32 0, i32 0):\n{ir}"
        );
        assert!(ir.contains("load i32"), "LoadTypeId loads i32 (the type-id):\n{ir}");
    }

    /// RF-T4 strip differential (the deterministic, non-JIT detector): a marked
    /// type's Type global references a non-null `%struct.FieldInfo` array; an
    /// unmarked type's `mFields` is null and NO FieldInfo array global is emitted.
    /// This proves policy gating at emission, independent of running the JIT.
    #[test]
    fn emit_metadata_strips_fields_unless_marked() {
        use newbf_ir::{
            FieldDef, FieldMeta, ReflectPolicy, StructDef, TypeMeta, VtableDef,
        };

        let mut m = IrModule::new("t");
        // Two classes, each `{ $header: ptr, mX: i32, mY: i32 }`.
        let mk = |m: &mut IrModule, name: &str| {
            m.add_struct(StructDef {
                name: name.into(),
                fields: vec![
                    FieldDef { name: "$header".into(), ty: IrType::Ptr },
                    FieldDef { name: "mX".into(), ty: IrType::I32 },
                    FieldDef { name: "mY".into(), ty: IrType::I32 },
                ],
            })
        };
        let marked = mk(&mut m, "Marked");
        let unmarked = mk(&mut m, "Unmarked");

        // ClassVData globals (so `type_global_name`'s prefix is recoverable).
        m.add_vtable(VtableDef { name: "Marked.$cvdata".into(), entries: vec![], type_id: 0 });
        m.add_vtable(VtableDef { name: "Unmarked.$cvdata".into(), entries: vec![], type_id: 1 });

        let user_fields = || {
            vec![
                FieldMeta { name: "mX".into(), ty: IrType::I32, field_index: 1 },
                FieldMeta { name: "mY".into(), ty: IrType::I32, field_index: 2 },
            ]
        };
        // Marked: TYPE|FIELDS ⇒ a FieldInfo array. Unmarked: TYPE only ⇒ stripped.
        // CA-T0: `TypeMeta::new` defaults `attributes` empty (the test exercises
        // field reflection only — no custom attributes).
        m.add_type_meta(TypeMeta::new(
            0,
            marked,
            "Marked".into(),
            ReflectPolicy(ReflectPolicy::TYPE.0 | ReflectPolicy::FIELDS.0),
            true,
            user_fields(),
            vec![],
        ));
        m.add_type_meta(TypeMeta::new(
            1,
            unmarked,
            "Unmarked".into(),
            ReflectPolicy::TYPE,
            true,
            vec![], // sema records none when FIELDS is stripped
            vec![],
        ));

        verify_module(&m).expect("metadata module verifies");
        let ir = lower_to_string(&m);

        // The marked Type global + its FieldInfo array exist; the registry +
        // in-module accessor are emitted.
        assert!(
            ir.contains("@\"Marked.$type\"") || ir.contains("@Marked.$type"),
            "marked Type global emitted:\n{ir}"
        );
        assert!(
            ir.contains("%struct.FieldInfo") && ir.contains("@.fieldinfo"),
            "marked type emits a %struct.FieldInfo array:\n{ir}"
        );
        // The strip differential: the marked type emits a FieldInfo array; the
        // unmarked type's array is absent (its `mFields` is null) — pinned by the
        // unmarked-only module below.
        assert!(
            ir.contains("@__newbf_type_table")
                && ir.contains("@__newbf_type_count")
                && ir.contains("@__newbf_type_unknown"),
            "registry table/count/unknown emitted:\n{ir}"
        );
        assert!(
            ir.contains("define ptr @__newbf_type_by_id(i32"),
            "in-module accessor emitted:\n{ir}"
        );

        // A module with ONLY the unmarked type emits NO FieldInfo array at all
        // (the clean strip side of the differential).
        let mut m2 = IrModule::new("t2");
        let only = m2.add_struct(StructDef {
            name: "Unmarked".into(),
            fields: vec![
                FieldDef { name: "$header".into(), ty: IrType::Ptr },
                FieldDef { name: "mX".into(), ty: IrType::I32 },
            ],
        });
        m2.add_vtable(VtableDef { name: "Unmarked.$cvdata".into(), entries: vec![], type_id: 0 });
        m2.add_type_meta(TypeMeta::new(
            0,
            only,
            "Unmarked".into(),
            ReflectPolicy::TYPE,
            true,
            vec![],
            vec![],
        ));
        let ir2 = lower_to_string(&m2);
        // The `%struct.FieldInfo` named TYPE is always declared, but NO
        // `@.fieldinfo` array GLOBAL is emitted when every type strips fields —
        // the clean strip side of the differential.
        assert!(
            !ir2.contains("@.fieldinfo"),
            "an unmarked-only module emits no FieldInfo array global (strip):\n{ir2}"
        );
    }

    /// MS-T7: `Module::alloc_sites` lowers to the `__newbf_alloc_sites` table the
    /// runtime guard registers — `%struct.AllocSite = { ptr function, ptr file,
    /// i32 line }`, a constant array of those, plus an `i32` count. The runtime's
    /// `AllocSiteRaw` reader pins this exact field order/types (memory-safety.md
    /// §A7). The strings + line of each site appear in the emitted IR. (This test
    /// only runs in debug — release omits the table, gated by `debug_assertions`.)
    #[cfg(debug_assertions)]
    #[test]
    fn emit_alloc_sites_table_with_function_file_line() {
        use newbf_ir::AllocSite;

        let mut m = IrModule::new("t");
        m.alloc_sites.push(AllocSite {
            function: "Program.Main".into(),
            file: "uaf.bf".into(),
            line: 4,
        });
        m.alloc_sites.push(AllocSite {
            function: "Node.$ctor0".into(),
            file: "node.bf".into(),
            line: 17,
        });
        let ir = lower_to_string(&m);
        assert!(
            ir.contains("%struct.AllocSite = type { ptr, ptr, i32 }"),
            "the AllocSite reader layout must be {{ ptr, ptr, i32 }}:\n{ir}"
        );
        assert!(
            ir.contains("@__newbf_alloc_sites = constant [2 x %struct.AllocSite]"),
            "the site table must be a 2-entry constant array:\n{ir}"
        );
        assert!(
            ir.contains("@__newbf_alloc_sites_count = constant i32 2"),
            "the count global must equal the number of sites:\n{ir}"
        );
        // The site strings + lines are present (emitted as private cstr globals).
        for needle in ["Program.Main", "uaf.bf", "i32 4", "Node.$ctor0", "node.bf", "i32 17"] {
            assert!(ir.contains(needle), "site data {needle:?} missing:\n{ir}");
        }
    }

    /// MS-T7: an allocation-free module still emits an EMPTY table + count 0 (so
    /// the host's `lookup("__newbf_alloc_sites")` resolves to a clean, empty
    /// registration rather than failing). Debug-only.
    #[cfg(debug_assertions)]
    #[test]
    fn emit_alloc_sites_empty_table_when_no_sites() {
        let m = IrModule::new("t");
        let ir = lower_to_string(&m);
        assert!(
            ir.contains("@__newbf_alloc_sites = constant [0 x %struct.AllocSite]"),
            "an allocation-free module emits a [0 x ...] table:\n{ir}"
        );
        assert!(
            ir.contains("@__newbf_alloc_sites_count = constant i32 0"),
            "count 0 for an allocation-free module:\n{ir}"
        );
    }
}
