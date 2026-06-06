// expect: 1131
// String interpolation `$"…{expr}…"`: builds a new String by appending each
// literal run and each hole's value via the type-matched String.Append. Here a
// String hole ({who}), an int hole ({n}), and literal text combine into
// "hi Bob! n=42" — length 12, last char '2' (= 50). Result = 12*90 + 50 + 1 = 1131.
class Program {
	public static int32 Main() {
		String who = "Bob";
		int n = 42;
		String s = $"hi {who}! n={n}";   // "hi Bob! n=42"
		int32 len = (int32)s.Length();   // 12
		char8 last = s.CharAt(len - 1);  // '2' = 50
		delete who;
		delete s;
		return len * 90 + last + 1;      // 1080 + 50 + 1 = 1131
	}
}
