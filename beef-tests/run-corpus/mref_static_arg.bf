// expect: 30
// A STATIC method reference passed to a generic HOF (FV-T4). `Mathx.Square` has
// no `$self` param, so it is wrapped in a `$mref$` thunk that drops the uniform
// convention's hidden `$self` and forwards the real arg — otherwise the
// `code(null, args…)` shape would shift every argument by one.
//   xs     = [1, 2, 3, 4]
//   Square → [1, 4, 9, 16]
//   Fold(+, 0) → 30
class Mathx {
	public static int32 Square(int32 x) { return x * x; }
}
class Program {
	public static int32 Main() {
		List<int32> xs = new List<int32>();
		xs.Add(1);
		xs.Add(2);
		xs.Add(3);
		xs.Add(4);
		function int32(int32) sq = Mathx.Square;        // static method ref
		List<int32> ys = Map<int32, int32>(xs, sq);
		function int32(int32, int32) plus = (acc, x) => acc + x;
		return Fold<int32, int32>(ys, 0, plus);         // 1 + 4 + 9 + 16 = 30
	}
}
