// expect: 21
// `break` leaves the loop early: accumulate 1,2,3,... and stop once >= 20.
class Program {
	public static int32 Main() {
		int32 sum = 0;
		for (int32 i = 1; i <= 100; i++) {
			sum += i;
			if (sum >= 20) {
				break;
			}
		}
		return sum;
	}
}
