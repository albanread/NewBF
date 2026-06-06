// expect: 5
// An interface method satisfied PURELY by an INHERITED base-class method
// (itables.md §8/8). `C : Base, IFace` declares no `M` of its own; `IFace.M` is
// resolved by `apply_itables` to the inherited `Base.M` (5). Pins the
// post-`apply_inheritance` resolution: `methods[C]` already includes the
// inherited `M`, so the impl symbol wired into the interface slot is `Base.M`.
interface IFace {
	int32 M();
}
class Base {
	public int32 M() { return 5; }
}
class C : Base, IFace {
}
class Program {
	public static int32 Main() {
		C c = new C();
		IFace f = c;
		int32 r = f.M();   // dispatches to inherited Base.M → 5
		delete c;
		return r;
	}
}
