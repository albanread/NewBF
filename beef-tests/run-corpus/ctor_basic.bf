// expect: 7
// A constructor that stores its argument into a field (via `this.`).
class Box {
	public int32 v;
	public this(int32 init) { this.v = init; }
}
class Program {
	public static int32 Main() {
		Box b = new Box(7);
		int32 r = b.v;
		delete b;
		return r;
	}
}
