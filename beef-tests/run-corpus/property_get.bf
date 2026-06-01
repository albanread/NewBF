// expect: 52
// Computed (get-only) properties: Area is a block-bodied getter, Perimeter an
// expression-bodied one. Reading `r.Area` lowers to a get_Area() call (no
// backing field — the getter computes from real fields). Auto-properties and
// setters are a later slice.
class Rect {
	public int32 w;
	public int32 h;
	public this(int32 w, int32 h) { this.w = w; this.h = h; }
	public int32 Area { get { return this.w * this.h; } }
	public int32 Perimeter { get => 2 * (this.w + this.h); }
}
class Program {
	public static int32 Main() {
		Rect r = new Rect(5, 6);
		int32 result = r.Area + r.Perimeter;   // 30 + 22 = 52
		delete r;
		return result;
	}
}
