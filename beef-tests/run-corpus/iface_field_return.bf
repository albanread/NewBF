// expect: 12
// Interface value stored in a class FIELD and RETURNED from a method
// (itables.md §8/5). `Holder.shape` is an `IShape` field; `Holder.Get()` returns
// `IShape`. We dispatch through the field-load path (5) and through the
// method-return path (7), summing 12. Pins that an interface value flows
// through field store/load and a return slot as a plain pointer, and dispatch
// works on both.
interface IShape {
	int32 Area();
}
class Five : IShape {
	public int32 Area() { return 5; }
}
class Seven : IShape {
	public int32 Area() { return 7; }
}
class Holder {
	public IShape shape;
	public IShape Get() { return this.shape; }
}
class Program {
	public static int32 Main() {
		Five five = new Five();
		Seven seven = new Seven();
		Holder ha = new Holder();
		Holder hb = new Holder();
		ha.shape = five;        // store interface value in a field
		hb.shape = seven;
		int32 r = ha.shape.Area();   // field-load dispatch → 5
		IShape got = hb.Get();       // interface returned from a method
		r = r + got.Area();          // return-path dispatch → 5 + 7 == 12
		delete five;
		delete seven;
		delete ha;
		delete hb;
		return r;
	}
}
