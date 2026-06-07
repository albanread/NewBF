// expect: 7
// A generic instance method that runs in a `this` context (GM-A3b): `Wrap<T>`
// just returns its arg, but a sibling non-generic instance method calls it
// *bare* (`Wrap<int32>(this.mV)`) — exercising the bare-same-class instance
// path (owner = cur_type, prepend the current `this`) and a `this`-field read
// inside the dispatch. mV starts at 7, Run() returns Wrap(this.mV) = 7.
class Cell {
	public int32 mV;
	public this() { this.mV = 7; }
	public T Wrap<T>(T x) { return x; }
	public int32 Run() { return Wrap<int32>(this.mV); }
}
class Program {
	public static int32 Main() {
		Cell c = new Cell();
		int32 r = c.Run();   // 7
		delete c;            // MS-T5.5: balance the `new Cell()` (behavior-neutral)
		return r;
	}
}
