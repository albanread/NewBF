// expect: 1234
// TA-6: NESTED target-typed `.( .(…), .(…) )` value-struct construction. The
// OUTER `.(…)` target-types to `Line` (a struct-of-structs); its inner args are
// themselves pending `.(…)` forms. `construct_value_struct`'s `has_pending` fork
// resolves the `Line(Vec2, Vec2)` ctor via `ctor_for_partial`, then `finish_args`
// lowers each inner `.(…)` against its `Vec2` ctor-param type. Each inner lower
// re-enters `construct_value_struct` (REENTRANT) and runs its OWN two-phase pass
// on its OWN stack-local vectors/slot — proving nested two-phase construction
// with no shared-state clobber and exactly one construction per pending arg.
//   l.a = Vec2(1,2), l.b = Vec2(3,4)
//   l.a.x*1000 + l.a.y*100 + l.b.x*10 + l.b.y = 1000 + 200 + 30 + 4 = 1234
struct Vec2 {
	public int32 x;
	public int32 y;
	public this(int32 x, int32 y) { this.x = x; this.y = y; }
}
struct Line {
	public Vec2 a;
	public Vec2 b;
	public this(Vec2 a, Vec2 b) { this.a = a; this.b = b; }
}
class Program {
	public static int32 Main() {
		Line l = .( .(1, 2), .(3, 4) ); // outer .(…) builds Line; inner .(…) build Vec2
		return l.a.x * 1000 + l.a.y * 100 + l.b.x * 10 + l.b.y; // 1234
	}
}
