// expect: 164
// Array collection initializer `new T[N] { v0, v1, … }`: the brace entries are
// the element values (sized and inferred-length forms). Three constant ints stay
// off the JIT __xmm constant-pool path (the 4+ threshold from §76).
class Program {
	public static int32 Main() {
		int32[] a = new int32[3] { 10, 20, 30 };   // Count 3
		int32[] b = new int32[] { 100 };           // Count 1
		int32 r = a[0] + a[1] + a[2]               // 60
			+ (int32)a.Count + (int32)b.Count       // + 4
			+ b[0];                                 // + 100
		delete a;
		delete b;
		return r;                                  // 164
	}
}
