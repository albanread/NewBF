// expect: 42
// Two type parameters, substituted independently and order-sensitively:
// Pair<int, int32> (A=i64, B=i32) and Pair<int32, int> (A=i32, B=i64) are
// distinct monomorphs (Pair$i64i32 vs Pair$i32i64).
//   p.first 30 + p.second 12 + q (0 + 0) = 42
struct Pair<A, B> {
	public A first;
	public B second;
}
class Program {
	public static int32 Main() {
		Pair<int, int32> p;
		p.first = 30;
		p.second = 12;
		Pair<int32, int> q;
		q.first = 0;
		q.second = 0;
		return p.first + p.second + q.first + q.second;
	}
}
