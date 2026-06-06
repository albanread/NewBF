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
//! included), which already type-checked. Foldable today = an `i64`-returning,
//! non-extern comptime function called with all-integer-constant args (the
//! common `const X = f(consts)` shape); a non-constant-arg call site is left as
//! an ordinary call (and its function kept), rather than failing the compile.

use std::collections::{HashMap, HashSet};

use newbf_ir::{
    BinOp, Const, FunctionBuilder, InstKind, IrType, Module as IrModule, Value,
};

use crate::eval::eval_const_i64;

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

    // Which comptime symbols are foldable: non-extern, `i64`-returning. Params
    // may be any int type — the call site supplies correctly-typed constant args.
    let foldable: HashSet<String> = module
        .funcs
        .iter()
        .filter(|f| comptime.contains(&f.name) && !f.is_extern && f.ret == IrType::I64)
        .map(|f| f.name.clone())
        .collect();

    // 1. Collect fold jobs: every call (in ordinary code) to a foldable comptime
    //    function whose args are all integer constants. Compute each value via a
    //    JIT'd wrapper, memoized on (symbol, constant args) so identical sites
    //    evaluate once. Borrow `module` read-only here; apply the rewrites after.
    let mut memo: HashMap<(String, Vec<i128>), i64> = HashMap::new();
    let mut jobs: Vec<(usize, usize, i64)> = Vec::new();
    for fi in 0..module.funcs.len() {
        if comptime.contains(&module.funcs[fi].name) {
            continue; // don't fold inside comptime bodies — they're dropped below
        }
        for ii in 0..module.funcs[fi].insts.len() {
            let InstKind::Call { callee, args } = &module.funcs[fi].insts[ii].kind else {
                continue;
            };
            if !foldable.contains(&callee.name) {
                continue;
            }
            // Every argument must be an integer constant to fold at compile time.
            let consts: Option<Vec<(i128, IrType)>> = args
                .iter()
                .map(|a| match a {
                    Value::Const(Const::Int(v, t)) => Some((*v, *t)),
                    _ => None,
                })
                .collect();
            let Some(consts) = consts else { continue };

            let key = (callee.name.clone(), consts.iter().map(|(v, _)| *v).collect());
            let value = match memo.get(&key) {
                Some(&v) => v,
                None => {
                    let v = eval_call(module, &callee.name, &consts)?;
                    memo.insert(key, v);
                    v
                }
            };
            jobs.push((fi, ii, value));
        }
    }

    // 2. Apply: rewrite each folded call into an identity `add v, 0` of the same
    //    `i64` result type. The instruction id (and every SSA use of it) stays
    //    valid — no operand rewiring or removal needed; LLVM folds the `+0` away.
    for (fi, ii, v) in jobs {
        module.funcs[fi].insts[ii].kind = InstKind::Bin {
            op: BinOp::Add,
            lhs: Value::int(v as i128, IrType::I64),
            rhs: Value::int(0, IrType::I64),
        };
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
fn eval_call(module: &IrModule, sym: &str, args: &[(i128, IrType)]) -> Result<i64, String> {
    let mut m = module.clone();
    let mut wb = FunctionBuilder::new("$ct_eval", vec![], IrType::I64);
    let argv: Vec<Value> = args.iter().map(|(v, t)| Value::int(*v, *t)).collect();
    let r = wb.call(sym, argv, IrType::I64);
    wb.ret(Some(r));
    m.add_function(wb.finish());
    eval_const_i64(&m, "$ct_eval")
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
}
