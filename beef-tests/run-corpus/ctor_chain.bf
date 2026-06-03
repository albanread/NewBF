// expect: 124
// Implicit base-constructor chaining: building a derived object runs the base
// constructors first (root-most first), so inherited fields are initialized
// before the derived ctor body. Three-level chain A : B : Base; each ctor sets
// its own field, and a derived ctor reads a base field the base ctor just set.
class Base { public int32 a; public this() { this.a = 100; } }
class Mid : Base { public int32 b; public this() { this.b = this.a / 5; } }  // 20
class Leaf : Mid { public int32 c; public this() { this.c = this.b / 5; } }  // 4
class Program {
	public static int32 Main() {
		Leaf t = new Leaf();
		int32 r = t.a + t.b + t.c;   // 100 + 20 + 4 = 124
		delete t;
		return r;
	}
}
