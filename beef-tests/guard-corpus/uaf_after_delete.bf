// guard: use-after-free. `new` an object, `delete` it, then read a field.
// Under the Stomp guard the page is decommitted + permanently quarantined, so
// the load faults: the child process exits with ACCESS_VIOLATION (0xC0000005).
// (memory-safety.md §1 program (a); the page fault is the fast hardware net.)
class Node {
	public int32 value;
	public this() { this.value = 7; }
}
class Program {
	public static int32 Main() {
		Node p = new Node();
		delete p;
		return p.value;   // ACCESS_VIOLATION: page decommitted, never reused
	}
}
