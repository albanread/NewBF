// expect: 19
// Shape-gated overload resolution (§3.2). Two overloads of `M` share arity 2:
//   M(Vec2 v, int32 k)  → v.x + v.y + k   (struct overload)
//   M(int32 n, int32 k) → n + k           (primitive overload)
// Called as `M(.(3,4), 5)`: the pending `.(3,4)` is a `.(args)` ctor shorthand,
// compatible ONLY with a value `Struct(_)`. The shape gate DISQUALIFIES the
// `M(int32,int32)` candidate (a `.(…)` can't target a primitive) and selects the
// `Vec2` overload, which back-fills `.(3,4)` against the `Vec2` param.
//   M(.(3,4), 5) = 3 + 4 + 5 = 12 + 7? -> 3+4+5 = 12 ... see below
// (If the wrong/primitive overload were picked, the `.(3,4)` would be undef and
// the result would not be the struct-branch value.)
struct Vec2 {
	public int32 x;
	public int32 y;
	public this(int32 x, int32 y) { this.x = x; this.y = y; }
}
class Use {
	public int32 M(Vec2 v, int32 k) { return v.x + v.y + k + 7; } // struct branch marker (+7)
	public int32 M(int32 n, int32 k) { return n + k; }            // primitive branch
}
class Program {
	public static int32 Main() {
		Use u = new Use();
		int32 r = u.M(.(3, 4), 5); // must pick M(Vec2,int): 3+4+5+7 = 19
		delete u;
		return r; // 19
	}
}
