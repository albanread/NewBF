// expect: 32
// String.Substring(start, len) builds a NEW corlib String of `len` chars
// starting at `start`, via the same Append path the String grows through.
//   s   = "abcdef"            (indices a=0 b=1 c=2 d=3 e=4 f=5)
//   sub = s.Substring(2, 3)   -> "cde"  (chars at 2,3,4), Length 3
// Pack the two checks into one int:
//   sub.Length() * 10            = 3 * 10        = 30
//   sub.CharAt(0) - 'a'          = 'c' - 'a'      =  2   ('c'=99, 'a'=97)
//   total                                          = 32
class Program {
	public static int32 Main() {
		String s = "abcdef";
		String sub = s.Substring(2, 3);
		int32 r = sub.Length() * 10 + (sub.CharAt(0) - 'a');
		delete sub;
		delete s;
		return r;
	}
}
