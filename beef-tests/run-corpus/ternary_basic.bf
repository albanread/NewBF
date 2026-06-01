// expect: 42
// Ternary `?:` selects, and short-circuits: only the taken arm is evaluated.
// `a` picks the then-arm; `b` picks the else-arm whose *then*-arm is `100 / x`
// with x == 0 — a divide-by-zero that must never run (a crash would fail this).
class Program {
	public static int32 Main() {
		int32 x = 0;
		int32 a = x == 0 ? 42 : 7;          // -> 42
		int32 b = x != 0 ? (100 / x) : 0;   // -> 0, and the 100/x is NOT evaluated
		return a + b;
	}
}
