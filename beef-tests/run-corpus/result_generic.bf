// expect: 42
// A generic `Result<T, E>` monomorphized to *heterogeneous* payloads:
// `Result<int32, bool>` has Ok(int32, 4 bytes) and Err(bool, 1 byte) — different
// types at the same position, so the union slot is the widest scalar (int32).
// Construction is target-typed (`.Ok(x)`/`.Err(x)` against the return type).
//   Div(20, 4) → .Ok(5)    → match .Ok(let q) → r += 5
//   Div(1, 0)  → .Err(false) → match .Err(let e) → e is false → r += 37
//   r = 5 + 37 = 42
enum Result<T, E> {
	case Ok(T value);
	case Err(E error);
}
class Program {
	static Result<int32, bool> Div(int32 a, int32 b) {
		if (b == 0) { return .Err(false); }
		return .Ok(a / b);
	}
	public static int32 Main() {
		int32 r = 0;
		switch (Div(20, 4)) {
		case .Ok(let q): r = r + q;        // 5
		case .Err(let e): r = r - 1;
		}
		switch (Div(1, 0)) {
		case .Ok(let q): r = r + q;
		case .Err(let e): if (!e) { r = r + 37; }  // e == false → +37
		}
		return r; // 5 + 37 = 42
	}
}
