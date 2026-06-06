// expect: 7
// Null function-value semantics (FV-T3 §5.4). `function R(P) f = null;` gives a
// defined `$Func { code = null, target = null }`. `f == null` is lowered as a
// single compare on the code field (`f.code == null`), so it is `true` here; a
// non-null function value compares `false`. (Calling a null function value is
// UB — same as Beef — and is NOT exercised.)
//   f = null     → f == null is true   → +5
//   g = x => x   → g == null is false  → +2
//   total = 7
class Program {
	public static int32 Main() {
		function int32(int32) f = null;
		function int32(int32) g = x => x;
		int32 r = 0;
		if (f == null) { r = r + 5; }     // true
		if (g != null) { r = r + 2; }     // true (g is non-null)
		return r;                          // 5 + 2 = 7
	}
}
