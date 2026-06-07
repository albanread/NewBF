// expect: 12
// TA-4: a target-typed `.(args)` value-struct-ctor shorthand passed to a BARE /
// FREE STATIC function (the `Expr::Ident`-callee call path, not a `obj.M`/`Type.M`
// member call). Before TA-4 a `.(3,4)` arg to such a call lowered to `undef` (the
// `Expr::Ident`-callee arg loop had no target type); now the `has_pending` fork
// resolves the same-type overload by arity + shape (`pick_overload_partial`), then
// `finish_args` back-fills the `.(3,4)` against the resolved `Vec2` param type and
// constructs it in place — passed by value to `Area`.
//   Area(.(3,4)) = 3 * 4 = 12
struct Vec2 {
	public int32 x;
	public int32 y;
	public this(int32 x, int32 y) { this.x = x; this.y = y; }
}
class Program {
	static int32 Area(Vec2 v) { return v.x * v.y; }
	public static int32 Main() {
		// `Area` is called BARE (same-type static), so it routes through the
		// `Expr::Ident`-callee path — TA-4's site. `.(3,4)` target-types to Vec2.
		return Area(.(3, 4)); // 12
	}
}
