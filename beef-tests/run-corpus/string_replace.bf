// expect: 735
// corlib String: Replace(char8,char8) returns a new same-length String with each
// `from` swapped for `to`; Count(char8) tallies occurrences; LastIndexOf(char8)
// scans from the end. "a.b.c.d" → Replace('.','-') = "a-b-c-d" (len 7), three
// dots, last dot at index 5.
class Program {
	public static int32 Main() {
		String s = "a.b.c.d";
		String r = s.Replace('.', '-');
		int32 dots = s.Count('.');         // 3
		int32 last = s.LastIndexOf('.');   // 5
		int32 result = (int32)r.Length() * 100 + dots * 10 + last;  // 735
		delete r;
		delete s;
		return result;
	}
}
