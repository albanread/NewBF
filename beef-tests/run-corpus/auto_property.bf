// expect: 7
// Auto-property `{ get; set; }`: the compiler synthesizes a backing field and
// trivial get/set bodies. Count starts default (0), is set to 7, read back.
class Box {
	public int32 Count { get; set; }
}
class Program {
	public static int32 Main() {
		Box b = new Box();
		b.Count = 7;          // synthesized set_Count -> backing = 7
		int32 r = b.Count;    // synthesized get_Count -> backing
		delete b;
		return r;
	}
}
