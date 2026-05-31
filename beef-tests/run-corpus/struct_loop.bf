// expect: 55
// Compound-assign into a struct field across a loop: sum 1..10 = 55.
struct Acc { public int32 sum; }
class Program {
	public static int32 Main() {
		Acc a = ?;
		a.sum = 0;
		for (int32 i = 1; i <= 10; i++) {
			a.sum += i;
		}
		return a.sum;
	}
}
