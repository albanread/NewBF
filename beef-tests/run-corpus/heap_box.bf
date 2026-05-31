// expect: 42
// Heap object: new a class, write+read a field through the reference, delete.
// (Read into a local *before* delete to avoid use-after-free.)
class Box { public int32 v; }
class Program {
	public static int32 Main() {
		Box b = new Box();
		b.v = 42;
		int32 r = b.v;
		delete b;
		return r;
	}
}
