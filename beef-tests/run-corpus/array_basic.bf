// expect: 64
// Heap arrays `T[]`: `new int32[n]` is a length-prefixed block (the count in the
// 8 bytes before the elements), so `a[i]` is an ordinary typed-pointer index,
// `a.Count` reads the header, and `delete a` frees the real base. Fill, sum, and
// fold in the count.
class Program {
	public static int32 Main() {
		int32[] a = new int32[4];
		for (int32 i = 0; i < 4; i++) {
			a[i] = (i + 1) * 10;   // 10, 20, 30, 40
		}
		int32 sum = 0;
		for (int32 i = 0; i < a.Count; i++) {
			sum = sum + a[i];      // 100
		}
		int32 r = sum + (int32)a.Count * 6 + a[3] - 100;  // 100 + 24 + 40 - 100 = 64
		delete a;
		return r;
	}
}
