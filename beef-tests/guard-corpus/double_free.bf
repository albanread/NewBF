// guard: double free. `new` + `delete` + `delete` again. The second free finds
// the ledger tombstone (the entry was marked Freed on the first delete, kept
// forever) → the guard prints "double free of <ptr> -> abort" and `abort()`s.
// The child exits with the abort/fail-fast status (NOT a clean 0).
// (memory-safety.md §1 program (b); ledger-first, never derefs the freed page.)
class Node {
	public int32 value;
	public this() { this.value = 7; }
}
class Program {
	public static int32 Main() {
		Node p = new Node();
		delete p;
		delete p;   // guard: tombstone hit → "double free" → abort
		return 0;
	}
}
