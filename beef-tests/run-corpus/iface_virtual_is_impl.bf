// expect: 4
// One method is BOTH an `override` (class vtable slot) AND the interface impl
// (interface slot) — the SAME symbol sits in two slots (itables.md §8/7).
// `Derived.M` overrides `Base.M` (class slot) and satisfies `IShape.M`
// (interface slot). Calling via the class type and via the interface both reach
// `Derived.M` → 2 + 2 == 4. Pins that one impl symbol can be wired into a class
// vslot and an interface slot simultaneously.
interface IShape {
	int32 M();
}
class Base {
	public virtual int32 M() { return 0; }
}
class Derived : Base, IShape {
	public override int32 M() { return 2; }
}
class Program {
	public static int32 Main() {
		Derived d = new Derived();
		Base b = d;             // class-type view (virtual dispatch)
		IShape s = d;           // interface view (itable dispatch)
		int32 r = b.M() + s.M();   // 2 (vslot) + 2 (iface slot) == 4
		delete d;
		return r;
	}
}
