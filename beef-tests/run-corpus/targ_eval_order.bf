// expect: 12
// Evaluation-order pin (§3.1, correctness blocker #1). The documented rule:
// CONCRETE args are emitted in Phase 1 in SOURCE ORDER; pending args are emitted
// in Phase 2 in source order. The only observable reorder vs the eager path is
// "pending observed AFTER concrete" — we do NOT claim full eval-order equivalence.
//
// This program pins the concrete-args-in-source-order guarantee. `M(g(), .(1,2),
// h())` has a pending `.(1,2)` between two side-effecting concrete args. Each of
// g/h bumps a shared `tick` and stamps its own order. The result `gOrder*10 +
// hOrder` is 12 IFF g (source-position 0) ran before h (source-position 2). A
// reorder (h before g) would yield 21.
class C {
	public static int32 Tick;
	public static int32 GOrder;
	public static int32 HOrder;
}
struct Vec2 {
	public int32 x;
	public int32 y;
	public this(int32 x, int32 y) { this.x = x; this.y = y; }
}
class Use {
	// The pending `.(1,2)` slot is in the MIDDLE; its construction is observed in
	// Phase 2, after both concrete args — that is the documented caveat, not tested
	// here. What IS tested: g before h (concrete source order).
	public int32 M(int32 a, Vec2 v, int32 b) { return a + v.x + v.y + b; }
}
class Program {
	static int32 G() { C.Tick = C.Tick + 1; C.GOrder = C.Tick; return 0; }
	static int32 H() { C.Tick = C.Tick + 1; C.HOrder = C.Tick; return 0; }
	public static int32 Main() {
		Use u = new Use();
		// g() concrete (pos 0), .(1,2) pending (pos 1), h() concrete (pos 2).
		int32 ignore = u.M(G(), .(1, 2), H());
		delete u;
		return C.GOrder * 10 + C.HOrder; // 1*10 + 2 = 12 (g before h)
	}
}
