// expect: 60
// Collection initializer `new List<T>() { v0, v1, … }`: a class with an `Add`
// method gets each bare entry added in order (the object-initializer machinery
// routes a non-`field = value` entry to `Add`). Equivalent to constructing then
// calling Add three times.
class Program {
	public static int32 Main() {
		List<int32> xs = new List<int32>() { 10, 20, 30 };
		int32 sum = 0;
		for (var v in xs) { sum += v; }   // 60
		delete xs;
		return sum;
	}
}
