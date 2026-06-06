// expect: 49
// Qualified generic-method call `Type.Method<T>(args)` — calling a generic
// helper on another class. Generic methods use global-name mangling, so the
// qualified call resolves to the same monomorph a bare call would. Pick returns
// the max; Util.Pick<int32>(40, 9) = 40, plus Util.Pick<int32>(2, 9) = 9 → 49.
class Util {
	public static T Pick<T>(T a, T b) { return a > b ? a : b; }
}
class Program {
	public static int32 Main() {
		int32 hi = Util.Pick<int32>(40, 9);   // 40
		int32 lo = Util.Pick<int32>(2, 9);    // 9
		return hi + lo;                       // 49
	}
}
