// expect: 1010
// List<int32>.IndexOf(T) returns the first index whose element == value (or -1);
// Contains(T) is IndexOf(value) >= 0.
//   xs = [10, 20, 30]  (indices 0,1,2)
//   idx20    = IndexOf(20) = 1   (20 is at index 1)
//   absent   = IndexOf(99) = -1  (99 not in the list)
//   has30    = Contains(30) = true
//   has99    = Contains(99) = false
// Pack into one int, each result in its own decimal slot:
//   idx20 * 1000              = 1 * 1000          = 1000
//   (absent + 1) * 100        = (-1 + 1) * 100     =    0
//   has30 -> +10 (true)                            =   10
//   has99 -> + 1 (false, so 0)                     =    0
//   total                                          = 1010
class Program {
	public static int32 Main() {
		List<int32> xs = new List<int32>();
		xs.Add(10);
		xs.Add(20);
		xs.Add(30);

		int32 idx20 = xs.IndexOf(20);
		int32 absent = xs.IndexOf(99);
		int32 r = idx20 * 1000 + (absent + 1) * 100;
		if (xs.Contains(30)) { r = r + 10; }
		if (xs.Contains(99)) { r = r + 1; }

		delete xs;
		return r;
	}
}
