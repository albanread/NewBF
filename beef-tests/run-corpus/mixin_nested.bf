// expect: 25
// MX-T3 — nested splice (mixins.md §3.3, depth guard): a mixin whose body invokes
// another mixin. `Outer!(x)` yields `Inner!(x) + 1`; `Inner!(y)` yields `y * 2`.
// The inner `Name!(…)` splices INSIDE the outer splice — depth 2 — bounded by
// MIXIN_MAX_DEPTH. The `mixin_stack` is truncated back to the pre-splice length
// after each (R5). Outer!(12) = Inner!(12) + 1 = 24 + 1 = 25.
class Program {
	static mixin Inner(int32 y) => y * 2;
	static mixin Outer(int32 x) => Inner!(x) + 1;
	public static int32 Main() {
		int32 r = Outer!(12);
		return r;   // 25
	}
}
