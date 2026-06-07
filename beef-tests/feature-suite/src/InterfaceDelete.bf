// MS-T4 verify pin: `delete` of an INTERFACE-typed local. An interface ref's
// concrete class isn't statically known, so the dtor chain can't be walked;
// `lower_delete` takes the bare `newbf_free` branch (no dtor) and an interface
// id never reaches `emit_destroy` (which asserts a concrete class). This must
// lower to a verifiable module — the delete is a plain free, no dtor-chain walk.
interface IShape {
	int32 Area();
}
class Square : IShape {
	public int32 side = 3;
	public int32 Area() { return this.side * this.side; }
}
class InterfaceDelete {
	public static int32 Run() {
		IShape s = new Square();   // interface-typed local
		int32 a = s.Area();
		delete s;                  // bare newbf_free branch (interface delete)
		return a;
	}
}
