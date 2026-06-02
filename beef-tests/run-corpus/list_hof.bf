// expect: 18
// Higher-order functions over List<int32> — the lambda payoff. The free generic
// `Map`/`Filter`/`Fold` (in corlib) each take a `function`-typed parameter and
// call it on every element, so the list flows through a map → filter → fold
// pipeline. The lambdas are bound to `function`-typed locals (target-typed) and
// passed by value; inside each method the function parameter is now callable.
//   xs           = [1, 2, 3, 4]
//   Map(x => x*2)    → [2, 4, 6, 8]
//   Filter(x => x>3) → [4, 6, 8]
//   Fold(+, 0)       → 18
class Program {
	public static int32 Main() {
		List<int32> xs = new List<int32>();
		xs.Add(1);
		xs.Add(2);
		xs.Add(3);
		xs.Add(4);
		function int32(int32) dbl = x => x * 2;
		function bool(int32) big = x => x > 3;
		function int32(int32, int32) plus = (a, x) => a + x;
		List<int32> doubled = Map<int32, int32>(xs, dbl);
		List<int32> kept = Filter<int32>(doubled, big);
		int32 sum = Fold<int32, int32>(kept, 0, plus);
		return sum; // 4 + 6 + 8 = 18
	}
}
