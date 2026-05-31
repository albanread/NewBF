// expect: 99
// A class with both a constructor and a destructor: `delete` calls ~this()
// before freeing. (The dtor is empty here — this exercises the call path.)
class Tracked {
	public int32 v;
	public this(int32 init) { this.v = init; }
	public ~this() { }
}
class Program {
	public static int32 Main() {
		Tracked t = new Tracked(99);
		int32 r = t.v;
		delete t;
		return r;
	}
}
