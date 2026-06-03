// expect: 11
// A method-scoped `defer` runs before the method returns. Fill sets the box to
// 10, defers a +1, then returns early; the defer still fires (the caller sees
// 11). Also checks the return *value* is captured before defers run: Bump
// returns the pre-defer value.
class Box { public int32 v; }
class Program {
	static void Fill(Box b) {
		defer b.v = b.v + 1;   // runs at method exit, after the body below
		b.v = 10;
		return;                // early return — defer still fires
	}
	static int32 Bump(Box b) {
		defer b.v = b.v + 100; // mutates b after the return value is captured
		return b.v;            // returns the value as of here (pre-defer)
	}
	public static int32 Main() {
		Box b = new Box();
		Fill(b);               // b.v = 11
		int32 seen = Bump(b);  // returns 11 (pre-defer), then b.v becomes 111
		int32 r = seen + (b.v - 111);  // 11 + 0 = 11
		delete b;
		return r;
	}
}
