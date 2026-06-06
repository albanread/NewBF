// expect: 36
// THE §49 FIX. A *capturing* closure passed into a GENERIC higher-order method.
// `addB = a => a + b` captures the outer local `b`, so its function value is a
// `$Func` { code, target=env }. Passing it to the generic `Map<int32,int32>`
// (corlib `Functional.Map`) used to SEGFAULT: the callee's `f(x)` took the
// non-closure branch and called the env pointer as a code pointer with the args
// shifted off by one. With the uniform `code(target, args…)` convention the
// capture is read correctly inside the lambda body.
//   xs   = [1, 2, 3]
//   b    = 10
//   addB = a => a + b      → [11, 12, 13]
//   Fold(+, 0)            → 36
class Program {
	public static int32 Main() {
		List<int32> xs = new List<int32>();
		xs.Add(1);
		xs.Add(2);
		xs.Add(3);
		int32 b = 10;
		function int32(int32) addB = a => a + b;     // CAPTURING closure
		List<int32> ys = Map<int32, int32>(xs, addB); // into the GENERIC Map
		function int32(int32, int32) plus = (acc, x) => acc + x;
		return Fold<int32, int32>(ys, 0, plus);       // 11 + 12 + 13 = 36
	}
}
