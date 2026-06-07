// expect: 1
// "Exactly one construction emitted" check (TA-3 acceptance, blocker #3 /
// double-emit guard). The `has_pending` fork shares ONE phase-1 + ONE
// finish_args across the base/static/instance sub-paths, so a pending `.(…)` arg
// is lowered EXACTLY ONCE — never re-lowered during a non-taken sub-path's
// resolution probe. `Vec2`'s ctor bumps a shared static counter; after a single
// `obj.Take(.(3,4))` call the counter must be 1. A double-emit (the bug this
// guards) would construct twice and the counter would read 2.
struct Vec2 {
	public int32 x;
	public int32 y;
	public this(int32 x, int32 y) {
		this.x = x;
		this.y = y;
		Stats.Built = Stats.Built + 1; // bumps once per construction
	}
}
class Stats {
	public static int32 Built;
}
class Use {
	public int32 Take(Vec2 v) { return v.x + v.y; }
}
class Program {
	public static int32 Main() {
		Use u = new Use();
		int32 ignore = u.Take(.(3, 4)); // pending arg constructed exactly once
		delete u;
		return Stats.Built; // 1 (not 2 — no double-emit)
	}
}
