// expect: 42
// Generic value struct, monomorphized per concrete type: Pair<int> (i64 fields)
// and Pair<int32> (i32 fields) are *distinct* layouts produced from one
// template. Proves type-parameter substitution + one concrete type per
// instantiation.
//   p: 10 + 30 = 40 (int)   q: 1 + 1 = 2 (int32)   => 42
struct Pair<T> {
	public T a;
	public T b;
}
class Program {
	public static int32 Main() {
		Pair<int> p;
		p.a = 10;
		p.b = 30;
		Pair<int32> q;
		q.a = 1;
		q.b = 1;
		return p.a + p.b + q.a + q.b;
	}
}
