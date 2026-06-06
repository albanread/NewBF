// expect: 14
// Value-struct construction via the `.(args)` constructor shorthand: in a local
// init (`Vec2 a = .(2,3)`), as an operator's `return .(…)`, exercising the
// constructor (which writes fields through `this`). `a+b` runs operator+ to make
// (6,8); Dot with (1,1) → 6*1 + 8*1 = 14.
struct Vec2 {
	public int32 x;
	public int32 y;
	public this(int32 x, int32 y) { this.x = x; this.y = y; }
	public static Vec2 operator+(Vec2 a, Vec2 b) { return .(a.x + b.x, a.y + b.y); }
	public int32 Dot(Vec2 o) { return this.x * o.x + this.y * o.y; }
}
class Program {
	public static int32 Main() {
		Vec2 a = .(2, 3);
		Vec2 b = .(4, 5);
		Vec2 c = a + b;      // (6, 8)
		Vec2 one = .(1, 1);
		return c.Dot(one);   // 6 + 8 = 14
	}
}
