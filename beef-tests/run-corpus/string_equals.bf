// expect: 10
// String.Equals is value equality (length + bytes), not identity: `a` and `b`
// are distinct objects with distinct buffers but compare equal; `c` differs.
class Program {
	public static int32 Main() {
		String a = "hello";
		String b = "hello";
		String c = "world";
		int32 r = 0;
		if (a.Equals(b)) { r = r + 10; }
		if (a.Equals(c)) { r = r + 1; }
		delete a;
		delete b;
		delete c;
		return r;
	}
}
