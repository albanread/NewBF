// expect: 15
// Nested value struct + chained member access (o.inner.v): a struct field
// whose type is itself a struct, GEP'd through two levels.
struct Inner { public int32 v; }
struct Outer { public Inner inner; public int32 w; }
class Program {
	public static int32 Main() {
		Outer o = ?;
		o.inner.v = 10;
		o.w = 5;
		return o.inner.v + o.w;
	}
}
