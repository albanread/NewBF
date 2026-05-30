// expect: 15
// do/while runs the body before the first test: i = 1,2,3,4,5 -> sum 15.
class Program {
	public static int32 Main() {
		int32 i = 0;
		int32 sum = 0;
		do {
			i++;
			sum += i;
		} while (i < 5);
		return sum;
	}
}
