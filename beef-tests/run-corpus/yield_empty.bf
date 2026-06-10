// expect: 0
// IT-T2+3: an EMPTY generator — an immediate `yield break;` yields nothing, so
// the eager rewrite produces an empty `List<int32>` and the trailing fall-off
// `return __yield;` returns it:
//   List<int32> Nothing() {
//       List<int32> __yield = new List<int32>();
//       return __yield;   // <- yield break (the only statement), then fall-off
//       return __yield;
//   }
// `foreach` over the empty list runs the body zero times. sum stays 0.
class Program {
	public static List<int32> Nothing() {
		yield break;
	}

	public static int32 Main() {
		int32 sum = 0;
		var ns = Nothing();
		for (var x in ns) { sum += x; }
		delete ns;
		return sum;   // empty sequence -> 0
	}
}
