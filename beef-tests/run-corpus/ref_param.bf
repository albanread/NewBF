// expect: 12
// `ref` parameters: the callee mutates the caller's storage through the passed
// address. Swap exchanges two locals in place; AddTo accumulates into one.
class Program {
	static void Swap(ref int32 a, ref int32 b) {
		int32 t = a;
		a = b;
		b = t;
	}
	static void AddTo(ref int32 acc, int32 v) {
		acc = acc + v;
	}
	public static int32 Main() {
		int32 x = 3;
		int32 y = 7;
		Swap(ref x, ref y);   // x=7, y=3
		AddTo(ref y, 9);      // y=12
		return y;             // 12
	}
}
