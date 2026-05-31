// expect: 142
// Generational handles (corlib Pool): a handle resolves to its object while
// live, and to null once Freed — use-after-free becomes a detectable null, not
// a dangling pointer. The safe-reference idea from GC.md §12 as an optional
// manual primitive (no GC).
//   live Get -> b.v (42); after Free, stale Get -> null (+100) => 142
class Box {
	public int v;
	public this(int x) { this.v = x; }
}
class Program {
	public static int32 Main() {
		Pool p = new Pool();
		Box b = new Box(42);
		int h = p.Alloc(b);

		Box got = p.Get(h);          // live -> b
		int32 r = got.v;             // 42

		p.Free(h);
		void* stale = p.Get(h);      // stale -> null
		if (stale == null) {
			r = r + 100;
		}

		delete b;
		delete p;
		return r;                    // 142
	}
}
