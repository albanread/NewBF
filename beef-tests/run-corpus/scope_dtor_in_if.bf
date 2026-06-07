// expect: 1
// MS-T4: a `scope` allocation INSIDE an `if` branch with a bare (non-block) body
// does NOT dominate the block exit, so it is tracked via a per-site entry-block
// null-guarded slot. The taken branch stores the pointer; block exit frees it
// only because the slot is non-null → exactly one dtor. The non-allocating call
// (cond false) leaves the slot null → no free, no leak, no fault (proven by the
// Stomp guard staying quiet across both calls).
class Tracked {
	public static int32 sDtors = 0;
	public int32 v = 1;
	public ~this() { Tracked.sDtors += 1; }
}
class Program {
	static int32 Maybe(bool make) {
		int32 r = 0;
		{
			if (make)
				r = (scope Tracked()).v;   // non-dominating → slot, null-guarded free
		}                                  // exit: free iff slot != null
		return r;
	}
	public static int32 Main() {
		Maybe(false);   // slot stays null → 0 dtors
		Maybe(true);    // slot set → exactly 1 dtor
		return Tracked.sDtors;   // 1
	}
}
