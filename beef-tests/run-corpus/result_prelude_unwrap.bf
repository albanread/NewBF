// expect: 42
// MX-T5 — the happy path against the PRELUDE `Result<T, E>` (corlib `Result.bf`),
// NOT a locally-declared one. This program declares NO `Result` of its own, so it
// resolves entirely to the canonical prelude type — the proof that `Result.bf`
// rides the prelude and a corpus program can construct + read it like any other
// corlib ADT (`Option<T>` rides the prelude the same way).
//   ok  = Result<int32,bool>.Ok(42)            constructed qualified
//   r1  = ok.Unwrap()  → switch(this) → .Ok(var v) → 42   (the prelude method)
//   r2  = ok.Value     → the prelude property → 42         (same .Err→default arm)
//   err = Result<int32,bool>.Err(true)
//   r3  = err.Unwrap() → .Err arm → default(int32) = 0     (NO FatalError, v1)
//   r   = 42 + 42 + 0 - 42 = 42
class Program {
	public static int32 Main() {
		Result<int32, bool> ok = Result<int32, bool>.Ok(42);
		Result<int32, bool> err = Result<int32, bool>.Err(true);
		int32 r1 = ok.Unwrap();   // 42
		int32 r2 = ok.Value;      // 42 (the property)
		int32 r3 = err.Unwrap();  // 0  (.Err → default)
		return r1 + r2 + r3 - 42; // 42 + 42 + 0 - 42 = 42
	}
}
