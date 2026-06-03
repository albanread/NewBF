// expect: 6070
// List<T>.AddRange appends another list's elements; First/Last read the ends.
//   a = [1, 2, 3]; b = [40, 50]; a.AddRange(b) → [1, 2, 3, 40, 50]
//   Count=5, Last=50, First=1, Get(3)=40
//   r = 5*1000 + 50*20 + 1*30 + 40 = 5000 + 1000 + 30 + 40 = 6070
class Program {
	public static int32 Main() {
		List<int32> a = new List<int32>();
		a.Add(1);
		a.Add(2);
		a.Add(3);
		List<int32> b = new List<int32>();
		b.Add(40);
		b.Add(50);

		a.AddRange(b);

		int32 r = a.Count() * 1000 + a.Last() * 20 + a.First() * 30 + a.Get(3);
		delete a;
		delete b;
		return r;
	}
}
