// expect: 1
// Polymorphism through ONE call site (itables.md §8/3): a single `IShape`-typed
// local `s` is rebound to two different concrete objects; the *same* textual
// `s.Area()` dispatches to a different impl each time. 4 - 3 == 1.
interface IShape {
	int32 Area();
}
class Big : IShape {
	public int32 Area() { return 4; }
}
class Small : IShape {
	public int32 Area() { return 3; }
}
class Program {
	public static int32 Main() {
		Big big = new Big();
		Small small = new Small();
		IShape s = big;
		int32 r = s.Area();      // dispatches to Big.Area → 4
		s = small;
		r = r - s.Area();        // SAME call site, now Small.Area → 4 - 3 == 1
		delete big;
		delete small;
		return r;
	}
}
