// expect: 1200
// String.StartsWith / EndsWith — byte-for-byte prefix/suffix checks over the
// corlib String. "Hello": StartsWith("He") and EndsWith("llo") hold; the
// mismatched probes don't.
//   StartsWith("He")  → true  → +1000
//   StartsWith("lo")  → false →   (none)
//   EndsWith("llo")   → true  → +200
//   EndsWith("He")    → false →   (none)
class Program {
	public static int32 Main() {
		String s = "Hello";
		String pre = "He";
		String bad = "lo";
		String suf = "llo";
		int32 r = 0;
		if (s.StartsWith(pre)) { r = r + 1000; }
		if (s.StartsWith(bad)) { r = r + 1; }
		if (s.EndsWith(suf)) { r = r + 200; }
		if (s.EndsWith(pre)) { r = r + 1; }
		delete s;
		delete pre;
		delete bad;
		delete suf;
		return r; // 1000 + 200 = 1200
	}
}
