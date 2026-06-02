// expect: 47
// `if x case .Some(let v)` — the case-test operator as a boolean *expression*
// that also binds the matched payload into the guarded branch. This is the
// building block enum methods need (`if x case .Some(let v)` instead of a full
// `switch`). Lowers to one arm of the `match` machinery: store the scrutinee,
// compare its discriminant, and bind `v` to the payload field.
//   a = Some(42) → `a case .Some(let v)` true,  binds v = 42 → r += 42
//   b = None     → `b case .Some(let v)` false, else branch  → r += 5
enum IntOpt {
	case Some(int32 value),
	case None
}
class Program {
	public static int32 Main() {
		IntOpt a = IntOpt.Some(42);
		IntOpt b = IntOpt.None;
		int32 r = 0;
		if (a case .Some(let v)) {
			r = r + v;        // 42
		} else {
			r = r - 1;
		}
		if (b case .Some(let v)) {
			r = r + v;
		} else {
			r = r + 5;        // 47
		}
		return r; // 42 + 5 = 47
	}
}
