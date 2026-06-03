// expect: 77
// `out` over a *class reference*: the callee allocates and hands the new object
// back through the caller's storage. The bound place is `Ref`-typed, so reading
// `c` loads the pointer and `c = new …` stores it into the caller's variable.
class Box { public int32 v; }
class Program {
	static void Make(int32 v, out Box b) {
		b = new Box();
		b.v = v;
	}
	public static int32 Main() {
		Box b;
		Make(77, out b);
		int32 r = b.v;
		delete b;
		return r;   // 77
	}
}
