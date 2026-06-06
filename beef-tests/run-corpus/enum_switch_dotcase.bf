// expect: 1322
// Switch over an int-backed enum with bare `.Case` labels, including a
// multi-value case. The bare `.Case` resolves against the scrutinee's enum
// (int-backed enums lower to int32, so the enum name comes from the scrutinee).
// Classify maps Red→.Red,.Blue arm (1), Green→2, Blue→1, and a value with no
// matching case → default (9). Sum over {Red, Green, Blue, Green} with a base.
enum Color { Red, Green, Blue }
class Program {
	static int32 Classify(Color c) {
		switch (c) {
		case .Red, .Blue: return 1;
		case .Green: return 2;
		default: return 9;
		}
	}
	public static int32 Main() {
		int32 a = Program.Classify(Color.Red);    // 1
		int32 b = Program.Classify(Color.Green);  // 2
		int32 c = Program.Classify(Color.Blue);   // 1
		// 1*1000 + 2*100 + 1*10 + 2 = 1212; +110 keeps it distinctive → 1322
		return a * 1000 + b * 100 + c * 10 + b + 110;
	}
}
