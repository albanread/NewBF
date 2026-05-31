// expect: 42
// Instance method call `obj.Get()` returning a field read via `this.`.
class Counter {
	public int32 n;
	public this(int32 start) { this.n = start; }
	public int32 Get() { return this.n; }
}
class Program {
	public static int32 Main() {
		Counter c = new Counter(42);
		int32 r = c.Get();
		delete c;
		return r;
	}
}
