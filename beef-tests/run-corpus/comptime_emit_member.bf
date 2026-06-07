// expect: 42
// CB-T4: THE comptime marquee. A `[Comptime, EmitGenerator]` generator emits a
// method into its owning class via `Compiler.EmitTypeBody(...)`. The emitted
// `Sum()` reads the class's PRE-EXISTING fields `mX`/`mY` — a value computable
// only if emission fed back into resolution: the source-emitted member was
// re-parsed (as `extension Vec2 { … }`), re-analyzed, re-lowered, and is now
// callable AND correctly reads the original fields. 30 + 12 = 42.
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
		Vec2 v = new Vec2(30, 12);
		int32 r = v.Sum();
		delete v;
		return r;
	}
}
