//! `newbf-ir` — the NewBF typed SSA IR.
//!
//! A single, typed, post-monomorphization SSA intermediate representation
//! that lowers straight to LLVM (decided 2026-05-30 — *not* a two-level IR;
//! see MANIFESTO core decision 3). Every [`Value`] carries a resolved
//! [`IrType`]; functions are basic blocks of [`InstData`] with one
//! [`Terminator`] each; locals are addressable via `alloca`/`load`/`store`.
//!
//! The IR is **environment-agnostic** — it has no notion of "app" vs.
//! "comptime". Which world a [`Module`] is lowered/JIT'd into is decided by
//! the lowering + JIT layer, so the same IR + pipeline serves both the
//! application and the comptime evaluator.
//!
//! Sprint 06 scope: the IR core + the `dump-ir` report. Lowering from the
//! def graph / AST (the primitive kernel) and LLVM emission follow in
//! Sprints 06b/07. Lowering reference: `E:\beef\IDEHelper\Compiler\
//! BfIRCodeGen.cpp` (Beef's own LLVM emitter).

mod func;
mod inst;
mod module;
mod print;
mod ty;

pub use func::{Block, Function, FunctionBuilder, Param};
pub use inst::{
    BinOp, BlockId, Callee, CastKind, CmpPred, Const, InstData, InstId, InstKind, Terminator, Value,
};
pub use module::{
    AllocSite, AttrMeta, EmitJob, FieldDef, FieldMeta, GlobalDef, MethodMeta, Module, ReflectPolicy,
    StructDef, TypeMeta, VtableDef,
};
pub use print::{format_ir, format_reflection};
pub use ty::{IrType, StructId};

#[cfg(test)]
mod tests {
    use super::*;

    /// `int add(int a, int b) => a + b;`
    fn build_add() -> Function {
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
        let a = f.param(0);
        let b = f.param(1);
        let s = f.bin(BinOp::Add, a, b, IrType::I64);
        f.ret(Some(s));
        f.finish()
    }

    #[test]
    fn ssa_numbering_params_then_results() {
        let f = build_add();
        let m = {
            let mut m = Module::new("t");
            m.add_function(f);
            m
        };
        let r = format_ir(&m);
        // params %0,%1; the add result is %2; ret references it.
        assert!(r.contains("func @add(i64 %0, i64 %1) -> i64"), "{r}");
        assert!(r.contains("%2 = add i64 %0, %1"), "{r}");
        assert!(r.contains("ret %2"), "{r}");
    }

    #[test]
    fn addressable_local_via_alloca_load_store() {
        // int x = 5; x = x + 1; return x;
        let mut f = FunctionBuilder::new("local", vec![], IrType::I64);
        let slot = f.alloca(IrType::I64); // %0
        f.store(slot.clone(), Value::int(5, IrType::I64));
        let cur = f.load(slot.clone(), IrType::I64); // %1
        let inc = f.bin(BinOp::Add, cur, Value::int(1, IrType::I64), IrType::I64); // %2
        f.store(slot.clone(), inc);
        let out = f.load(slot, IrType::I64); // %3
        f.ret(Some(out));
        let mut m = Module::new("t");
        m.add_function(f.finish());
        let r = format_ir(&m);
        assert!(r.contains("%0 = alloca i64"), "{r}");
        assert!(r.contains("store 5, %0"), "{r}");
        assert!(r.contains("%2 = add i64 %1, 1"), "{r}");
        // store yields no value, so it isn't numbered (next load is %3).
        assert!(r.contains("%3 = load i64, %0"), "{r}");
    }

    #[test]
    fn if_diamond_with_phi() {
        // int max(int a, int b) { return a > b ? a : b; } via control flow.
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
        let a = f.param(0);
        let b = f.param(1);
        let then_b = f.create_block("then");
        let else_b = f.create_block("else");
        let join = f.create_block("join");
        let c = f.cmp(CmpPred::Sgt, a.clone(), b.clone()); // %2
        f.cond_br(c, then_b, else_b);
        f.switch_to(then_b);
        f.br(join);
        f.switch_to(else_b);
        f.br(join);
        f.switch_to(join);
        let m = f.phi(vec![(then_b, a), (else_b, b)], IrType::I64);
        f.ret(Some(m));
        let mut module = Module::new("t");
        module.add_function(f.finish());
        let r = format_ir(&module);
        // Blocks get a unique index suffix (then→then1, else→else2, join→join3).
        assert!(r.contains("icmp sgt %0, %1"), "{r}");
        assert!(r.contains("condbr %2, then1, else2"), "{r}");
        assert!(r.contains("phi i64 [ %0, then1 ], [ %1, else2 ]"), "{r}");
    }

    #[test]
    fn extern_declaration_and_call() {
        let mut m = Module::new("t");
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
        let out = format_ir(&m);
        assert!(out.contains("declare @puts(ptr %0) -> i32"), "{out}");
        assert!(out.contains("call i32 @puts(null)"), "{out}");
        assert!(
            out.starts_with("ir module \"t\": 1 functions, 1 externs"),
            "{out}"
        );
    }

    #[test]
    fn trap_intrinsics_render() {
        // void t() { debugtrap; trap; ret; }
        let mut f = FunctionBuilder::new("t", vec![], IrType::Void);
        f.trap(true);
        f.trap(false);
        f.ret(None);
        let mut m = Module::new("m");
        m.add_function(f.finish());
        let r = format_ir(&m);
        assert!(r.contains("    debugtrap\n"), "{r}");
        // 4-space indent before bare `trap` distinguishes it from `debugtrap`.
        assert!(r.contains("    trap\n"), "{r}");
    }

    #[test]
    fn struct_layout_alloca_fieldaddr() {
        // struct Point { i32 x; i32 y; }
        // int sum_xy() { Point p; p.x = 3; p.y = 4; return p.x + p.y; }
        let mut m = Module::new("t");
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
        let slot = f.alloca(IrType::Struct(point)); // %0 : ptr to Point
        let xp = f.field_addr(slot.clone(), point, 0); // %1
        f.store(xp, Value::int(3, IrType::I32));
        let yp = f.field_addr(slot.clone(), point, 1); // %2
        f.store(yp, Value::int(4, IrType::I32));
        let xp2 = f.field_addr(slot.clone(), point, 0); // %3
        let x = f.load(xp2, IrType::I32); // %4
        let yp2 = f.field_addr(slot, point, 1); // %5
        let y = f.load(yp2, IrType::I32); // %6
        let s = f.bin(BinOp::Add, x, y, IrType::I32); // %7
        f.ret(Some(s));
        m.add_function(f.finish());
        let r = format_ir(&m);
        assert!(r.contains("%s0 = type { i32, i32 }  ; Point"), "{r}");
        assert!(r.contains("%0 = alloca %s0"), "{r}");
        assert!(r.contains("%1 = fieldaddr %s0, %0, 0"), "{r}");
        assert!(r.contains("store 3, %1"), "{r}");
        assert!(r.contains("store 4, %2"), "{r}");
    }

    #[test]
    fn ref_type_and_sizeof_render() {
        // ref<C> mk() { return (C)malloc(sizeof C); }  — class C { int64 hdr; i32 x; }
        let mut m = Module::new("t");
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
        let mut f = FunctionBuilder::new("mk", vec![], IrType::Ref(c));
        let sz = f.size_of(c); // %0 : i64
        let p = f.call("malloc", vec![sz], IrType::Ref(c)); // %1 : &s0
        f.ret(Some(p));
        m.add_function(f.finish());
        let r = format_ir(&m);
        assert!(r.contains("func @mk() -> &s0"), "{r}");
        assert!(r.contains("%0 = sizeof %s0"), "{r}");
        assert!(r.contains("%1 = call &s0 @malloc(%0)"), "{r}");
    }

    #[test]
    fn elem_addr_indexing_renders() {
        // i32 at2(i32* p) => p[2]
        let mut f = FunctionBuilder::new(
            "at2",
            vec![Param {
                name: Some("p".into()),
                ty: IrType::Ptr,
            }],
            IrType::I32,
        );
        let p = f.param(0);
        let addr = f.elem_addr(p, IrType::I32, Value::int(2, IrType::I64)); // %1
        let v = f.load(addr, IrType::I32); // %2
        f.ret(Some(v));
        let mut m = Module::new("t");
        m.add_function(f.finish());
        let r = format_ir(&m);
        assert!(r.contains("%1 = elemaddr i32, %0, 2"), "{r}");
        assert!(r.contains("%2 = load i32, %1"), "{r}");
    }

    #[test]
    fn types_and_floats() {
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
        let x = f.param(0);
        let y = f.param(1);
        let p = f.bin(BinOp::FMul, x, y, IrType::F64);
        let r = f.bin(BinOp::FAdd, p, Value::float(1.5, IrType::F64), IrType::F64);
        f.ret(Some(r));
        let mut m = Module::new("t");
        m.add_function(f.finish());
        let out = format_ir(&m);
        assert!(out.contains("%2 = fmul f64 %0, %1"), "{out}");
        assert!(out.contains("%3 = fadd f64 %2, 1.5"), "{out}");
    }
}
