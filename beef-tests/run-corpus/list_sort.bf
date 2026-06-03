// expect: 11345
// List<T>.Sort() — in-place ascending insertion sort using `<` on the element
// type (monomorphized for int32). [3,1,4,1,5] sorts to [1,1,3,4,5]; the digits
// are read back positionally to verify order.
class Program {
	public static int32 Main() {
		List<int32> xs = new List<int32>();
		xs.Add(3);
		xs.Add(1);
		xs.Add(4);
		xs.Add(1);
		xs.Add(5);
		xs.Sort();   // [1, 1, 3, 4, 5]
		int32 r = xs[0] * 10000 + xs[1] * 1000 + xs[2] * 100 + xs[3] * 10 + xs[4];
		delete xs;
		return r;    // 11345
	}
}
