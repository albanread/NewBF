// expect: 100
// MX-T4.5 — exercises the `.Err → default` arm of a generic enum instance
// `switch (this)` method AS A VALUE (the happy-path test never reaches it). The
// `.Err` arm returns `default`, which must lower to the zeroed `T` (here int32 0),
// not an unresolved-ident `undef`. Also proves a SECOND, distinct monomorphization
// `Result<int32,int32>` (different `E` than the int32/bool sibling) keys its own
// layout — monomorph keys don't collide.
//   ok  = Result<int32,int32>.Ok(100).Unwrap()  → .Ok(var v) → 100
//   err = Result<int32,int32>.Err(7).Unwrap()   → .Err arm  → default(int32) = 0
//   r = 100 + 0 = 100
enum Result<T, E> {
	case Ok(T value);
	case Err(E error);

	public T Unwrap() {
		switch (this) {
		case .Ok(var v): return v;
		case .Err(var e): return default;
		}
	}
}
class Program {
	public static int32 Main() {
		Result<int32, int32> ok = Result<int32, int32>.Ok(100);
		Result<int32, int32> err = Result<int32, int32>.Err(7);
		return ok.Unwrap() + err.Unwrap(); // 100 + 0 = 100
	}
}
