// expect: 5
// A string literal in a `String` context becomes a real corlib String object
// (target-typed via coerce): `String s = "Hello"` constructs `new String("Hello")`.
class Program {
	public static int32 Main() {
		String s = "Hello";
		int32 n = s.Length();
		delete s;
		return n;
	}
}
