// expect: 33
// Null-conditional method call `a?.M()`: evaluates the receiver once and
// null-guards the call, yielding the method's default (0) when null. A live node
// returns its computed value; a null node's `?.Get()` yields 0.
class Box {
	public int32 n;
	public int32 Get() { return this.n; }
	public int32 Scale(int32 k) { return this.n * k; }
}
class Program {
	public static int32 Main() {
		Box b = new Box();
		b.n = 11;
		int32 got = b?.Get();        // 11
		int32 scaled = b?.Scale(2);  // 22
		Box gone = null;
		int32 zero = gone?.Get();    // 0 (null receiver → default)
		delete b;
		return got + scaled + zero;  // 11 + 22 + 0 = 33
	}
}
