// expect: 7
// MX-T6 — the v1 `Try!` ERROR path: the same-error ESCAPE (the capstone's whole
// point). `Try!(MakeErr(7))` hits `.Err`, so the body's `if (res case .Err(let e))`
// is TRUE and runs `return .Err(e);` — a `return` STATEMENT spliced into `Compute`.
// Because the splice reuses the live Lowerer, that `return` lowers against
// `Compute`'s `Result<int32,bool>` ret_ty: `.Err(e)` target-types to the CALLER's
// Result and EARLY-RETURNS `Compute`, propagating the error code outward. The
// `int32 b = …` and `return .Ok(…)` after it are DEAD.
//
// This pins the SAME-ERROR escape reaching the caller: the `e` bound in `Compute`'s
// `Try!` splice is the bool-payloadless `int32` error code, reconstructed as the
// caller's `.Err(e)` and returned. `Main` then switch-matches the returned `.Err`
// and yields the carried code, proving the escape value survived.
//   a = Try!(MakeOk(99)) → 99   (Ok; control continues)
//   _ = Try!(MakeErr(7)) → .Err(7) fires → Compute early-returns .Err(7)
//   the trailing `return .Ok(...)` is never reached
//   Main matches .Err(let code) → returns 7
class Program {
	static mixin Try(var res) {
		if (res case .Err(let e)) {
			return .Err(e);   // EARLY return: escapes Compute with the SAME error
		}
		res.Value
	}

	static Result<int32, int32> MakeOk(int32 v) {
		return .Ok(v);
	}

	static Result<int32, int32> MakeErr(int32 code) {
		return .Err(code);
	}

	static Result<int32, int32> Compute() {
		int32 a = Try!(MakeOk(99));   // 99 (Ok) — continues
		int32 b = Try!(MakeErr(7));   // .Err(7) → Compute returns .Err(7) HERE
		return .Ok(a + b);            // DEAD — never reached on the .Err path
	}

	public static int32 Main() {
		Result<int32, int32> r = Compute();
		switch (r) {
		case .Ok(let v): return v - 1000;   // would prove the escape did NOT fire
		case .Err(let code): return code;   // 7 — the escaped error code
		}
	}
}
