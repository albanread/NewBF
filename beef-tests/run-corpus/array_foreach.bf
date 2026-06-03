// expect: 100
// `foreach` over a heap array: `for (v in a)` lowers to an indexed loop reading
// the length header and `a[i]`. Also exercises `break`/`continue` over an array
// (wired to the loop stack like the other foreach shapes).
class Program {
	public static int32 Main() {
		int32[] a = new int32[5];
		for (int32 i = 0; i < 5; i++) { a[i] = (i + 1) * 10; }  // 10..50
		int32 sum = 0;
		for (var v in a) {
			if (v == 30) { continue; }   // skip 30
			if (v == 50) { break; }      // stop before 50
			sum += v;                    // 10 + 20 + 40 = 70
		}
		int32 total = 0;
		for (var v in a) { total += v; } // 150
		delete a;
		return sum + total - 120;        // 70 + 150 - 120 = 100
	}
}
