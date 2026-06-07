// expect: 42
// MX-T4.5 precursor — a GENERIC payload enum's INSTANCE method that does
// `switch (this)` with `var` payload binding, returning the `.Ok` payload, with
// the `.Err` arm returning `default` (the zeroed `T`). This is the load-bearing
// shape MX-T5 (`Result.bf` prelude) + MX-T6 (`Try!`) rest on: the generic enum
// instance method must monomorphize for `(int32, bool)` and dispatch on a `this`
// pointer (a `Ref`, not a `Struct` value) — switch-on-`this` was the gap.
//   r1 = Result<int32,bool>.Ok(42).Unwrap()  → switch(this) → .Ok(var v) → 42
//   r2 = Result<int32,bool>.Err(true).Unwrap() → .Err arm → default(int32) = 0
//   r = 42 + 0 = 42
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
		Result<int32, bool> a = Result<int32, bool>.Ok(42);
		Result<int32, bool> b = Result<int32, bool>.Err(true);
		return a.Unwrap() + b.Unwrap(); // 42 + 0 = 42
	}
}
