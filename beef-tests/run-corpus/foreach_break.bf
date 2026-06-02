// expect: 7
// break + continue inside a foreach: `continue` skips 3 (targets the loop's
// increment), `break` stops at 5 (targets the loop exit). Sum = 1 + 2 + 4 = 7.
class Program {
	public static int32 Main() {
		List<int32> xs = new List<int32>();
		xs.Add(1);
		xs.Add(2);
		xs.Add(3);
		xs.Add(4);
		xs.Add(5);
		xs.Add(6);
		int32 sum = 0;
		for (var x in xs) {
			if (x == 3) { continue; }
			if (x == 5) { break; }
			sum += x;
		}
		delete xs;
		return sum;
	}
}
