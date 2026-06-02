// expect: 4321
// List<T>.Insert(index, value) shifts the tail up a slot and drops `value` in;
// Reverse() flips the elements in place.
//   xs = [1, 2, 4]
//   xs.Insert(2, 3)  →  [1, 2, 3, 4]   (4 shifted up, 3 placed at index 2)
//   xs.Reverse()     →  [4, 3, 2, 1]
// Encode the final order: Get(0)*1000 + Get(1)*100 + Get(2)*10 + Get(3) = 4321.
class Program {
	public static int32 Main() {
		List<int32> xs = new List<int32>();
		xs.Add(1);
		xs.Add(2);
		xs.Add(4);

		xs.Insert(2, 3);
		xs.Reverse();

		int32 r = xs.Get(0) * 1000 + xs.Get(1) * 100 + xs.Get(2) * 10 + xs.Get(3);
		delete xs;
		return r; // 4321
	}
}
