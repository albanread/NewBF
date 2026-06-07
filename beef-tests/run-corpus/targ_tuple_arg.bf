// expect: 23
// The §3.6 Tuple decision: a bare tuple literal `(a, b)` as a CALL ARGUMENT is
// classified CONCRETE (not pending). `arg_is_pending` returns false for an
// `Expr::Tuple`, so this call takes the EAGER path; the tuple lowers via the
// existing `build_tuple(None, …)` inference, which matches the element widths
// (`i32,i32`) to the registered `(int32,int32)` tuple struct and passes it by
// value. This program PROVES the concrete-Tuple path works (no promotion needed).
//   Sum((15, 8)) = 15 + 8 = 23
class Program {
	static int32 Sum((int32, int32) p) {
		return p.0 + p.1;
	}
	public static int32 Main() {
		int32 a = 15;
		int32 b = 8;
		return Sum((a, b)); // bare tuple literal arg → concrete, build_tuple(None)
	}
}
