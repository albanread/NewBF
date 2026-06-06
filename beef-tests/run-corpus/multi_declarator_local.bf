// expect: 12345
// Multi-declarator local `int a = 1, b = 2, …;` — each declarator shares the
// leading type and is bound in the enclosing scope (a scope-transparent group),
// so all five are usable afterward. r = 1*10000 + 2*1000 + 3*100 + 4*10 + 5.
class Program {
	public static int32 Main() {
		int32 a = 1, b = 2, c = 3, d = 4, e = 5;
		return a * 10000 + b * 1000 + c * 100 + d * 10 + e;
	}
}
