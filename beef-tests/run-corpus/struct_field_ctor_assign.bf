// expect: 34
// Target-typed `.(args)` on an assignment RHS: a constructor body assigns
// `this.a = .(x)` where field `a` has a value-struct type, so the RHS is
// constructed against the field's type (not just local-init / return). Outer
// builds two Inner fields; o.a.v=3, o.b.v=4 → 3*10 + 4 = 34.
struct Inner {
	public int32 v;
	public this(int32 v) { this.v = v; }
}
struct Outer {
	public Inner a;
	public Inner b;
	public this(int32 x, int32 y) {
		this.a = .(x);
		this.b = .(y);
	}
}
class Program {
	public static int32 Main() {
		Outer o = .(3, 4);
		return o.a.v * 10 + o.b.v;   // 34
	}
}
