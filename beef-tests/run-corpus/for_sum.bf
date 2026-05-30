// expect: 55
// C-style for with a postfix-increment update: sum 1..10.
class Program {
	public static int32 Main() {
		int32 sum = 0;
		for (int32 i = 1; i <= 10; i++) {
			sum += i;
		}
		return sum;
	}
}
