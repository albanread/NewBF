// expect: 46
// Operator overloading: a user `struct` defines static `operator+` / `operator*`,
// and `a + b` / `a * b` dispatch to them by operand type. Scalar arithmetic is
// untouched (it never sees a struct operand). Each operator returns a new Vec.
//   a = (2, 3), b = (4, 5)
//   a + b = (6, 8)        ; (a + b).x + (a + b).y = 14
//   a * b = (8, 15)       ; (a * b).x + (a * b).y = 23
//   plus a scalar 9 (proves the kernel path still works) ⇒ 14 + 23 + 9 = 46
struct Vec {
	public int32 x;
	public int32 y;

	public static Vec operator+(Vec a, Vec b) {
		Vec r;
		r.x = a.x + b.x;
		r.y = a.y + b.y;
		return r;
	}
	public static Vec operator*(Vec a, Vec b) {
		Vec r;
		r.x = a.x * b.x;
		r.y = a.y * b.y;
		return r;
	}
}
class Program {
	public static int32 Main() {
		Vec a;
		a.x = 2;
		a.y = 3;
		Vec b;
		b.x = 4;
		b.y = 5;

		Vec sum = a + b;
		Vec prod = a * b;
		int32 scalar = 4 + 5;
		return (sum.x + sum.y) + (prod.x + prod.y) + scalar; // 14 + 23 + 9 = 46
	}
}
