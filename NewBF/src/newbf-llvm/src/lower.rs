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
    Module as IrModule, Param, Terminator, Value,
};

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

/// Lower an IR module into an `inkwell` module owned by `ctx`. Both must
/// outlive any JIT engine or object emission built from the result.
pub fn emit_module<'ctx>(ctx: &'ctx Context, ir: &IrModule) -> LlvmModule<'ctx> {
    let module = ctx.create_module(&ir.name);
    let builder = ctx.create_builder();
    let mut cg = Codegen {
        ctx,
        module: &module,
        builder: &builder,
        struct_types: Vec::new(),
    };
    cg.build_struct_types(ir);
    cg.declare_all(ir);
    cg.emit_vtables(ir);
    cg.emit_globals(ir);
    for f in &ir.funcs {
        if !f.is_extern {
            cg.lower_function(f);
        }
    }
    module
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

    /// Emit each class vtable as a constant global array of function pointers.
    /// Functions are already declared, so each slot resolves to its pointer
    /// (a missing entry — e.g. an abstract slot — becomes null).
    fn emit_vtables(&self, ir: &IrModule) {
        let ptr_ty = self.ctx.ptr_type(AddressSpace::default());
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
            let arr = ptr_ty.const_array(&entries);
            let g = self
                .module
                .add_global(ptr_ty.array_type(entries.len() as u32), None, &vt.name);
            g.set_initializer(&arr);
            g.set_constant(true);
        }
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
}
