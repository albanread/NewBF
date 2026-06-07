// expect: 1
// MS-T4: a `return` out of an open `scope` frame frees its scope allocations
// (dtor + free) before unwinding — exactly once. The dtor bumps a static counter
// stored into an out-slot before the return so the returned value reflects the
// post-cleanup count.
class Tracked {
	public static int32 sDtors = 0;
	public ~this() { Tracked.sDtors += 1; }
}
class Program {
	static int32 Helper() {
		Tracked t = scope Tracked();   // open scope frame
		return 7;                      // return frees the scope → dtor runs once
	}
	public static int32 Main() {
		int32 r = Helper();            // r == 7, dtor already ran inside Helper
		return Tracked.sDtors;         // 1
	}
}
