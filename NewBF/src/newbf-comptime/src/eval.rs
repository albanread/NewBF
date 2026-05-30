//! The comptime evaluation core: JIT a function and call it at compile time.

use newbf_ir::Module as IrModule;
use newbf_llvm::OrcJit;

/// JIT-compile `module` and call its nullary, `i64`-returning function
/// `name`, returning the value it computes — i.e. evaluate it *at compile
/// time*. This is the beating heart of comptime: the same IR→LLVM→ORC
/// pipeline the application uses, run during compilation.
///
/// # Contract
/// `name` must be a defined function in `module` with signature `i64 ()`
/// (no parameters). Argument marshalling and wider return types arrive with
/// the breadth work; pinning the shape here keeps the call sound.
///
/// # Safety / bounded execution
/// The JIT'd code runs natively in-process. For now a fault propagates (the
/// `newbf-runtime` SEH crash dump fires); the bounded-execution +
/// fault-to-diagnostic recovery boundary is a follow-on, so pass only
/// trusted, terminating constant functions.
pub fn eval_const_i64(module: &IrModule, name: &str) -> Result<i64, String> {
    let jit = OrcJit::from_ir(module)?;
    let addr = jit
        .lookup(name)
        .ok_or_else(|| format!("comptime: symbol `{name}` not found in JIT'd module"))?;
    // SAFETY: by contract `name` is a nullary `i64 ()` function; `addr` is its
    // entry point in JIT'd memory, which stays mapped while `jit` is alive —
    // it is, until this function returns *after* the call completes.
    let f: extern "C" fn() -> i64 = unsafe { std::mem::transmute(addr) };
    Ok(f())
}

#[cfg(test)]
mod tests {
    use super::eval_const_i64;
    use newbf_ir::{BinOp, FunctionBuilder, IrType, Module, Value};

    #[test]
    fn evaluates_constant_at_compile_time() {
        // const answer() => 6 * 7;
        let mut f = FunctionBuilder::new("answer", vec![], IrType::I64);
        let v = f.bin(
            BinOp::Mul,
            Value::int(6, IrType::I64),
            Value::int(7, IrType::I64),
            IrType::I64,
        );
        f.ret(Some(v));
        let mut m = Module::new("ct");
        m.add_function(f.finish());

        assert_eq!(eval_const_i64(&m, "answer").unwrap(), 42);
    }

    #[test]
    fn folds_a_small_pipeline() {
        // const f() => (10 + 5) * 2 - 1;  →  29
        let mut f = FunctionBuilder::new("f", vec![], IrType::I64);
        let add = f.bin(
            BinOp::Add,
            Value::int(10, IrType::I64),
            Value::int(5, IrType::I64),
            IrType::I64,
        );
        let mul = f.bin(BinOp::Mul, add, Value::int(2, IrType::I64), IrType::I64);
        let sub = f.bin(BinOp::Sub, mul, Value::int(1, IrType::I64), IrType::I64);
        f.ret(Some(sub));
        let mut m = Module::new("ct");
        m.add_function(f.finish());

        assert_eq!(eval_const_i64(&m, "f").unwrap(), 29);
    }

    #[test]
    fn unknown_symbol_is_err() {
        let m = Module::new("empty");
        assert!(eval_const_i64(&m, "nope").is_err());
    }
}
