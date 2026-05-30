// expect: 36
// Nested for loops with their own scoped counters: sum of i*j over 1..3 = 6*6.
class Program {
	public static int32 Main() {
		int32 sum = 0;
		for (int32 i = 1; i <= 3; i++) {
			for (int32 j = 1; j <= 3; j++) {
				sum += i * j;
			}
		}
		return sum;
	}
}
