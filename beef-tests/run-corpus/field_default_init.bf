// expect: 582
// Constant field default initializers (`int32 a = 5;`) are applied at
// construction, before the constructor body, and across inheritance: Base.a=5
// and Derived.b=7 are set before Derived's ctor runs, so the ctor reading them
// computes c = a + b = 12. r = 5*100 + 7*10 + 12 = 582.
class Base {
	public int32 a = 5;
}
class Derived : Base {
	public int32 b = 7;
	public int32 c;
	public this() { this.c = this.a + this.b; }
}
class Program {
	public static int32 Main() {
		Derived d = new Derived();
		int32 r = d.a * 100 + d.b * 10 + d.c;
		delete d;
		return r;
	}
}
