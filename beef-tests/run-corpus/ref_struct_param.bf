// expect: 42
// `ref` over a *value struct*: the callee reaches the caller's struct body
// through the passed address, so writing `p.x`/`p.y` mutates the caller's `p`.
// (`struct_base` already treats a `Struct`-typed place as the body pointer, so
// the §71 by-ref binding — name bound straight to the param pointer — composes.)
struct Point { public int32 x; public int32 y; }
class Program {
	static void MoveTo(ref Point p, int32 nx, int32 ny) {
		p.x = nx;
		p.y = ny;
	}
	public static int32 Main() {
		Point p = .{ x = 1, y = 1 };
		MoveTo(ref p, 30, 12);
		return p.x + p.y;   // 42
	}
}
