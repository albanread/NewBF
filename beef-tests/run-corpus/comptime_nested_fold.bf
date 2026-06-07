// expect: 40
// CB-T6: inner-fold-first / fixpoint. `Outer(Inner(3))` where BOTH are
// `[Comptime]`: the fold collapses bottom-up — the first pass folds `Inner(3)`
// to the constant 4, the next pass then sees `Outer(4)` (its arg is now a
// compile-time constant) and folds it to 40. The collect/apply loop iterates to
// a fixpoint, so the nested comptime calls fully collapse to a single literal and
// both comptime functions are dropped. (3 + 1) * 10 = 40.
class Program {
	[Comptime]
	public static int32 Inner(int32 x) {
		return x + 1;
	}

	[Comptime]
	public static int32 Outer(int32 y) {
		return y * 10;
	}

	public static int32 Main() {
		return Outer(Inner(3));
	}
}
