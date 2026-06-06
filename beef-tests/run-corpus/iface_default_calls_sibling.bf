// expect: 30
// The hardest part of T6 (itables.md §5 T6, §7): a DEFAULT interface method body
// that calls a SIBLING ABSTRACT interface method through `this`. Inside `D`'s
// body `this` is an interface value (`Ref(I)`), so the bare `A()` must become an
// INTERFACE dispatch on `this` through the interface vtable — NOT a direct call
// (A is abstract, it has no direct symbol). The concrete class provides `A`
// returning 10; `D()` returns `A() * 3 == 30`.
interface I {
	int32 A();
	int32 D() { return A() * 3; }
}
class C : I {
	public int32 A() { return 10; }
}
class Program {
	public static int32 Main() {
		C c = new C();
		I i = c;
		int32 r = i.D();    // default body calls this.A() (=10) * 3 → 30
		delete c;
		return r;
	}
}
