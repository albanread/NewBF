// expect: 876
// String.IndexOf(String) substring search + ToUpper/ToLower. IndexOf(String) is
// overload-resolved against IndexOf(char8) by the String argument.
//   "Hello, World".IndexOf("World") = 7 ; .IndexOf("xyz") = -1
//   ToUpper()[0] = 'H' (72) ; ToLower()[0] = 'h' (104)
//   r = 7*100 + (-1 + 1) + 72 + 104 = 700 + 0 + 72 + 104 = 876
class Program {
	public static int32 Main() {
		String s = "Hello, World";
		int32 idx = s.IndexOf("World");
		int32 miss = s.IndexOf("xyz");
		String up = s.ToUpper();
		String lo = s.ToLower();
		int32 r = idx * 100 + (miss + 1) + up[0] + lo[0];
		delete s;
		delete up;
		delete lo;
		return r; // 876
	}
}
