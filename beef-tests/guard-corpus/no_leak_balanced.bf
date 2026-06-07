// guard: balanced new/delete. `new` + `delete` (matched). The ledger marks the
// entry Freed, so after Main returns there are zero *live* (un-tombstoned)
// entries: `live_count() == 0`. The `leakcheck` runner exits 0 iff the ledger
// is balanced. Proves the no-false-leak side of the guard.
class Node {
	public int32 value;
	public this() { this.value = 7; }
}
class Program {
	public static int32 Main() {
		Node p = new Node();
		int32 r = p.value;
		delete p;
		return r;
	}
}
