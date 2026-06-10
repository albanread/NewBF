// expect: 3
// IT-T2+3: `yield break` mid-sequence inside a `for` loop — proves the RECURSIVE
// rewrite (R9: the yield-in-loop stays IN the loop, the surrounding control flow
// is preserved) and that `yield break` → an early `return __yield;`. `Upto(5)`
// desugars to:
//   List<int32> Upto(int32 n) {
//       List<int32> __yield = new List<int32>();
//       for (var i in 1...n) {
//           if (i > 2) return __yield;   // <- yield break, in situ in the loop
//           __yield.Add(i);              // <- yield return i, in situ in the loop
//       }
//       return __yield;
//   }
// so it collects 1, 2, then breaks at i == 3. 1 + 2 = 3.
class Program {
	public static List<int32> Upto(int32 n) {
		for (var i in 1...n) {
			if (i > 2) { yield break; }
			yield return i;
		}
	}

	public static int32 Main() {
		int32 sum = 0;
		var ns = Upto(5);
		for (var x in ns) { sum += x; }
		delete ns;
		return sum;   // 1 + 2 = 3
	}
}
