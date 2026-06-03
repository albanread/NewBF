// expect: 45
// Local (nested) functions: declared inside a method body, emitted as their own
// free functions and called by name. Non-capturing — they take everything as
// parameters. Square and Clamp are independent helpers used by Main.
class Program {
	public static int32 Main() {
		int32 Square(int32 x) { return x * x; }
		int32 Clamp(int32 v, int32 hi) { return v > hi ? hi : v; }
		int32 s = Square(6) + Square(2);     // 36 + 4 = 40
		int32 c = Clamp(Square(3), 5);       // min(9, 5) = 5
		return s + c;                        // 45
	}
}
