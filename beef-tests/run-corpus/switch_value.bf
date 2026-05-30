// expect: 20
// Value switch with an early return from each arm; x = 2 hits `case 2`.
class Program {
	public static int32 Main() {
		int32 x = 2;
		switch (x) {
		case 1:
			return 10;
		case 2:
			return 20;
		case 3:
			return 30;
		default:
			return 99;
		}
	}
}
