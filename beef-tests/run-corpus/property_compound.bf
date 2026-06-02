// expect: 15
// Compound assignment on an auto-property: Value starts 10 (set in the ctor),
// then `+= 5` reads via get_Value, adds, writes via set_Value. (Previously a
// silent no-op — the setter interception only handled plain `=`.)
class Counter {
	public int32 Value { get; set; }
	public this() { this.Value = 10; }
}
class Program {
	public static int32 Main() {
		Counter c = new Counter();
		c.Value += 5;          // get_Value (10) + 5 -> set_Value(15)
		int32 r = c.Value;     // 15
		delete c;
		return r;
	}
}
