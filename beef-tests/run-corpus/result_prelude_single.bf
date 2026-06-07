// expect: 55
// MX-T5 — the single-type-param convenience `Result<T>` from the PRELUDE
// (`Result.bf`): `Ok(T)` or a payloadless `Err`. No local declaration — this
// resolves to the canonical prelude `Result<T>` (a DISTINCT arity from
// `Result<T, E>`; the generic-decl index now keys by (name, arity), so both
// coexist). Proves the 1-arg arity monomorphizes + reads its payload.
//   ok  = Result<int32>.Ok(55)
//   r1  = ok.Value     → .Ok(var v) → 55
//   err = Result<int32>.Err
//   r2  = err.Unwrap() → .Err arm → default(int32) = 0
//   r   = 55 + 0 = 55
class Program {
	public static int32 Main() {
		Result<int32> ok = Result<int32>.Ok(55);
		Result<int32> err = Result<int32>.Err;
		int32 r1 = ok.Value;      // 55
		int32 r2 = err.Unwrap();  // 0 (.Err → default)
		return r1 + r2;           // 55
	}
}
