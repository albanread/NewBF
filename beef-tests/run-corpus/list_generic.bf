// expect: 42
// corlib List<T>, monomorphized: List<int> (8-byte stride) grows past its
// initial capacity; List<int32> (4-byte stride) is a distinct instantiation.
// Proves a generic collection end to end — new, Add, Get, Count, grow, delete.
//   xs = [1..6] sum 21 (forces one Grow);  ys = [7,14] sum 21  =>  42
class Program {
	public static int32 Main() {
		List<int> xs = new List<int>();
		for (int32 j = 1; j <= 6; j++) {
			xs.Add(j);
		}
		int sum = 0;
		for (int32 i = 0; i < xs.Count(); i++) {
			sum = sum + xs.Get(i);
		}

		List<int32> ys = new List<int32>();
		ys.Add(7);
		ys.Add(14);
		int32 ysum = 0;
		for (int32 i = 0; i < ys.Count(); i++) {
			ysum = ysum + ys.Get(i);
		}

		int32 r = sum + ysum;
		delete xs;
		delete ys;
		return r;
	}
}
