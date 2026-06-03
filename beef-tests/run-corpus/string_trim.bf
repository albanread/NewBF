// expect: 751
// String.Trim / TrimStart / TrimEnd remove ASCII whitespace (space/tab/CR/LF),
// returning a new String via Substring.
//   "  hi  ".Trim()      = "hi"   (len 2)
//   "  hi  ".TrimStart() = "hi  " (len 4)
//   "  hi  ".TrimEnd()   = "  hi" (len 4)
//   "\t\nok\r ".Trim()   = "ok"   (len 2; [0] = 'o' = 111)
//   r = 2*100 + 4*100 + 4*10 + 111 = 200 + 400 + 40 + 111 = 751
class Program {
	public static int32 Main() {
		String s = "  hi  ";
		String a = s.Trim();
		String b = s.TrimStart();
		String c = s.TrimEnd();

		String w = "\t\nok\r ";
		String d = w.Trim();

		int32 r = a.Length() * 100 + b.Length() * 100 + c.Length() * 10 + d[0];
		delete s;
		delete a;
		delete b;
		delete c;
		delete w;
		delete d;
		return r;
	}
}
