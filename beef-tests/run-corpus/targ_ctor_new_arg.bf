// expect: 34
// TA-5: a target-typed `.(args)` value-struct-ctor shorthand passed as an
// argument to a `new T(args)` CLASS constructor. Before TA-5 the `new`-ctor path
// lowered each arg eagerly via `self.expr` (no target type), so `.(3,4)` became
// `undef`. Now `lower_new`'s `has_pending` fork resolves the ctor via
// `ctor_for_partial` (arity + shape gate) and back-fills `.(3,4)` against the
// resolved `Vec2` ctor param, constructing it in place and passing it by value.
//   Holder(.(3,4)) → field v = Vec2(3,4); v.x*10 + v.y = 30 + 4 = 34.
struct Vec2 {
	public int32 x;
	public int32 y;
	public this(int32 x, int32 y) { this.x = x; this.y = y; }
}
class Holder {
	public Vec2 v;
	public this(Vec2 v) { this.v = v; }   // value-struct ctor param
}
class Program {
	public static int32 Main() {
		Holder h = new Holder(.(3, 4)); // `.(3,4)` target-types to Vec2
		int32 r = h.v.x * 10 + h.v.y;   // 30 + 4 = 34
		delete h;
		return r;
	}
}
