// expect: 7
// Default-method OVERRIDE precedence (itables.md §5/§6 T6, resolve_itable_impl
// step 2 before step 3): the interface provides a default `D` returning 100, but
// the class OVERRIDES it to return 7. `apply_itables` resolves the slot to the
// class's own symbol (the same-name pick_overload) BEFORE falling back to the
// interface default, so dispatch picks the override.
interface I {
	int32 D() { return 100; }
}
class C : I {
	public int32 D() { return 7; }   // overrides the default
}
class Program {
	public static int32 Main() {
		C c = new C();
		I i = c;
		int32 r = i.D();    // class override wins → 7
		delete c;
		return r;
	}
}
