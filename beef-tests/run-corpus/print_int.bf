// expect: 12345
// Console.WriteLine(int) — a typed overload beside WriteLine(String), chosen by
// argument type, rendering decimal via an itoa over String.Append. The value
// harness checks the return; the console_output capture test checks the digits.
class Program {
	public static int32 Main() {
		int32 n = 12345;
		Console.WriteLine(n);
		return n;
	}
}
