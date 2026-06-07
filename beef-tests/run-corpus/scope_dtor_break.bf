// expect: 3
// MS-T4: `break` runs the depth-range scope cleanup of the frames being exited
// before branching out of the loop. The loop body block allocates a `scope`
// object each iteration; it must be freed on every iteration — the two that fall
// through normally AND the one that `break`s — for a total of exactly 3 dtors.
class Tracked {
	public static int32 sDtors = 0;
	public ~this() { Tracked.sDtors += 1; }
}
class Program {
	public static int32 Main() {
		for (int32 i = 0; i < 10; i++) {
			Tracked t = scope Tracked();   // per-iteration scope alloc (Direct)
			if (i == 2) {
				break;                     // depth-range cleanup frees t here
			}
		}                                  // iterations 0,1 free t on fallthrough
		return Tracked.sDtors;             // 3 (i=0,1 fallthrough + i=2 break)
	}
}
