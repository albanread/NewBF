// MS-T4 verify pin: both arms of an `if` allocate a `scope` object via a bare
// (non-block) if-body, so neither alloc dominates the enclosing block exit. Each
// arm's alloc gets its OWN per-site entry-block null-guarded slot (one slot per
// scope SITE, not per binding) — whichever arm ran has its slot set, the other
// stays null, and block exit frees exactly the one that ran. Both allocs must use
// slots and the module must verify clean (R9).
class Node {
	public int32 v = 1;
	public ~this() { }
}
class ScopeInBothIfBranches {
	public static int32 Run(bool flag) {
		int32 r = 0;
		{
			if (flag)
				r = (scope Node()).v;       // arm A: site #1 slot
			else
				r = (scope Node()).v + 1;   // arm B: site #2 slot (distinct object)
		}
		return r;
	}
}
