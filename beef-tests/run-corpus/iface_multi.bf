// expect: 7
// A class implementing TWO interfaces (itables.md §8/4): `Both : IA, IB`. The
// same object is viewed through each interface-typed value and dispatched
// through each interface's own slot block. IA.GetA → 3, IB.GetB → 4, summing 7.
// Pins that two interfaces on one class get distinct, non-overlapping slot
// ranges in the class vtable.
interface IA {
	int32 GetA();
}
interface IB {
	int32 GetB();
}
class Both : IA, IB {
	public int32 GetA() { return 3; }
	public int32 GetB() { return 4; }
}
class Program {
	public static int32 Main() {
		Both o = new Both();
		IA a = o;
		IB b = o;
		int32 r = a.GetA() + b.GetB();   // 3 + 4 == 7
		delete o;
		return r;
	}
}
