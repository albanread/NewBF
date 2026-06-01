// expect: 142
// Handle<T> (corlib): a typed, generation-checked reference over Pool — the
// type-safe capstone of the RNIM thread, built from generics + Pool with no GC.
//   live h.Get(p) -> t.v (42); after p.Free, h.Get(p) -> null (+100) => 142
class Thing {
	public int v;
	public this(int x) { this.v = x; }
}
class Program {
	public static int32 Main() {
		Pool p = new Pool();
		Thing t = new Thing(42);
		int raw = p.Alloc(t);
		Handle<Thing> h = new Handle<Thing>(raw);

		Thing got = h.Get(p);        // live -> t
		int32 r = got.v;             // 42

		p.Free(raw);
		Thing stale = h.Get(p);      // stale -> null
		if (stale == null) {
			r = r + 100;
		}

		delete t;
		delete h;
		delete p;
		return r;                    // 142
	}
}
