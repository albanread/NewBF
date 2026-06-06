// expect: 100
// Default interface method (itables.md §5/§8 T6): an interface method with a
// body. A class implementing the interface but NOT overriding `D` inherits the
// default through its itable slot. `apply_itables` (IT-T3) writes the default's
// symbol (`I.D`) into the slot; IT-T6 emits `I.D` as a real free function with
// `this : Ref(I)`. Calling `D()` through an `I`-typed value dispatches to the
// default body.
interface I {
	int32 D() { return 100; }
}
class C : I {
	public int32 V() { return 1; }   // an unrelated method so C isn't empty
}
class Program {
	public static int32 Main() {
		C c = new C();
		I i = c;            // free upcast
		int32 r = i.D();    // dispatches to the default body → 100
		delete c;
		return r;
	}
}
