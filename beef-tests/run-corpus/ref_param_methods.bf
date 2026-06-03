// expect: 100
// `ref` across the two other call shapes: a qualified static call
// (`Mut.Negate(ref ...)`) and an instance-method call (`c.Bump(ref ...)`).
class Mut {
	public static void Double(ref int32 v) { v = v + v; }
}
class Counter {
	int32 step;
	public this(int32 s) { this.step = s; }
	public void Bump(ref int32 v) { v = v + this.step; }
}
class Program {
	public static int32 Main() {
		int32 a = 20;
		Mut.Double(ref a);          // a = 40
		Counter c = new Counter(10);
		c.Bump(ref a);              // a = 50
		c.Bump(ref a);              // a = 60
		Mut.Double(ref a);          // a = 120
		delete c;
		return a - 20;              // 120 - 20 = 100
	}
}
