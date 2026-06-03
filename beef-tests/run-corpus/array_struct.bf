// expect: 90
// Arrays of a value struct (8-byte elements via DataLayout `sizeof`) and of
// int64. Element compound-assignment `a[i] += v` goes through the typed-pointer
// index lvalue. Exercises per-element struct field writes too.
struct Pair { public int32 a; public int32 b; }
class Program {
	public static int32 Main() {
		Pair[] ps = new Pair[3];
		for (int32 i = 0; i < 3; i++) {
			ps[i].a = i + 1;       // 1,2,3
			ps[i].b = (i + 1) * 10; // 10,20,30
		}
		int32 sum = 0;
		for (int32 i = 0; i < ps.Count; i++) {
			sum += ps[i].a + ps[i].b;  // (1+10)+(2+20)+(3+30) = 66
		}
		int64[] xs = new int64[2];
		xs[0] = 8;
		xs[1] = 16;
		xs[0] += xs[1];            // 24
		int32 r = sum + (int32)xs[0];  // 66 + 24 = 90
		delete ps;
		delete xs;
		return r;
	}
}
