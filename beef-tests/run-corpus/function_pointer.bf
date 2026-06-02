// expect: 12
// Function pointers — the foundation lambdas build on. Take the code address of
// a named static method into a `function R(P)` local, then call it indirectly.
class Math2 {
	public static int32 Twice(int32 x) { return x + x; }
}
class Program {
	public static int32 Main() {
		function int32(int32) f = Math2.Twice;   // method reference -> code ptr
		return f(6);                              // indirect call -> Twice(6) = 12
	}
}
