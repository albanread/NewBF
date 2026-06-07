// MS-T4 verify pin: a `scope` alloc inside an `if` (bare body) inside a `while`,
// with a `break`. The non-dominating alloc uses an entry-block null-guarded slot;
// the `break` runs the depth-range frame cleanup before branching to the loop
// exit; the loop-fallthrough and break edges each free via the null-guard. No
// `new` value crosses a block edge, so the module must verify clean (R9).
class Node {
	public int32 v = 1;
	public ~this() { }
}
class ScopeInIfInWhileBreak {
	public static int32 Run(int32 n) {
		int32 sum = 0;
		int32 i = 0;
		while (i < n) {
			{
				if (i == 2) {
					Node n2 = scope Node();   // dominating in THIS block (Direct)
					sum += n2.v;
					break;                    // depth-range cleanup + br exit
				}
				if (i == 1)
					sum += (scope Node()).v;  // non-dominating bare-body → Slot
			}
			sum += i;
			i += 1;
		}
		return sum;
	}
}
