// expect: 60
// `for (var x in xs)` over a List<int32>: the compiler lowers it to an indexed
// loop over Count()/Get(i), binding x to each element. (Previously foreach was
// parsed but never lowered — the loop body was silently skipped.)
class Program {
	public static int32 Main() {
		List<int32> xs = new List<int32>();
		xs.Add(10);
		xs.Add(20);
		xs.Add(30);
		int32 sum = 0;
		for (var x in xs) { sum += x; }   // 10 + 20 + 30 = 60
		delete xs;
		return sum;
	}
}
