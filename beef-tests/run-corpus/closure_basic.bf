// expect: 57
// Closures — Beefy-Lisp! A lambda captures outer locals into a heap environment
// `[code_ptr | captures…]`; the function value is that env pointer, and a call
// passes it back as a hidden `$self` so the body can read its captures.
//   get  = () => n        captures n (paramless)
//   addB = a => a + b      captures b, takes a param a
class Program {
	public static int32 Main() {
		int32 n = 42;
		function int32() get = () => n;            // closes over n
		int32 b = 10;
		function int32(int32) addB = a => a + b;   // closes over b, param a
		return get() + addB(5);                     // 42 + (5 + 10) = 57
	}
}
