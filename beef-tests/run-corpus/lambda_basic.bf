// expect: 42
// An anonymous lambda: `() => 42` is emitted as a free function; the
// `function int32()` local holds its code address; calling it runs the body.
// (Paramless, non-capturing — the first slice of lambdas.)
class Program {
	public static int32 Main() {
		function int32() f = () => 42;
		return f();
	}
}
