// expect: 75
// Compound assignment through an indexer: `g[i] += v` reads via `get_this`,
// adds, and writes via `set_this`, evaluating the receiver and index once.
//   g[0]=10, g[1]=20, g[2]=30
//   g[0] += 5   → 15
//   g[1] *= 2   → 40   (compound `*=` through the indexer too)
//   g[2] -= 10  → 20
//   r = g[0] + g[1] + g[2] = 15 + 40 + 20 = 75
class Grid {
	int32* data;
	public this() { this.data = Internal.Malloc(4 * 4); }
	public ~this() { Internal.Free(this.data); }
	public int32 this[int32 i] {
		get { return this.data[i]; }
		set { this.data[i] = value; }
	}
}
class Program {
	public static int32 Main() {
		Grid g = new Grid();
		g[0] = 10;
		g[1] = 20;
		g[2] = 30;

		g[0] += 5;
		g[1] *= 2;
		g[2] -= 10;

		int32 r = g[0] + g[1] + g[2];
		delete g;
		return r; // 75
	}
}
