// expect: 42
// `==`/`!=` on a class with an Equals(Self) method (String) are value equality:
// a and b are distinct objects with the same text, so a == b; a != c. (null and
// other classes keep reference identity.)
class Program {
	public static int32 Main() {
		String a = "hello";
		String b = "hello";   // distinct object, equal value
		String c = "world";
		int32 r = 0;
		if (a == b) { r = r + 40; }    // value-equal -> +40
		if (a != c) { r = r + 2; }     // value-unequal -> +2
		if (a == c) { r = r + 100; }   // false -> no
		delete a;
		delete b;
		delete c;
		return r;
	}
}
