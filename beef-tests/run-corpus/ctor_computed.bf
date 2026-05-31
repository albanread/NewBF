// expect: 42
// The constructor does real work: computes a field from two arguments.
class Adder {
	public int32 total;
	public this(int32 a, int32 b) { this.total = a + b; }
}
class Program {
	public static int32 Main() {
		Adder a = new Adder(40, 2);
		int32 r = a.total;
		delete a;
		return r;
	}
}
