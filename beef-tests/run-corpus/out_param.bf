// expect: 30
// `out` parameters: like `ref`, the callee writes through the caller's address,
// but the local need not be initialised first (`int32 q;`). DivMod returns the
// quotient via the return value and the remainder via `out r`.
class Program {
	static int32 DivMod(int32 n, int32 d, out int32 r) {
		r = n - (n / d) * d;
		return n / d;
	}
	public static int32 Main() {
		int32 rem;
		int32 quo = DivMod(23, 5, out rem);  // quo=4, rem=3
		return quo * 5 + rem + 7;            // 20 + 3 + 7 = 30
	}
}
