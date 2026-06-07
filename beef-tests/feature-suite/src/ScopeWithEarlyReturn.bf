// MS-T4 verify pin: a dominating `scope` alloc with an EARLY return out of the
// frame. The early-return path frees every open scope frame; the normal
// fallthrough path frees the same frame on its own edge — each free is emitted on
// exactly one edge, so the value never gets double-referenced across a block and
// the module must verify clean (R9). Mixes a dominating (Direct) alloc with a
// non-dominating (Slot, bare-if-body) one inside the `if`.
class Node {
	public int32 v = 1;
	public ~this() { }
}
class ScopeWithEarlyReturn {
	public static int32 Run(bool flag) {
		Node head = scope Node();          // dominating → Direct
		if (flag)
			return (scope Node()).v + head.v;   // early return: free head (+ slot)
		return head.v;                     // fallthrough frees head
	}
}
