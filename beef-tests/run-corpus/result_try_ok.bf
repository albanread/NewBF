// expect: 42
// MX-T6 — the v1 `Try!` happy path (the mixins capstone). `Try!` is a CONCRETE
// (non-generic) corpus mixin with a `var res` param (VarInfer, bound once) whose
// body is the v1 shape from mixins.md §3.7:
//
//     mixin Try(var res) {
//         if (res case .Err(let e)) return .Err(e);  // escape → caller's Result
//         res.Value                                   // block-trailing yield = Ok payload
//     }
//
// It composes MX-T3 (splice + block-trailing yield), MX-T4 (return escapes to the
// caller, coerced to the caller's `Result<T,E>` ret_ty), and MX-T5 (the prelude
// `Result<int32,bool>` with `.Value`). On the happy path every `Try!` hits `.Ok`,
// so the `if` is false, the body yields `res.Value` (the Ok payload), and `Compute`
// reaches its `return .Ok(a + b)`.
//   a = Try!(MakeOk(40)) → 40
//   b = Try!(MakeOk(2))  → 2
//   Compute() → .Ok(42); Main unwraps → 42
class Program {
	// A concrete `var`-param mixin: bound ONCE to the evaluated arg (a Result),
	// only READ (never assigned) → the simple VarInfer form v1 supports.
	static mixin Try(var res) {
		if (res case .Err(let e)) {
			return .Err(e);   // escapes to Compute's Result<int32,bool> return
		}
		res.Value             // trailing bare expr → yields the .Ok payload
	}

	static Result<int32, bool> MakeOk(int32 v) {
		return .Ok(v);
	}

	static Result<int32, bool> Compute() {
		int32 a = Try!(MakeOk(40));   // 40 (the Ok payload)
		int32 b = Try!(MakeOk(2));    // 2
		return .Ok(a + b);            // .Ok(42)
	}

	public static int32 Main() {
		Result<int32, bool> r = Compute();
		return r.Value;   // 42
	}
}
