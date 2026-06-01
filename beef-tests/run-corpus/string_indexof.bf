// expect: 20
// String.IndexOf(char8) returns the index of the first occurrence, or -1.
// "Hello" indices: H=0 e=1 l=2 l=3 o=4.
//   found  = IndexOf('l') = 2  (first 'l', not the second at 3)
//   absent = IndexOf('z') = -1 ('z' not present)
// Encode both unambiguously: found * 10 + (absent + 1)
//   = 2 * 10 + (-1 + 1) = 20 + 0 = 20
class Program {
	public static int32 Main() {
		String s = "Hello";
		int32 found = s.IndexOf('l');
		int32 absent = s.IndexOf('z');
		int32 r = found * 10 + (absent + 1);
		delete s;
		return r;
	}
}
