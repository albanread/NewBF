// expect: 85
// Arrays cross call boundaries: `Make` allocates and returns a `T[]` (the caller
// binds it to a `T[]` local, so `.Count`/indexing/`delete` all track); `Sum`
// takes a `T[]` parameter and uses `.Count` + foreach on it. The marker that
// makes `.Count` work is now applied to array params and the receiving local.
class Program {
	static int32[] Make(int32 n) {
		int32[] a = new int32[n];
		for (int32 i = 0; i < n; i++) { a[i] = (i + 1) * 5; }  // 5,10,15,20,25
		return a;
	}
	static int32 Sum(int32[] xs) {
		int32 s = 0;
		for (var v in xs) { s += v; }
		return s + (int32)xs.Count;   // 75 + 5
	}
	public static int32 Main() {
		int32[] a = Make(5);
		int32 total = Sum(a);   // 80
		int32 r = total + a[0]; // + 5 = 85
		delete a;
		return r;
	}
}
