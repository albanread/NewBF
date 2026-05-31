// expect: 42
// A method calling a sibling instance method through `this` (with an argument):
// PlusTwice(d) = this.Plus(d) + d = (v + d) + d. With v=40, d=1 -> 42.
class Box {
	public int32 v;
	public this(int32 init) { this.v = init; }
	public int32 Plus(int32 d) { return this.v + d; }
	public int32 PlusTwice(int32 d) { return this.Plus(d) + d; }
}
class Program {
	public static int32 Main() {
		Box b = new Box(40);
		int32 r = b.PlusTwice(1);
		delete b;
		return r;
	}
}
