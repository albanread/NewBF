// expect: 1333
// corlib String operator+: `a + b` (String+String) and `a + c` (String+char8)
// build new owned Strings via the overloaded static `operator+`, selected by
// operand type. "Hello, " + "World" + '!' = "Hello, World!" (length 13, last
// char '!' = 33). Result = 13*100 + 33 = 1333.
class Program {
	public static int32 Main() {
		String a = "Hello, ";
		String b = "World";
		String c = a + b;          // "Hello, World" (len 12)
		String d = c + '!';        // "Hello, World!" (len 13)
		int32 r = (int32)d.Length() * 100 + d.CharAt(12);  // 1300 + 33 = 1333
		delete a;
		delete b;
		delete c;
		delete d;
		return r;
	}
}
