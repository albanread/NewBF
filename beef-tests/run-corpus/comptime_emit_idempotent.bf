// expect: 42
// CB-T5: idempotent emission. TWO `[Comptime, EmitGenerator]` methods on the
// same owner emit the SAME (normalized) member text. The fixpoint loop's `seen`
// dedup (keyed by owner + normalized text) must splice the member EXACTLY ONCE
// — emitting it twice would produce a `duplicate member` analyze diagnostic and
// abort. That the program compiles + runs proves the dedup makes re-emission of
// identical text idempotent (the termination guarantee). The two generators emit
// cosmetically-different text (extra whitespace / a trailing `// comment`) that
// `normalize` collapses to the same key, so dedup still fires. 30 + 12 = 42.
class Vec2 {
	public int32 mX;
	public int32 mY;
	public this(int32 x, int32 y) { this.mX = x; this.mY = y; }

	[Comptime, EmitGenerator]
	public static void GenA() {
		Compiler.EmitTypeBody("public int32 Sum() { return this.mX + this.mY; }");
	}

	[Comptime, EmitGenerator]
	public static void GenB() {
		Compiler.EmitTypeBody("public  int32  Sum()  {  return this.mX + this.mY;  } // dup");
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
