// expect: 15
// foreach over a closed range `1...5`: i takes 1,2,3,4,5 (inclusive). Sum = 15.
class Program {
	public static int32 Main() {
		int32 sum = 0;
		for (var i in 1...5) { sum += i; }   // 1+2+3+4+5 = 15
		return sum;
	}
}
