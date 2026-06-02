// expect: 30
// Compound assignment on a user type uses the operator overload: `acc += v`
// lowers to `acc = acc.operator+(acc, v)` (load, call, store). Scalars still take
// the numeric path.
//   acc = (0,0); add (1,2), (3,4), (6,8) via += → (10, 14)
//   plus a scalar `s` that also uses += on an int (kernel path): s = 6
//   result: acc.x + acc.y + s = 10 + 14 + 6 = 30
struct Vec {
	public int32 x;
	public int32 y;

	public static Vec operator+(Vec a, Vec b) {
		Vec r;
		r.x = a.x + b.x;
		r.y = a.y + b.y;
		return r;
	}
}
class Program {
	static Vec V(int32 x, int32 y) {
		Vec r;
		r.x = x;
		r.y = y;
		return r;
	}
	public static int32 Main() {
		Vec acc;
		acc.x = 0;
		acc.y = 0;
		acc += V(1, 2);
		acc += V(3, 4);
		acc += V(6, 8);   // (10, 14)

		int32 s = 0;
		s += 1;
		s += 2;
		s += 3;           // 6 (scalar compound path unchanged)

		return acc.x + acc.y + s; // 10 + 14 + 6 = 30
	}
}
