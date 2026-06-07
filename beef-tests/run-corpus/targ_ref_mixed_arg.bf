// expect: 30
// TA-8 (c): a call MIXING a `ref` arg and a PENDING dot-form arg — proving they
// coexist. A pending `.(…)` is never an lvalue, so `arg_value`'s `ref`/`out` branch
// never wraps it; in Phase 1 the `ref total` arg lowers concretely (its address-of
// taken via `arg_value`), and the `.(3,4)` stays a pending hole, back-filled in
// Phase 2 against the resolved `Vec2` param. The callee mutates the caller's
// `total` through the passed address while the `.(…)` is constructed by value.
//   total = 6
//   Accum(ref total, .(3,4)):  total += 3 + 4 = 7  -> total = 13
//   Accum(ref total, .(8,9)):  total += 8 + 9 = 17 -> total = 30
struct Vec2 {
	public int32 x;
	public int32 y;
	public this(int32 x, int32 y) { this.x = x; this.y = y; }
}
class Program {
	static void Accum(ref int32 total, Vec2 v) {
		total = total + v.x + v.y;
	}
	public static int32 Main() {
		int32 total = 6;
		Accum(ref total, .(3, 4)); // ref + pending coexist: total = 6 + 7 = 13
		Accum(ref total, .(8, 9)); //                        total = 13 + 17 = 30
		return total;              // 30
	}
}
