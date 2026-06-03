// expect: 7313
// `params T[]` variadic parameters: the call site packs the overflow arguments
// into a fresh `T[]`, which the callee uses like any array (foreach/.Count). Sum
// is pure-variadic (incl. a zero-arg call → empty array); Tag has a fixed leading
// param then the pack. Runtime-derived element values keep the packed stores off
// the JIT's __xmm constant-pool path.
class Program {
	static int32 Sum(params int32[] xs) {
		int32 s = 0;
		for (var v in xs) { s += v; }
		return s;
	}
	static int32 Tag(int32 tag, params int32[] xs) {
		int32 s = tag * 1000;
		for (var v in xs) { s += v; }
		return s;
	}
	public static int32 Main() {
		int32 k = 1;
		int32 a = Sum(k, k + 1, k + 2, k + 3);  // [1,2,3,4] → 10
		int32 b = Sum();                        // [] → 0
		int32 c = Sum(100, 200);                // [100,200] → 300
		int32 d = Tag(7, k, k + 1);             // 7000 + 1 + 2 → 7003
		return a + b + c + d;                   // 10 + 0 + 300 + 7003 = 7313
	}
}
