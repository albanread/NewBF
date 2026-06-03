// expect: 109
// Explicit null comparison on class references — `a != null` / `a == null`.
// These compile to a pointer `icmp` against null; before the `as_int` ptrtoint
// fix they folded to `undef` and branched unpredictably.
//   a = Make(true)  → non-null → (a != null) → +9
//   b = Make(false) → null     → (b == null) → +100
//   (a == null) is false → skip
class Box {
	public int32 v;
}
class Program {
	static Box Make(bool yes) {
		if (yes) {
			Box b = new Box();
			b.v = 9;
			return b;
		}
		return null;
	}
	public static int32 Main() {
		Box a = Make(true);
		Box b = Make(false);
		int32 r = 0;
		if (a != null) { r = r + a.v; }   // +9
		if (b == null) { r = r + 100; }   // +100
		if (a == null) { r = r + 1; }     // skip
		delete a;
		return r; // 109
	}
}
