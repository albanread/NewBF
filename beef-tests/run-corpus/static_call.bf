// expect: 42
// Qualified static call on a (user) type: `Calc.Twice(21)` — the receiver is a
// type name, not an instance, so no `this` is passed.
class Calc {
	public static int32 Twice(int32 n) { return n + n; }
}
class Program {
	public static int32 Main() {
		return Calc.Twice(21);
	}
}
