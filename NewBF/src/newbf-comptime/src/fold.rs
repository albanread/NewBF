//! Comptime call-site folding: evaluate `[Comptime]` functions at compile time
//! and replace their call sites in ordinary code with the resulting literal.
//!
//! This is the second half of comptime (the first, `eval_const_i64`, evaluates a
//! single function). A `[Comptime]` function is compile-time-only: it must not
//! survive into the final program. `fold_comptime` walks `module.comptime` (the
//! symbols `newbf-sema` marked), JIT-evaluates each foldable one against the
//! whole module, rewrites every nullary call to it into an integer literal, and
//! then drops the comptime functions entirely.
//!
//! Foldable today = a nullary, `i64`-returning, non-extern function (the same
//! shape `eval_const_i64` accepts). Argument marshalling and wider/non-integer
//! return types arrive with the breadth work; a non-foldable comptime function
//! is left in place (its call sites stay ordinary calls) rather than failing the
//! compile.

use std::collections::{HashMap, HashSet};

use newbf_ir::{BinOp, InstKind, IrType, Module as IrModule, Value};

use crate::eval::eval_const_i64;

/// Evaluate the `[Comptime]` functions in `module` and fold their call sites
/// into literals, then remove the comptime functions (compile-time only).
///
/// A no-op when `module.comptime` is empty (the common case), so ordinary
/// programs pay nothing. Returns `Err` only if JIT-evaluating a foldable
/// comptime function fails — a genuine comptime fault.
pub fn fold_comptime(module: &mut IrModule) -> Result<(), String> {
    if module.comptime.is_empty() {
        return Ok(());
    }

    // The set of comptime symbols, owned so we can mutate `module.funcs` freely.
    let comptime: HashSet<String> = module.comptime.iter().cloned().collect();

    // 1. Evaluate each *foldable* comptime function against the whole module
    //    (so any callee it reaches is present, exactly like `eval_const_i64`).
    let mut values: HashMap<String, i64> = HashMap::new();
    for name in &comptime {
        let foldable = module.funcs.iter().any(|f| {
            &f.name == name && !f.is_extern && f.params.is_empty() && f.ret == IrType::I64
        });
        if !foldable {
            continue;
        }
        let v = eval_const_i64(module, name)?;
        values.insert(name.clone(), v);
    }

    // 2. Rewrite call sites: a nullary `call @sym` to a folded comptime function
    //    becomes the literal it evaluated to. We materialize the constant as an
    //    identity `add v, 0` so the instruction id (and every SSA use of it)
    //    stays valid — no operand rewiring or instruction removal needed; LLVM
    //    constant-folds it away. Skip comptime functions themselves (dropped in
    //    step 3, and we don't want a comptime body folding against itself).
    for f in &mut module.funcs {
        if comptime.contains(&f.name) {
            continue;
        }
        for inst in &mut f.insts {
            if let InstKind::Call { callee, args } = &inst.kind
                && args.is_empty()
                && let Some(&v) = values.get(&callee.name)
            {
                inst.kind = InstKind::Bin {
                    op: BinOp::Add,
                    lhs: Value::int(v as i128, IrType::I64),
                    rhs: Value::int(0, IrType::I64),
                };
                // `inst.ty` is already the call's `i64` return type — leave it.
            }
        }
    }

    // 3. Drop the comptime functions: they exist only at compile time.
    module.funcs.retain(|f| !comptime.contains(&f.name));

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::fold_comptime;
    use newbf_ir::{BinOp, FunctionBuilder, InstKind, IrType, Module, Value};

    /// Build a module with a comptime `answer() => 6*7` and a `main() => answer()`,
    /// fold it, and assert the comptime function is gone and `main` no longer
    /// calls it (the call became a constant).
    #[test]
    fn folds_a_nullary_comptime_call() {
        // comptime answer() => 6 * 7;
        let mut a = FunctionBuilder::new("answer", vec![], IrType::I64);
        let v = a.bin(
            BinOp::Mul,
            Value::int(6, IrType::I64),
            Value::int(7, IrType::I64),
            IrType::I64,
        );
        a.ret(Some(v));

        // main() => answer();
        let mut mf = FunctionBuilder::new("main", vec![], IrType::I64);
        let c = mf.call("answer", vec![], IrType::I64);
        mf.ret(Some(c));

        let mut m = Module::new("ct");
        m.add_function(a.finish());
        m.add_function(mf.finish());
        m.comptime.push("answer".to_string());

        fold_comptime(&mut m).unwrap();

        // The comptime function is dropped.
        assert!(!m.funcs.iter().any(|f| f.name == "answer"));
        // `main` survives and no longer contains a call to `answer`.
        let main = m.funcs.iter().find(|f| f.name == "main").unwrap();
        assert!(!main.insts.iter().any(|i| matches!(
            &i.kind,
            InstKind::Call { callee, .. } if callee.name == "answer"
        )));
        // The call became an identity-add materializing 42.
        assert!(main.insts.iter().any(|i| matches!(
            &i.kind,
            InstKind::Bin { op: BinOp::Add, lhs: Value::Const(_), .. }
        )));
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
