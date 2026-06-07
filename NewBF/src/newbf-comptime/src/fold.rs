//! Comptime call-site folding: evaluate `[Comptime]` functions at compile time
//! and replace their call sites in ordinary code with the resulting literal.
//!
//! This is the second half of comptime (the first, `eval_const_i64`, evaluates a
//! single nullary function). A `[Comptime]` function is compile-time-only: it
//! must not survive into the final program. `fold_comptime` walks the call sites
//! of the symbols `newbf-sema` marked (`module.comptime`), JIT-evaluates each
//! foldable one against the whole module, rewrites its call into an integer
//! literal, and then drops the comptime functions that nothing references.
//!
//! **Constant arguments are supported via a wrapper.** A call
//! `Factorial(5)` is folded by synthesizing a nullary
//! `$ct_eval() => Factorial(5)` into a clone of the module and JIT-evaluating
//! *that* — arg marshalling without an FFI calling convention. The wrapper is
//! type-safe by construction: it copies the original call (constant args
//! included), which already type-checked.
//!
//! **Width breadth (CB-T6).** Foldable = a non-extern comptime function whose
//! return type is a width-bounded integer (`i8/i16/i32/i64`, signed/unsigned) or
//! `bool` (everything [`eval_const`] can read), called with all-integer-constant
//! args. The call site is JIT-evaluated *at the call's own result width* and
//! rewritten to a literal of the call instruction's **own** [`IrType`] (not a
//! hardcoded `i64`), so an `int32`-returning fold yields an `i32` constant that
//! matches every SSA use and the module verifies. A non-constant-arg call site
//! is left as an ordinary call (and its function kept), rather than failing the
//! compile.
//!
//! **Inner-fold-first / fixpoint.** A comptime call whose arguments are
//! themselves comptime calls — `Outer(Inner(3))` — folds bottom-up: once `Inner`
//! folds to a constant, the next pass sees `Outer(<const>)` and folds it too. The
//! collect/apply loop iterates to a **fixpoint** (until a pass folds nothing),
//! bounded by the total instruction count, so nested comptime calls fully
//! collapse to a single literal.

use std::collections::{HashMap, HashSet};

use newbf_ir::{
    BinOp, Const, FunctionBuilder, InstKind, IrType, Module as IrModule, Value,
};

use crate::eval::eval_const;

/// Whether a comptime function's *return* type is foldable: a width-bounded
/// integer (`i8/i16/i32/i64`, signed or unsigned) or `bool`. These are exactly
/// the types [`eval_const`] can read back from the JIT (CB-T1); float/ptr/struct
/// returns are left as real calls (the `__real@`/heap-marshalling gaps).
fn is_foldable_ret(ty: IrType) -> bool {
    matches!(ty, IrType::Int { bits, .. } if bits <= 64) || ty == IrType::Bool
}

/// Resolve an argument operand to an integer constant `(value, type)` if it is
/// one *now*. A literal `Const::Int` resolves directly; an `Inst` reference
/// resolves through the function's instruction arena (`insts`) for the two shapes
/// that wrap a compile-time-constant integer:
///
/// 1. the **identity `add v, 0` of integer constants** the fold pass itself
///    writes when it folds a comptime call — i.e. a previously-folded *inner*
///    comptime call (this is what lets `Outer(Inner(3))` fold inner-first across
///    fixpoint passes);
/// 2. an **integer-to-integer cast of an integer constant** (`trunc`/`zext`/
///    `sext`/`bitcast`) — sema lowers a literal like `7` as `i64` and inserts a
///    `Cast` to the parameter's width (`F(7)` becomes `F(trunc i64 7 to i32)`), so
///    the call's arg is an `Inst` pointing at that cast, not a bare `Const`.
///    The resolved type is the cast's **result** width; the source constant value
///    is carried verbatim — the wrapper's `Const::Int(value, dest_ty)` lowering
///    truncates/extends it to the destination width exactly as LLVM would.
///
/// Any other operand (a runtime value, a non-folded instruction, a non-int
/// constant, a float/ptr cast) is not a compile-time integer constant.
fn arg_const(insts: &[newbf_ir::InstData], v: &Value) -> Option<(i128, IrType)> {
    use newbf_ir::CastKind;
    match v {
        Value::Const(Const::Int(val, t)) => Some((*val, *t)),
        Value::Inst(id) => {
            let inst = insts.get(id.0 as usize)?;
            match &inst.kind {
                // A folded inner comptime call (identity `add v, 0`).
                InstKind::Bin {
                    op: BinOp::Add,
                    lhs: Value::Const(Const::Int(val, t)),
                    rhs: Value::Const(Const::Int(0, _)),
                } => Some((*val, *t)),
                // An int→int cast of an integer constant: the value at the cast's
                // result width. Only width-changing integer casts (not int↔float
                // or int↔ptr, which have no sound `i128` value here).
                InstKind::Cast {
                    kind: CastKind::Trunc | CastKind::ZExt | CastKind::SExt | CastKind::Bitcast,
                    val: Value::Const(Const::Int(val, _)),
                } if inst.ty.is_int() => Some((*val, inst.ty)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Evaluate the `[Comptime]` functions in `module` and fold their call sites
/// into literals, then remove the comptime functions nothing references
/// (compile-time only).
///
/// A no-op when `module.comptime` is empty (the common case), so ordinary
/// programs pay nothing. Returns `Err` only if JIT-evaluating a foldable
/// comptime call fails — a genuine comptime fault.
pub fn fold_comptime(module: &mut IrModule) -> Result<(), String> {
    if module.comptime.is_empty() {
        return Ok(());
    }

    let comptime: HashSet<String> = module.comptime.iter().cloned().collect();

    // Which comptime symbols are foldable: non-extern, returning a width-bounded
    // integer (`i8/i16/i32/i64`, signed/unsigned) or `bool` — exactly what
    // `eval_const` (CB-T1) can read back. Params may be any int type; the call
    // site supplies correctly-typed constant args.
    let foldable: HashSet<String> = module
        .funcs
        .iter()
        .filter(|f| comptime.contains(&f.name) && !f.is_extern && is_foldable_ret(f.ret))
        .map(|f| f.name.clone())
        .collect();

    // 1+2. Collect fold jobs and apply them, iterating to a **fixpoint** so a
    //      comptime call whose args are themselves comptime calls — `Outer(Inner(3))`
    //      — folds inner-first: pass N folds `Inner` to a constant, pass N+1 then
    //      sees `Outer(<const>)` and folds it. Each pass collects every foldable
    //      call site whose args are all integer constants *now*, computes its value
    //      (memoized on (symbol, args) so identical sites evaluate once across the
    //      whole run), and rewrites it; repeat until a pass folds nothing. Bounded
    //      by the total instruction count (each pass folds ≥1 distinct site or
    //      stops), so it always terminates.
    let mut memo: HashMap<(String, Vec<i128>), (i64, IrType)> = HashMap::new();
    loop {
        // Collect this pass's jobs. Borrow `module` read-only here; apply after.
        // A job is (function index, instruction index, folded value, result type).
        let mut jobs: Vec<(usize, usize, i64, IrType)> = Vec::new();
        for fi in 0..module.funcs.len() {
            if comptime.contains(&module.funcs[fi].name) {
                continue; // don't fold inside comptime bodies — they're dropped below
            }
            for ii in 0..module.funcs[fi].insts.len() {
                // The call's own result type — the load-bearing CB-T6 fix: the
                // literal we fold to must match this width, not a hardcoded i64.
                let result_ty = module.funcs[fi].insts[ii].ty;
                let InstKind::Call { callee, args } = &module.funcs[fi].insts[ii].kind else {
                    continue;
                };
                if !foldable.contains(&callee.name) {
                    continue;
                }
                // Every argument must resolve to an integer constant to fold at
                // compile time. `arg_const` also resolves an SSA reference to a
                // *previously folded* inner comptime call (an identity `add v, 0`
                // we wrote in an earlier pass) back to its constant — this is the
                // inner-fold-first mechanism: once `Inner(3)` folds to `add 4, 0`,
                // the `Outer(Inner(3))` arg `%inner` reads back as the constant 4,
                // so the next pass folds `Outer` too.
                let consts: Option<Vec<(i128, IrType)>> = args
                    .iter()
                    .map(|a| arg_const(&module.funcs[fi].insts, a))
                    .collect();
                let Some(consts) = consts else { continue };

                let key = (callee.name.clone(), consts.iter().map(|(v, _)| *v).collect());
                let value = match memo.get(&key) {
                    Some(&(v, _)) => v,
                    None => {
                        // Evaluate at the call's own result width (CB-T1's
                        // width-correct `eval_const`), not the i64-only wrapper.
                        let v = eval_call(module, &callee.name, &consts, result_ty)?;
                        memo.insert(key, (v, result_ty));
                        v
                    }
                };
                jobs.push((fi, ii, value, result_ty));
            }
        }

        if jobs.is_empty() {
            break; // fixpoint: nothing left to fold
        }

        // Apply: rewrite each folded call into an identity `add v, 0` **of the
        // call's own result type** (CB-T6 width fix). The instruction id (and
        // every SSA use of it) stays valid — no operand rewiring or removal
        // needed; LLVM folds the `+0` away. Using the call's `InstData.ty` makes
        // the literal width match every use, so an `i32` fold verifies clean.
        for (fi, ii, v, ty) in jobs {
            module.funcs[fi].insts[ii].kind = InstKind::Bin {
                op: BinOp::Add,
                lhs: Value::int(v as i128, ty),
                rhs: Value::int(0, ty),
            };
            // The result type already equals `ty` (we read it from the call), so
            // no `.ty` update is needed — but keep it explicit for clarity.
            module.funcs[fi].insts[ii].ty = ty;
        }
    }

    // 3. Drop comptime functions no longer *reachable from ordinary code*. We
    //    compute reachability rather than a flat "is it called" so a comptime
    //    function kept alive only by its own recursion (its sole remaining caller
    //    is itself, after its outside call sites folded) is still dropped, while
    //    one reached from an unfolded call site — directly or through another
    //    comptime body — is kept (no dangling reference).
    let reachable = reachable_from_ordinary(module, &comptime);
    module
        .funcs
        .retain(|f| !comptime.contains(&f.name) || reachable.contains(&f.name));

    Ok(())
}

/// Evaluate `sym(args…)` at compile time by JIT-running a nullary wrapper that
/// makes exactly that call. The wrapper is added to a *clone* of `module` (so
/// the original is untouched and the comptime callee is still present), then
/// JIT-evaluated. `args` carry their own IR types, taken verbatim from the call
/// site, so the wrapper's call is as type-correct as the original.
///
/// `ret` is the call's **own** result type (the call instruction's `InstData.ty`,
/// CB-T6): the wrapper returns at that width, and [`eval_const`] reads the JIT
/// return value back masked + sign/zero-extended for that width (CB-T1). The
/// returned `i64` is the canonical value (e.g. an `i32 = -1` reads back as `-1`),
/// folded into a literal of type `ret` by the caller.
fn eval_call(
    module: &IrModule,
    sym: &str,
    args: &[(i128, IrType)],
    ret: IrType,
) -> Result<i64, String> {
    let mut m = module.clone();
    let mut wb = FunctionBuilder::new("$ct_eval", vec![], ret);
    let argv: Vec<Value> = args.iter().map(|(v, t)| Value::int(*v, *t)).collect();
    let r = wb.call(sym, argv, ret);
    wb.ret(Some(r));
    m.add_function(wb.finish());
    eval_const(&m, "$ct_eval", ret).map_err(Into::into)
}

/// The set of comptime symbols reachable from *ordinary* (non-comptime) code,
/// following the call graph. Roots are the symbols called directly from
/// non-comptime functions; from there we walk into comptime bodies. A comptime
/// function reached only via its own recursion is *not* in the result (nothing
/// outside it calls it), so it can be dropped after its outside calls folded.
fn reachable_from_ordinary(module: &IrModule, comptime: &HashSet<String>) -> HashSet<String> {
    // Per-function direct callees (for the transitive walk).
    let callees: HashMap<&str, Vec<&str>> = module
        .funcs
        .iter()
        .map(|f| {
            let cs = f
                .insts
                .iter()
                .filter_map(|i| match &i.kind {
                    InstKind::Call { callee, .. } => Some(callee.name.as_str()),
                    _ => None,
                })
                .collect();
            (f.name.as_str(), cs)
        })
        .collect();

    // Worklist seeded with everything ordinary code calls directly.
    let mut work: Vec<&str> = module
        .funcs
        .iter()
        .filter(|f| !comptime.contains(&f.name))
        .flat_map(|f| callees.get(f.name.as_str()).into_iter().flatten().copied())
        .collect();

    let mut reachable: HashSet<String> = HashSet::new();
    while let Some(sym) = work.pop() {
        if !reachable.insert(sym.to_string()) {
            continue; // already visited
        }
        if let Some(next) = callees.get(sym) {
            work.extend(next.iter().copied());
        }
    }
    reachable
}

#[cfg(test)]
mod tests {
    use super::fold_comptime;
    use newbf_ir::{BinOp, Callee, FunctionBuilder, InstKind, IrType, Module, Value};

    /// A comptime `answer() => 6*7` called by `main() => answer()` folds: the
    /// comptime function is dropped and the call becomes a constant.
    #[test]
    fn folds_a_nullary_comptime_call() {
        let mut a = FunctionBuilder::new("answer", vec![], IrType::I64);
        let v = a.bin(
            BinOp::Mul,
            Value::int(6, IrType::I64),
            Value::int(7, IrType::I64),
            IrType::I64,
        );
        a.ret(Some(v));

        let mut mf = FunctionBuilder::new("main", vec![], IrType::I64);
        let c = mf.call("answer", vec![], IrType::I64);
        mf.ret(Some(c));

        let mut m = Module::new("ct");
        m.add_function(a.finish());
        m.add_function(mf.finish());
        m.comptime.push("answer".to_string());

        fold_comptime(&mut m).unwrap();

        assert!(!m.funcs.iter().any(|f| f.name == "answer"));
        let main = m.funcs.iter().find(|f| f.name == "main").unwrap();
        assert!(!main.insts.iter().any(|i| matches!(
            &i.kind,
            InstKind::Call { callee, .. } if callee.name == "answer"
        )));
        assert!(main.insts.iter().any(|i| matches!(
            &i.kind,
            InstKind::Bin { op: BinOp::Add, lhs: Value::Const(_), .. }
        )));
    }

    /// A comptime function called with a *constant argument* — `dbl(21) => 21*2`
    /// — folds to 42 via the synthesized wrapper.
    #[test]
    fn folds_a_comptime_call_with_const_arg() {
        // dbl(x) => x * 2;  (param 0 is i64)
        let mut d = FunctionBuilder::new(
            "dbl",
            vec![newbf_ir::Param {
                name: Some("x".into()),
                ty: IrType::I64,
            }],
            IrType::I64,
        );
        let x = d.param(0);
        let v = d.bin(BinOp::Mul, x, Value::int(2, IrType::I64), IrType::I64);
        d.ret(Some(v));

        // main() => dbl(21);
        let mut mf = FunctionBuilder::new("main", vec![], IrType::I64);
        let c = mf.call("dbl", vec![Value::int(21, IrType::I64)], IrType::I64);
        mf.ret(Some(c));

        let mut m = Module::new("ct");
        m.add_function(d.finish());
        m.add_function(mf.finish());
        m.comptime.push("dbl".to_string());

        fold_comptime(&mut m).unwrap();

        assert!(!m.funcs.iter().any(|f| f.name == "dbl"));
        let main = m.funcs.iter().find(|f| f.name == "main").unwrap();
        // The call became `add 42, 0`.
        assert!(main.insts.iter().any(|i| matches!(
            &i.kind,
            InstKind::Bin { op: BinOp::Add, lhs: Value::Const(newbf_ir::Const::Int(42, _)), .. }
        )));
    }

    /// A comptime function called with a *non-constant* argument can't be folded;
    /// the call and the function are both kept (no dangling reference).
    #[test]
    fn keeps_comptime_called_with_runtime_arg() {
        let mut d = FunctionBuilder::new(
            "dbl",
            vec![newbf_ir::Param {
                name: Some("x".into()),
                ty: IrType::I64,
            }],
            IrType::I64,
        );
        let x = d.param(0);
        let v = d.bin(BinOp::Mul, x, Value::int(2, IrType::I64), IrType::I64);
        d.ret(Some(v));

        // main(p) => dbl(p);  — p is a parameter, not a constant.
        let mut mf = FunctionBuilder::new(
            "main",
            vec![newbf_ir::Param {
                name: Some("p".into()),
                ty: IrType::I64,
            }],
            IrType::I64,
        );
        let p = mf.param(0);
        let c = mf.call("dbl", vec![p], IrType::I64);
        mf.ret(Some(c));

        let mut m = Module::new("ct");
        m.add_function(d.finish());
        m.add_function(mf.finish());
        m.comptime.push("dbl".to_string());

        fold_comptime(&mut m).unwrap();

        // Not folded → `dbl` is still called and therefore kept.
        let main = m.funcs.iter().find(|f| f.name == "main").unwrap();
        assert!(main.insts.iter().any(|i| matches!(
            &i.kind,
            InstKind::Call { callee: Callee { name }, .. } if name == "dbl"
        )));
        assert!(m.funcs.iter().any(|f| f.name == "dbl"));
    }

    /// A module with no comptime functions is untouched.
    #[test]
    fn no_comptime_is_a_noop() {
        let mut mf = FunctionBuilder::new("main", vec![], IrType::I64);
        let c = mf.call("helper", vec![], IrType::I64);
        mf.ret(Some(c));
        let mut m = Module::new("app");
        m.add_function(mf.finish());
        let before = m.clone();
        fold_comptime(&mut m).unwrap();
        assert_eq!(m, before);
    }

    // ── CB-T6: widened-int folds + fold-width fix + inner-fold-first ───────────

    /// **The fold-width fix.** An `int32`-returning `[Comptime] F(int32 x) => x*x`
    /// called as `F(7)` folds to the constant 49 typed at the call's own width
    /// (`i32`), NOT a hardcoded `i64`. The rewritten `add 49, 0` carries `i32` (the
    /// call's `InstData.ty`) on both operands and on the result, so every SSA use
    /// stays width-consistent and the module verifies.
    #[test]
    fn folds_an_i32_comptime_call_to_an_i32_constant() {
        // F(x: i32) => x * x;
        let mut f = FunctionBuilder::new(
            "F",
            vec![newbf_ir::Param {
                name: Some("x".into()),
                ty: IrType::I32,
            }],
            IrType::I32,
        );
        let x = f.param(0);
        let v = f.bin(BinOp::Mul, x.clone(), x, IrType::I32);
        f.ret(Some(v));

        // main() => F(7)  (the call instruction's result type is i32)
        let mut mf = FunctionBuilder::new("main", vec![], IrType::I32);
        let c = mf.call("F", vec![Value::int(7, IrType::I32)], IrType::I32);
        mf.ret(Some(c));

        let mut m = Module::new("ct");
        m.add_function(f.finish());
        m.add_function(mf.finish());
        m.comptime.push("F".to_string());

        fold_comptime(&mut m).unwrap();

        // `F` is dropped (compile-time only) and the call became `add 49, 0` typed
        // at i32 — the width fix: the literal AND the instruction result are i32.
        assert!(!m.funcs.iter().any(|f| f.name == "F"));
        let main = m.funcs.iter().find(|f| f.name == "main").unwrap();
        let folded = main
            .insts
            .iter()
            .find(|i| matches!(&i.kind, InstKind::Bin { op: BinOp::Add, .. }))
            .expect("the F(7) call folded to an add");
        assert_eq!(folded.ty, IrType::I32, "folded instruction is i32-typed");
        assert!(
            matches!(
                &folded.kind,
                InstKind::Bin {
                    op: BinOp::Add,
                    lhs: Value::Const(newbf_ir::Const::Int(49, IrType::I32)),
                    rhs: Value::Const(newbf_ir::Const::Int(0, IrType::I32)),
                }
            ),
            "the folded literal is an i32 49 (+0 i32), got {:?}",
            folded.kind
        );
    }

    /// **Inner-fold-first / fixpoint.** `Outer(Inner(3))` where both are comptime
    /// folds bottom-up: the first pass folds `Inner(3)` to a constant, the next
    /// pass sees `Outer(<const>)` and folds it. Both comptime functions are
    /// dropped and `main` is left with a single literal — proving the collect/apply
    /// loop iterates to a fixpoint rather than folding only one level.
    #[test]
    fn folds_nested_comptime_calls_inner_first() {
        // Inner(x) => x + 1;  (i64)
        let mut inner = FunctionBuilder::new(
            "Inner",
            vec![newbf_ir::Param {
                name: Some("x".into()),
                ty: IrType::I64,
            }],
            IrType::I64,
        );
        let ix = inner.param(0);
        let iv = inner.bin(BinOp::Add, ix, Value::int(1, IrType::I64), IrType::I64);
        inner.ret(Some(iv));

        // Outer(y) => y * 10;  (i64)
        let mut outer = FunctionBuilder::new(
            "Outer",
            vec![newbf_ir::Param {
                name: Some("y".into()),
                ty: IrType::I64,
            }],
            IrType::I64,
        );
        let oy = outer.param(0);
        let ov = outer.bin(BinOp::Mul, oy, Value::int(10, IrType::I64), IrType::I64);
        outer.ret(Some(ov));

        // main() => Outer(Inner(3))   →  (3+1)*10 = 40
        let mut mf = FunctionBuilder::new("main", vec![], IrType::I64);
        let inner_call = mf.call("Inner", vec![Value::int(3, IrType::I64)], IrType::I64);
        let outer_call = mf.call("Outer", vec![inner_call], IrType::I64);
        mf.ret(Some(outer_call));

        let mut m = Module::new("ct");
        m.add_function(inner.finish());
        m.add_function(outer.finish());
        m.add_function(mf.finish());
        m.comptime.push("Inner".to_string());
        m.comptime.push("Outer".to_string());

        fold_comptime(&mut m).unwrap();

        // Both comptime functions are gone and main holds the single literal 40.
        assert!(!m.funcs.iter().any(|f| f.name == "Inner"));
        assert!(!m.funcs.iter().any(|f| f.name == "Outer"));
        let main = m.funcs.iter().find(|f| f.name == "main").unwrap();
        assert!(
            !main.insts.iter().any(|i| matches!(
                &i.kind,
                InstKind::Call { callee: Callee { name }, .. } if name == "Inner" || name == "Outer"
            )),
            "no comptime call survives — both folded"
        );
        assert!(
            main.insts.iter().any(|i| matches!(
                &i.kind,
                InstKind::Bin { op: BinOp::Add, lhs: Value::Const(newbf_ir::Const::Int(40, _)), .. }
            )),
            "the nested fold collapsed to the literal 40"
        );
    }
}
