// expect: 6
// IT-T2+3: a `yield return`-based generator, materialized EAGERLY into a
// `List<int32>` by `rewrite_generators` (newbf-sema, before collect_insts/
// ownership). `Nums()` desugars to:
//   List<int32> Nums() {
//       List<int32> __yield = new List<int32>();
//       __yield.Add(1); __yield.Add(2); __yield.Add(3);
//       return __yield;
//   }
// then `foreach (var x in Nums())` iterates the returned List via the ordinary
// Count/Get path (IT-T1 composition). 1 + 2 + 3 = 6.
class Program {
	public static List<int32> Nums() {
		yield return 1;
		yield return 2;
		yield return 3;
	}

	public static int32 Main() {
		int32 sum = 0;
		var ns = Nums();
		for (var x in ns) { sum += x; }
		delete ns;
		return sum;   // 1 + 2 + 3 = 6
	}
}
