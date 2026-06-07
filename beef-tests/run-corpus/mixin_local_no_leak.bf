// expect: 7
// MX-T3 — scope/cleanup correctness under the splice (mixins.md §3.3 steps 5/8):
// a mixin body that `new`s + `delete`s within its own block. The lockstep
// scope/defer/scope_allocs frame is pushed around the splice and truncated back
// after, so the body's local `b` does not leak into the caller and the heap object
// is freed inside the body. The mixin yields `b.value` (7) read before delete.
class Cell {
	public int32 value = 7;
}
class Program {
	static mixin MakeAndRead() {
		Cell b = new Cell();
		int32 got = b.value;
		delete b;
		got
	}
	public static int32 Main() {
		int32 r = MakeAndRead!();
		return r;   // 7 — allocated, read, freed inside the splice; no leak/fault
	}
}
