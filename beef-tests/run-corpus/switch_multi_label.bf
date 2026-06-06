// expect: 330
// Multi-value case labels `case a, b, c:` — the arm matches if the scrutinee
// equals any listed value. Sum the arm result for x in {2, 3, 7}: 2 and 3 both
// hit `case 1,2,3 → 100`; 7 hits `case 6,7 → 30`. 100 + 100 + 30 = 230... plus
// a default hit for 9 → 9 makes it deterministic. Total = 100+100+30+100 = 330.
class Program {
	static int32 Classify(int32 x) {
		switch (x) {
		case 1, 2, 3: return 100;
		case 6, 7: return 30;
		default: return 0;
		}
	}
	public static int32 Main() {
		return Program.Classify(2)   // 100
		     + Program.Classify(3)   // 100
		     + Program.Classify(7)   // 30
		     + Program.Classify(1);  // 100
	}
}
