// expect: 96
// Array initializers: `new T[](v…)` infers the length from the value count;
// `new T[N](v…)` uses the explicit size. Both store each value into its slot.
// The 4-element array uses runtime-derived values on purpose — an all-constant
// initializer of 4+ ints lets LLVM merge the stores into a vector-constant store
// (`__xmm@…`), a constant-pool symbol the RTDyld JIT can't materialize (the same
// limitation as `__real@` floats). AOT is unaffected; this keeps the JIT corpus
// green while still exercising both initializer forms and lengths.
class Program {
	public static int32 Main() {
		int32 k = 1;
		int32[] a = new int32[](10, 20, 30);          // Count 3, sum 60
		int32[] b = new int32[4](k, k + 1, k + 2, k + 3);  // 1,2,3,4 (runtime)
		int32 sum = 0;
		for (var v in a) { sum += v; }                // 60
		for (var v in b) { sum += v; }                // + 10 = 70
		int32 r = sum + (int32)a.Count * 10 - (int32)b.Count;  // 70 + 30 - 4 = 96
		delete a;
		delete b;
		return r;
	}
}
