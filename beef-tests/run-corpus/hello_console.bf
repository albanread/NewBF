// expect: 13
// The first program that talks back: Console.WriteLine prints through corlib
// (GetStdHandle + WriteFile). The value harness can't see stdout, so Main also
// returns the string length (13) to gate that it ran end-to-end; the exact
// bytes printed are asserted by the console_output capture test.
class Program {
	public static int32 Main() {
		String s = "Hello, world!";
		Console.WriteLine(s);
		int32 n = s.Length();
		delete s;
		return n;
	}
}
