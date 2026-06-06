// expect: 5
// Interface INHERITANCE (itables.md §5/§9 T7): `interface IB : IA`. A class `C`
// implements `IB`; `IA.Base()` is reached through an `IB`-typed value (the slot
// comes from the base-interface block of `imethods[IB]`, which lists IA's
// methods first). Separately, `is IA` is true for an `IB` implementer via the
// transitively-flattened `iface_bases` (C drags in IA through IB : IA).
//   - call IA.Base() through IB value → 2
//   - call IB.Derived() through IB value → 1
//   - `c is IA` (transitive) true → +1
//   - `c is IB` true → +1
//   2 + 1 + 1 + 1 == 5
interface IA {
	int32 Base();
}
interface IB : IA {
	int32 Derived();
}
class C : IB {
	public int32 Base() { return 2; }
	public int32 Derived() { return 1; }
}
class Program {
	public static int32 Main() {
		C c = new C();
		IB b = c;
		int32 r = 0;
		r += b.Base();          // IA method via base-interface slot block → 2
		r += b.Derived();       // IB's own method → 3
		if (c is IA) { r += 1; } // transitive: C : IB : IA → 4
		if (c is IB) { r += 1; } // direct → 5
		delete c;
		return r;
	}
}
