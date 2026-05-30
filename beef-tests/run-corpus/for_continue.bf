// expect: 25
// `continue` skips the even values, so this sums the odds 1+3+5+7+9.
class Program {
	public static int32 Main() {
		int32 sum = 0;
		for (int32 i = 1; i <= 10; i++) {
			if (i % 2 == 0) {
				continue;
			}
			sum += i;
		}
		return sum;
	}
}
