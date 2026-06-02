// expect: 45
// foreach over a half-open range `0..<10`: i takes 0,1,...,9. Sum = 45.
class Program {
	public static int32 Main() {
		int32 sum = 0;
		for (var i in 0..<10) { sum += i; }   // 0+1+...+9 = 45
		return sum;
	}
}
