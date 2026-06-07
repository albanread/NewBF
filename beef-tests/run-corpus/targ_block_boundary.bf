// expect: 1
// SSA dominance across a block boundary (§5.4, blocker #3). A null-conditional
// `a?.M(.(…))` lowers `lower_method_call` INSIDE the `qcall.nonnull` block, so
// the pending `.(0,1)` construction must be emitted there (where it dominates the
// call), not in the entry block. We also use the pending arg in both arms of an
// `if` so the construction lands in each arm's own block.
//   a non-null → a.M(.(0,1)) = 0 + 1 = 1, the ?. result is 1
//   then if (r == 1) the second pending call confirms the value is preserved.
struct Vec2 {
	public int32 x;
	public int32 y;
	public this(int32 x, int32 y) { this.x = x; this.y = y; }
}
class Use {
	public int32 M(Vec2 v) { return v.x + v.y; }
}
class Program {
	public static int32 Main() {
		Use a = new Use();
		int32 r = a?.M(.(0, 1)); // pending arg constructed in the nonnull block
		int32 result = 0;
		if (r == 1) {
			result = a.M(.(1, 0)); // pending arg in the taken if-arm's block
		} else {
			result = a.M(.(9, 9)); // pending arg in the (untaken) else block
		}
		delete a;
		return result; // 1
	}
}
