// expect: 180
// GM-B2 — the marquee payoff: higher-order functions as *instance* generic
// methods on a real `List<T>`. `xs.Map<R>(f)` is an instance generic method on
// the generic owner `List<int32>` taking a uniform function value — it resolves
// to an owner-mono-prefixed symbol (`@List$i32.Map$i32`) via generic-methods B1,
// and the `function`-typed argument flows through fn-values Slice A. This is the
// idiomatic form the static `Functional.Map/Filter/Fold` (still in corlib, used
// by list_hof.bf) was a stand-in for.
//   xs                 = [1, 2, 3, 4]
//   .Map<int32>(*10)   → [10, 20, 30, 40]
//   .Filter(x > 15)    → [20, 30, 40]
//   .Fold<int32>(0, +) → 90    ... and a second Map<R> with R != T (see below)
class Program {
	public static int32 Main() {
		List<int32> xs = new List<int32>();
		xs.Add(1);
		xs.Add(2);
		xs.Add(3);
		xs.Add(4);

		function int32(int32) ten = x => x * 10;
		List<int32> scaled = xs.Map<int32>(ten);   // [10, 20, 30, 40]

		function bool(int32) big = x => x > 15;
		List<int32> kept = scaled.Filter(big);      // [20, 30, 40]

		function int32(int32, int32) plus = (acc, x) => acc + x;
		int32 sum = kept.Fold<int32>(0, plus);      // 20 + 30 + 40 = 90

		// A second Map<R> where the method type-param R differs from the owner T:
		// map List<int32> -> List<int64>, then fold back to int32. Proves the
		// method type-param is distinct from the owner type-param.
		function int64(int32) widen = x => (int64)x;
		List<int64> wide = kept.Map<int64>(widen);  // [20L, 30L, 40L]
		function int64(int64, int64) plus64 = (acc, x) => acc + x;
		int64 wsum = wide.Fold<int64>(0, plus64);   // 90L

		return sum + (int32)wsum;                   // 90 + 90 = 180
	}
}
