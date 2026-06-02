// expect: 30
// Lambdas with parameters. The params are untyped in the source (`x`, `(a, b)`);
// their types come from the target `function` type (target-typing). A single
// bare-ident param and a two-param paren form, each emitted as a free function
// and called.
class Program {
	public static int32 Main() {
		function int32(int32) dbl = x => x * 2;             // 5 * 2 = 10
		function int32(int32, int32) add = (a, b) => a + b; // 8 + 12 = 20
		return dbl(5) + add(8, 12);                          // 10 + 20 = 30
	}
}
