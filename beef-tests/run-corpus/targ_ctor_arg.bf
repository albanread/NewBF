// expect: 25
// TA-3 headline: a target-typed `.(args)` value-struct-ctor shorthand as a
// CALL ARGUMENT. Before TA-3 a `.(3,4)` arg lowered to `undef` (no target type
// at the arg loop); now the `has_pending` fork picks the overload by arity +
// shape, then back-fills the `.(3,4)` against the resolved `Vec2` param type and
// constructs it in place — passed by value to `Dot`.
//   Dot(.(3,4)) = 3*3 + 4*4 = 9 + 16 = 25
struct Vec2 {
	public int32 x;
	public int32 y;
	public this(int32 x, int32 y) { this.x = x; this.y = y; }
}
class Use {
	public int32 Dot(Vec2 v) { return v.x * v.x + v.y * v.y; }
}
class Program {
	public static int32 Main() {
		Use u = new Use();
		int32 r = u.Dot(.(3, 4)); // `.(3,4)` target-types to Vec2
		delete u;
		return r;
	}
}
