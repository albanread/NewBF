// MS-T4 verify pin: a `scope` allocation inside a single `if` branch that does
// NOT have its own block frame (a bare-statement if-body) — so the alloc does
// not dominate the enclosing block's exit. Freeing it unconditionally at exit
// would be an "instruction does not dominate all uses" verifier failure (R9).
// The per-site entry-block null-guarded slot makes only the slot pointer and the
// loaded ptr-or-null cross block edges, so the module must verify clean.
class Node {
	public int32 v = 1;
	public ~this() { }
}
class ScopeInIfBranch {
	public static int32 Run(bool flag) {
		int32 r = 0;
		{
			if (flag)
				r = (scope Node()).v;   // non-dominating scope alloc → slot
		}
		return r;
	}
}
