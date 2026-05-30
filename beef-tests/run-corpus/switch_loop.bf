// expect: 60
// switch inside a loop: map 1->10 (explicit break), 2->20 (implicit arm exit),
// everything else -> 30 via default. Sum over i in 1..3 = 10+20+30 = 60.
class Program {
	public static int32 Main() {
		int32 sum = 0;
		for (int32 i = 1; i <= 3; i++) {
			switch (i) {
			case 1:
				sum += 10;
				break;
			case 2:
				sum += 20;
			default:
				sum += 30;
			}
		}
		return sum;
	}
}
