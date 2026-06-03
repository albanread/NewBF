// expect: 12
// `??` null-coalescing: `a ?? b` is `a` when non-null, else `b`, evaluating `b`
// only when `a` is null (short-circuit, lowered like `?:` on `a == null`).
//   a = Pick(true)  → non-null → r1 = a        (v = 7)
//   b = Pick(false) → null     → r2 = fallback (v = 5)
//   r1.v + r2.v = 7 + 5 = 12
class Box {
	public int32 v;
}
class Program {
	static Box Pick(bool yes) {
		if (yes) {
			Box b = new Box();
			b.v = 7;
			return b;
		}
		return null;
	}
	public static int32 Main() {
		Box fallback = new Box();
		fallback.v = 5;

		Box a = Pick(true);
		Box r1 = a ?? fallback;   // a non-null → a

		Box b = Pick(false);
		Box r2 = b ?? fallback;   // b null → fallback

		int32 result = r1.v + r2.v; // 7 + 5 = 12
		delete a;
		delete fallback;
		return result;
	}
}
