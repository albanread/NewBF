// expect: 110
// MX-T3 — block-trailing-yield (mixins.md §3.5, the hard piece): a BLOCK-bodied
// mixin with LEADING statements (`int32 t = a + b;`) AND a trailing bare
// expression (`t * 2`) that is the yield. The leading statements lower normally
// into a fresh splice scope; the trailing `Stmt::Expr` is NOT discarded but
// stored into the pre-alloca'd result slot. Params `a`/`b` bind once.
// t = 5 + 50 = 55; yield t * 2 = 110.
class Program {
	static mixin Combine(int32 a, int32 b) {
		int32 t = a + b;
		t * 2
	}
	public static int32 Main() {
		int32 r = Combine!(5, 50);
		return r;   // 110
	}
}
