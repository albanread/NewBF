// expect: 6533
// A fuller String workflow on the real corlib String: construct from a literal,
// Append, then iterate with Length()/CharAt().
//   "Hello" = 72+101+108+108+111 = 500; + '!'(33) -> 533; len 6 -> 6*1000+533.
class Program {
	public static int32 Main() {
		String s = "Hello";
		s.Append('!');
		int32 sum = 0;
		for (int32 i = 0; i < s.Length(); i++) {
			sum = sum + s.CharAt(i);
		}
		int32 r = s.Length() * 1000 + sum;
		delete s;
		return r;
	}
}
