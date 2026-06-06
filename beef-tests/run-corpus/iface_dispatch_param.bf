// expect: 42
// Interface-typed PARAMETER dispatch (itables.md §8/2): `F(IShape s)` calls
// `s.Area()` through the interface vtable, so the same call site routes to
// whichever concrete type was passed. Summed over two implementers (9 + 33 = 42)
// to prove the slot resolves per object, not statically.
interface IShape {
	int32 Area();
}
class Square : IShape {
	public int32 Area() { return 9; }
}
class Rect : IShape {
	public int32 Area() { return 33; }
}
class Program {
	public static int32 F(IShape s) { return s.Area(); }
	public static int32 Main() {
		Square sq = new Square();
		Rect rc = new Rect();
		int32 r = F(sq) + F(rc);   // 9 + 33 == 42
		delete sq;
		delete rc;
		return r;
	}
}
