// expect: 23
// Tuples cross call boundaries: passed by value as a `(int32, int32)` param,
// returned by value, and mutated field-by-field as a local. MakePair returns a
// tuple; Sum takes one; the local `p` is reassigned through `p.0`.
class Program {
	static (int32, int32) MakePair(int32 a, int32 b) {
		(int32, int32) r = (a, b);
		return r;
	}
	static int32 Sum((int32, int32) p) {
		return p.0 + p.1;
	}
	public static int32 Main() {
		(int32, int32) p = MakePair(5, 8);   // (5, 8)
		p.0 = p.0 + 10;                       // (15, 8)
		return Sum(p);                        // 23
	}
}
