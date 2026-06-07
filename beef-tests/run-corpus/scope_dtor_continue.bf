// expect: 4
// MS-T4: `continue` also runs the depth-range scope cleanup of the inner frames
// (the current iteration's blocks) before re-testing the loop. Each of the 4
// iterations allocates a `scope` object; the dtor must fire once per iteration —
// including the one that `continue`s — for exactly 4 dtors total.
class Tracked {
	public static int32 sDtors = 0;
	public ~this() { Tracked.sDtors += 1; }
}
class Program {
	public static int32 Main() {
		for (int32 i = 0; i < 4; i++) {
			Tracked t = scope Tracked();   // per-iteration scope alloc (Direct)
			if (i == 1) {
				continue;                  // depth-range cleanup frees t here too
			}
		}
		return Tracked.sDtors;             // 4 (one dtor per iteration)
	}
}
