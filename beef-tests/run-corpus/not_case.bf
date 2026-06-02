// expect: 17
// Beef's negated case-test: `x not case .P` is `!(x case .P)`. The parser now
// keeps the negation (it used to drop it), wrapping the case-test in a logical
// `!`. Used the idiomatic way — to guard the *absence* of a case — so no binding
// is read on the matched side.
//   a = Some(9) → `a not case .None` true  → r += 9
//   b = None    → `b not case .None` false → else → r += 8
enum IntOpt {
	case Some(int32 value);
	case None;
}
class Program {
	public static int32 Main() {
		IntOpt a = IntOpt.Some(9);
		IntOpt b = IntOpt.None;
		int32 r = 0;
		if (a not case .None) {
			r = r + 9;        // 9
		}
		if (b not case .None) {
			r = r + 100;
		} else {
			r = r + 8;        // 17
		}
		return r; // 9 + 8 = 17
	}
}
