// expect: 26
// `??=` null-coalescing assignment: `a ??= b` stores `b` into `a` only when `a`
// is currently null, leaving a non-null `a` untouched.
//   a = null   → a ??= Mk(7)  → a becomes the new box (v = 7)
//   b = Mk(20) → b ??= Mk(99) → b stays as-is (v = 20)
//   a.v + b.v - 1 = 7 + 20 - 1 = 26
class Box {
	public int32 v;
}
class Program {
	static Box Mk(int32 v) {
		Box b = new Box();
		b.v = v;
		return b;
	}
	public static int32 Main() {
		Box a = null;
		a ??= Mk(7);     // a was null → becomes Mk(7)

		Box b = Mk(20);
		Box keep = b;    // remember the original to free it
		b ??= Mk(99);    // b non-null → unchanged

		int32 r = a.v + b.v - 1; // 7 + 20 - 1 = 26
		delete a;
		delete keep;
		return r;
	}
}
