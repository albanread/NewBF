// expect: 49
// CB-T6: widened-int comptime fold + the fold-width fix. A `[Comptime]` function
// that returns `int32` and takes an `int32` arg, called with a constant `F(7)`,
// folds AT COMPILE TIME to the `i32` constant 49 (7*7). The fold rewrites the
// call into a literal of the call's OWN result width (`i32`), not a hardcoded
// `i64`, so the module is verify-clean and `F` is dropped (compile-time only) —
// yet `Main` still returns 49, the proof the call was folded, not run.
class Program {
	[Comptime]
	public static int32 F(int32 x) {
		return x * x;
	}

	public static int32 Main() {
		return F(7);
	}
}
