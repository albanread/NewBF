// expect: 64
// List<T>.ToArray() → T[]: a generic method allocating `new T[Count]` (sized by
// T's stride through the monomorph env — the fix that lets `new T[n]` work with a
// generic element type) and copying the elements. Verifies the returned array is
// packed at the element width (4 bytes for int32) by reading it back.
class Program {
	public static int32 Main() {
		List<int32> xs = new List<int32>();
		xs.Add(10);
		xs.Add(20);
		xs.Add(34);
		int32[] a = xs.ToArray();
		int32 sum = 0;
		for (var v in a) { sum += v; }   // 64
		delete a;
		delete xs;
		return sum;
	}
}
