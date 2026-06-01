// expect: 99
// Body-having property setter: `c.Value = 99` lowers to set_Value(c, 99) which
// stores the backing field; reading c.Value calls get_Value. (Auto-properties
// are a later slice — this setter has an explicit body.)
class Counter {
	public int32 raw;
	public int32 Value { get { return this.raw; } set { this.raw = value; } }
}
class Program {
	public static int32 Main() {
		Counter c = new Counter();
		c.Value = 99;          // set_Value(c, 99) -> this.raw = 99
		int32 r = c.Value;     // get_Value(c) -> 99
		delete c;
		return r;
	}
}
