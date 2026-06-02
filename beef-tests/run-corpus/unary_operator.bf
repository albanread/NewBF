// expect: 12
// Unary operator overloading: a one-arg static `operator-` negates a Vec. The
// one-param signature distinguishes it from a (two-param) binary `operator-`.
//   a = (3, -7)  =>  -a = (-3, 7)
//   return n.y + (-n.x) + 2 = 7 + 3 + 2 = 12   (the inner -n.x is scalar negate)
struct Vec {
	public int32 x;
	public int32 y;

	public static Vec operator-(Vec a) {
		Vec r;
		r.x = -a.x;
		r.y = -a.y;
		return r;
	}
}
class Program {
	public static int32 Main() {
		Vec a;
		a.x = 3;
		a.y = -7;
		Vec n = -a;          // (-3, 7)
		return n.y + (-n.x) + 2; // 7 + 3 + 2 = 12
	}
}
