// expect: 21
// Multi-capture closure: validates the new env layout where captures are stored
// at index `i` (no leading code-pointer slot). `scale` captures BOTH `m` and `c`;
// the body reads them as `$self[0]` and `$self[1]`. Passed to the generic Map.
//   xs    = [1, 2, 3]
//   m = 3, c = 1
//   scale = x => x * m + c   → [4, 7, 10]
//   Fold(+, 0)              → 21
class Program {
	public static int32 Main() {
		List<int32> xs = new List<int32>();
		xs.Add(1);
		xs.Add(2);
		xs.Add(3);
		int32 m = 3;
		int32 c = 1;
		function int32(int32) scale = x => x * m + c;  // captures m AND c
		List<int32> ys = Map<int32, int32>(xs, scale);
		function int32(int32, int32) plus = (acc, x) => acc + x;
		return Fold<int32, int32>(ys, 0, plus);        // 4 + 7 + 10 = 21
	}
}
