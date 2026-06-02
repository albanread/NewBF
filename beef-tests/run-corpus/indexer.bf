// expect: 60
// User-defined indexer: `Type this[int i] { get; set; }`. `g[i]` reads call
// `get_this(g, i)`; `g[i] = v` calls `set_this(g, i, v)`. The bracket param `i`
// is bound in each accessor body like a method parameter, threaded between
// `this` and (for the setter) the implicit `value`.
//   g[0]=10, g[1]=20, g[2]=g[0]+g[1]=30
//   r = g[0] + g[1] + g[2] = 10 + 20 + 30 = 60
class Grid {
	int32* data;
	public this() {
		this.data = Internal.Malloc(4 * 4); // room for 4 int32s
	}
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
		g[2] = g[0] + g[1]; // 30
		int32 r = g[0] + g[1] + g[2];
		delete g;
		return r; // 60
	}
}
