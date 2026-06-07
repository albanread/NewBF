//! The comptime evaluation core: JIT a function and call it at compile time.

use newbf_ir::IrType;
use newbf_ir::Module as IrModule;
use newbf_llvm::OrcJit;

/// A result type the comptime evaluator cannot interpret. JIT-running a
/// function with such a return type would either fail to materialize (the
/// `__real@` float-constant gap — see project memory / design §3.5) or yield a
/// value with no sound `i64` interpretation (pointers, aggregates). Surfaced as
/// a typed `Err` so the caller produces a diagnostic instead of a miscompile.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum EvalError {
    /// `name` was not present in the JIT'd module.
    SymbolNotFound { name: String },
    /// The result type is not a width-bounded integer/bool the JIT-return ABI
    /// can be read as an `i64`. `ret` is the offending type's mnemonic.
    Unsupported { name: String, ret: String },
    /// An integer wider than 64 bits (none exist in the current IR, but the
    /// width interpretation only covers `bits <= 64`).
    WidthTooLarge { name: String, bits: u16 },
    /// A lower-level JIT failure (module build / LLJIT creation), forwarded.
    Jit(String),
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EvalError::SymbolNotFound { name } => {
                write!(f, "comptime: symbol `{name}` not found in JIT'd module")
            }
            EvalError::Unsupported { name, ret } => write!(
                f,
                "comptime: result type `{ret}` of `{name}` is not evaluable \
                 (only width-bounded integers and bool are supported; \
                 float return types hit the ORC `__real@` constant gap)"
            ),
            EvalError::WidthTooLarge { name, bits } => write!(
                f,
                "comptime: integer width {bits} of `{name}` exceeds the 64-bit \
                 comptime-eval limit"
            ),
            EvalError::Jit(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for EvalError {}

impl From<EvalError> for String {
    fn from(e: EvalError) -> String {
        e.to_string()
    }
}

/// JIT-compile `module` and call its nullary function `name`, interpreting the
/// return value at the **width and signedness** of `ret` — i.e. evaluate it *at
/// compile time*. This is the beating heart of comptime: the same
/// IR→LLVM→ORC pipeline the application uses, run during compilation.
///
/// # Width interpretation (the load-bearing detail)
/// The result is read by transmuting the entry point to a nullary `i64 ()`
/// function and calling it: on Win64 the integer return lives in RAX. For a
/// sub-64-bit return the callee leaves RAX's **upper bits undefined**, so the
/// raw machine word must be **masked to `bits` first**, then **sign-extended
/// (signed) or zero-extended (unsigned)** to the canonical `i64` the caller
/// stores. Concretely:
/// - `i32 = -1`   → mask to `0xFFFF_FFFF` → sign-extend → `-1i64`
/// - `u8  = 250`  → mask to `0xFA`        → zero-extend → `250i64` (not `-6`)
/// - `i8  = -7`   → mask to `0xF9`        → sign-extend → `-7i64`
/// - `bool`       → mask to `0x1`         → `0`/`1`
///
/// # Supported result types
/// - [`IrType::Int { bits, signed }`] with `bits <= 64`, and [`IrType::Bool`].
/// - Everything else — `Float`/`Ptr`/`Ref`/`Struct`/`Void` — returns a typed
///   [`EvalError::Unsupported`] *without JIT-running the function*. Float in
///   particular must not reach the JIT: the ORC/RTDyld linker cannot resolve
///   `__real@` float-constant relocations (project memory), so attempting it
///   would fail to materialize rather than fabricate a value.
///
/// # Contract
/// `name` must be a defined, nullary function in `module` whose actual return
/// type matches `ret`. Argument marshalling is handled by the fold pass's
/// wrapper (`fold.rs`), which always evaluates a synthesized nullary `i64`
/// wrapper.
///
/// # Safety / bounded execution
/// The JIT'd code runs natively in-process. For now a fault propagates (the
/// `newbf-runtime` SEH crash dump fires); the bounded-execution +
/// fault-to-diagnostic recovery boundary is a follow-on, so pass only
/// trusted, terminating constant functions.
pub fn eval_const(module: &IrModule, name: &str, ret: IrType) -> Result<i64, EvalError> {
    // Decide how to interpret the return value *before* touching the JIT, so
    // unsupported result types (notably floats — the `__real@` gap) never reach
    // materialization and can't crash the compiler.
    let (bits, signed) = match ret {
        IrType::Bool => (1u16, false),
        IrType::Int { bits, signed } => {
            if bits > 64 {
                return Err(EvalError::WidthTooLarge {
                    name: name.to_string(),
                    bits,
                });
            }
            (bits, signed)
        }
        IrType::Void
        | IrType::Float { .. }
        | IrType::Ptr
        | IrType::Struct(_)
        | IrType::Ref(_) => {
            return Err(EvalError::Unsupported {
                name: name.to_string(),
                ret: ret.mnemonic(),
            });
        }
    };

    let jit = OrcJit::from_ir(module).map_err(EvalError::Jit)?;
    let addr = jit
        .lookup(name)
        .ok_or_else(|| EvalError::SymbolNotFound {
            name: name.to_string(),
        })?;
    // SAFETY: by contract `name` is a nullary function returning an integer/bool
    // of width `bits`; `addr` is its entry point in JIT'd memory, which stays
    // mapped while `jit` is alive — it is, until this function returns *after*
    // the call completes. We transmute to `i64 ()` and read the raw return word
    // (RAX on Win64); the upper bits are undefined for sub-64-bit returns, which
    // is exactly why we mask before extending below.
    let f: extern "C" fn() -> i64 = unsafe { std::mem::transmute(addr) };
    let raw = f() as u64;

    Ok(extend_to_i64(raw, bits, signed))
}

/// Mask `raw` to its low `bits` bits, then extend to a canonical `i64`:
/// sign-extend if `signed`, else zero-extend. `bits` is in `1..=64`.
///
/// This is the correctness core: masking discards the Win64 undefined-upper-bits
/// garbage, and the per-signedness extension makes `u8 = 250` read as `250`
/// (not `-6`) and `i32 = -1` read as `-1` (not `4294967295`).
fn extend_to_i64(raw: u64, bits: u16, signed: bool) -> i64 {
    debug_assert!((1..=64).contains(&bits));
    if bits >= 64 {
        // No masking/extension needed; the full 64-bit word is the value.
        return raw as i64;
    }
    let mask = (1u64 << bits) - 1;
    let masked = raw & mask;
    if signed {
        let sign_bit = 1u64 << (bits - 1);
        if masked & sign_bit != 0 {
            // Set the high (64 - bits) bits to 1 to sign-extend.
            (masked | !mask) as i64
        } else {
            masked as i64
        }
    } else {
        masked as i64
    }
}

/// JIT-evaluate the nullary, `i64`-returning function `name` in `module`.
///
/// A thin wrapper over [`eval_const`] at [`IrType::I64`] — kept for the existing
/// callers (the fold pass) so their behavior is unchanged. The fold pass folds
/// `i64`-returning comptime calls today; the widened [`eval_const`] is consumed
/// by the fold-width work (CB-T6).
pub fn eval_const_i64(module: &IrModule, name: &str) -> Result<i64, String> {
    eval_const(module, name, IrType::I64).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::{eval_const, eval_const_i64, extend_to_i64, EvalError};
    use newbf_ir::{BinOp, FunctionBuilder, IrType, Module, Value};

    // ── existing i64 wrapper tests (behavior-preserving) ──────────────────

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

    // ── pure mask-then-extend unit tests (no JIT) ─────────────────────────

    #[test]
    fn extend_masks_then_sign_or_zero_extends() {
        // i32 = -1: upper RAX bits garbage; mask to 0xFFFF_FFFF, sign-extend.
        assert_eq!(extend_to_i64(0xDEAD_BEEF_FFFF_FFFF, 32, true), -1);
        // u32 of the same low word, unsigned → 0xFFFF_FFFF = 4294967295.
        assert_eq!(extend_to_i64(0xDEAD_BEEF_FFFF_FFFF, 32, false), 4_294_967_295);
        // i8 = -7 (0xF9): sign-extend regardless of upper garbage.
        assert_eq!(extend_to_i64(0xAB_CD_EF_F9, 8, true), -7);
        // u8 = 250 (0xFA): zero-extend → 250, NOT -6.
        assert_eq!(extend_to_i64(0xFF_FF_FF_FA, 8, false), 250);
        // i16 = -1 (0xFFFF): sign-extend.
        assert_eq!(extend_to_i64(0x1234_FFFF, 16, true), -1);
        // u16 = 65535: zero-extend.
        assert_eq!(extend_to_i64(0x1234_FFFF, 16, false), 65535);
        // bool 1 with garbage upper bits → 1.
        assert_eq!(extend_to_i64(0xFF_FF_FF_FF, 1, false), 1);
        // bool 0 → 0.
        assert_eq!(extend_to_i64(0xFF_FF_FF_FE, 1, false), 0);
        // 64-bit identity (signed): full word passes through.
        assert_eq!(extend_to_i64(0xFFFF_FFFF_FFFF_FFFF, 64, true), -1);
        // 64-bit identity (unsigned): same raw word reinterpreted.
        assert_eq!(extend_to_i64(0x0000_0000_0000_002A, 64, false), 42);
    }

    // ── width-correct JIT eval tests ──────────────────────────────────────

    /// Helper: build a nullary fn `name` that returns the constant `v` typed at
    /// `ty`, JIT it, and read it back at `ty`.
    fn eval_int_const(name: &str, v: i128, ty: IrType) -> Result<i64, EvalError> {
        let mut f = FunctionBuilder::new(name, vec![], ty);
        f.ret(Some(Value::int(v, ty)));
        let mut m = Module::new("ct");
        m.add_function(f.finish());
        eval_const(&m, name, ty)
    }

    #[test]
    fn i32_negative_sign_extends() {
        // i32 = -1 → -1i64 (not 0x0000_0000_FFFF_FFFF).
        assert_eq!(eval_int_const("ineg", -1, IrType::I32).unwrap(), -1);
    }

    #[test]
    fn i8_negative_sign_extends() {
        // i8 = -7 → -7i64.
        assert_eq!(eval_int_const("i8neg", -7, IrType::I8).unwrap(), -7);
    }

    #[test]
    fn u8_near_max_zero_extends() {
        // u8 = 250 → 250i64 (NOT a negative number).
        assert_eq!(eval_int_const("u8max", 250, IrType::U8).unwrap(), 250);
    }

    #[test]
    fn i16_roundtrips() {
        // i16 = -300 → -300i64.
        assert_eq!(eval_int_const("i16", -300, IrType::I16).unwrap(), -300);
        // u16 = 65000 → 65000i64 (zero-extend, not sign).
        let u16t = IrType::Int {
            bits: 16,
            signed: false,
        };
        assert_eq!(eval_int_const("u16", 65000, u16t).unwrap(), 65000);
    }

    #[test]
    fn i64_is_identity() {
        // i64 = a big negative value passes through unchanged.
        assert_eq!(
            eval_int_const("i64", -1_000_000_000_000, IrType::I64).unwrap(),
            -1_000_000_000_000
        );
    }

    #[test]
    fn bool_evaluates_to_zero_or_one() {
        // true → 1
        let mut t = FunctionBuilder::new("btrue", vec![], IrType::Bool);
        t.ret(Some(Value::bool(true)));
        let mut mt = Module::new("ct");
        mt.add_function(t.finish());
        assert_eq!(eval_const(&mt, "btrue", IrType::Bool).unwrap(), 1);

        // false → 0
        let mut fa = FunctionBuilder::new("bfalse", vec![], IrType::Bool);
        fa.ret(Some(Value::bool(false)));
        let mut mf = Module::new("ct");
        mf.add_function(fa.finish());
        assert_eq!(eval_const(&mf, "bfalse", IrType::Bool).unwrap(), 0);
    }

    // ── typed-Err result types (no JIT attempt, no crash) ─────────────────

    /// A float result type returns `Err(Unsupported)` *without* JIT-running the
    /// function — guarding the `__real@` constant-pool gap (project memory).
    #[test]
    fn float_result_is_unsupported_err_no_jit() {
        // Build a float-returning fn; eval must NOT attempt to JIT/materialize.
        let mut f = FunctionBuilder::new("pi", vec![], IrType::F64);
        f.ret(Some(Value::float(3.5, IrType::F64)));
        let mut m = Module::new("ct");
        m.add_function(f.finish());

        let err = eval_const(&m, "pi", IrType::F64).unwrap_err();
        assert!(
            matches!(err, EvalError::Unsupported { ref ret, .. } if ret == "f64"),
            "expected Unsupported(f64), got {err:?}"
        );
    }

    #[test]
    fn ptr_and_void_result_are_unsupported_err() {
        // The type-check happens before any JIT attempt, so an empty module is
        // fine — we only assert the type gate rejects ptr/void.
        let m = Module::new("ct");
        assert!(matches!(
            eval_const(&m, "p", IrType::Ptr).unwrap_err(),
            EvalError::Unsupported { .. }
        ));
        assert!(matches!(
            eval_const(&m, "v", IrType::Void).unwrap_err(),
            EvalError::Unsupported { .. }
        ));
        assert!(matches!(
            eval_const(&m, "s", IrType::Struct(newbf_ir::StructId(0))).unwrap_err(),
            EvalError::Unsupported { .. }
        ));
    }
}
