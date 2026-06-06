// expect: 9
// Canonical interface dispatch (itables.md §1/§8): a method call through an
// interface-typed value reaches the concrete implementation. `Square` has NO
// `virtual` method of its own, so this pins the interface-only-class
// vtable/header-emission path (the only vtable slot Square has is the appended
// IShape.Area slot). `IShape s = sq` is a free upcast (pointer identity); the
// call dispatches through s's $header vtable at IShape's slot.
interface IShape {
	int32 Area();
}
class Square : IShape {
	public int32 Area() { return 9; }
}
class Program {
	public static int32 Main() {
		Square sq = new Square();
		IShape s = sq;          // upcast: a no-op reinterpret
		int32 r = s.Area();     // dynamic interface dispatch → 9
		delete sq;
		return r;
	}
}
