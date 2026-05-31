// expect: 5427
// Append is overloaded by argument type: `s.Append(t)` (t is a String) selects
// Append(String); `s.Append('!')` selects Append(char8). Resolution is by the
// argument's type, not just arity.
//   "ab"(97,98) + "cd"(99,100) -> "abcd", + '!'(33) -> "abcd!"
//   sum = 97+98+99+100+33 = 427, len 5 -> 5*1000 + 427
class Program {
	public static int32 Main() {
		String s = "ab";
		String t = "cd";
		s.Append(t);
		s.Append('!');
		int32 sum = 0;
		for (int32 i = 0; i < s.Length(); i++) {
			sum = sum + s.CharAt(i);
		}
		int32 r = s.Length() * 1000 + sum;
		delete s;
		delete t;
		return r;
	}
}
