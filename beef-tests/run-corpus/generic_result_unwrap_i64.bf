// expect: 77
// MX-T4.5 — the SAME generic enum instance `switch (this)` `Unwrap` monomorphized
// for a DIFFERENT payload width: `Result<int64,bool>`. Proves the generic-enum-
// instance-method monomorphization isn't hard-coded to int32 — the `.Ok(var v)`
// binding reads an int64 payload slot, and the result narrows to the i32 return.
// `Result<int64,bool>` keys a distinct mono from `Result<int32,bool>` (different T).
//   r = Result<int64,bool>.Ok(77).Unwrap()  → .Ok(var v) → 77 (i64) → 77 (i32)
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
		Result<int64, bool> a = Result<int64, bool>.Ok(77);
		int64 v = a.Unwrap();
		return (int32)v; // 77
	}
}
