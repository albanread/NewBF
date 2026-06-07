// expect: 30
// MX-T3 — an EXPRESSION mixin yielding a value via an `=> expr` body. `Double!(x)`
// splices `x * 2` and yields it; the result slot is target-typed from the
// `int32 a = …` declaration (mixins.md §3.5). The param `x` is bound ONCE to the
// argument value in the splice scope. 15 * 2 = 30.
class Program {
	static mixin Double(int32 x) => x * 2;
	public static int32 Main() {
		int32 a = Double!(15);
		return a;   // 30
	}
}
