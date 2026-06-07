// expect: 42
// CB-T4: the emitted member is a reusable symbol — call it from two sites with
// different receivers. Emission is idempotent (one generator, emitted once); the
// generated `Sum()` resolves like any hand-written method and is callable any
// number of times. (10+5) + (20+7) = 15 + 27 = 42.
class Vec2 {
	public int32 mX;
	public int32 mY;
	public this(int32 x, int32 y) { this.mX = x; this.mY = y; }

	[Comptime, EmitGenerator]
	public static void Generate() {
		Compiler.EmitTypeBody("public int32 Sum() { return this.mX + this.mY; }");
	}
}

class Program {
	public static int32 Main() {
		Vec2 a = new Vec2(10, 5);
		Vec2 b = new Vec2(20, 7);
		int32 r = a.Sum() + b.Sum();
		delete a;
		delete b;
		return r;
	}
}
