// expect: 21
// Field independence: read a member into a local, then swap the two fields.
struct Pair { public int32 a; public int32 b; }
class Program {
	public static int32 Main() {
		Pair p = ?;
		p.a = 1;
		p.b = 2;
		int32 tmp = p.a;
		p.a = p.b;
		p.b = tmp;
		return p.a * 10 + p.b;
	}
}
