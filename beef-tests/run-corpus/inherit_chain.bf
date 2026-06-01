// expect: 42
// Multi-level inheritance (C : B : A) composes base-first, and a derived
// reference up-casts to a base (prefix-compatible pointer): A.GetA() reads the
// `a` field through 2 levels of inheritance on a C instance.
//   a 20 + b 12 + c 10 = 42 (Sum); GetA() through the upcast -> 20
class A {
	public int a;
	public int GetA() { return this.a; }
}
class B : A {
	public int b;
}
class C : B {
	public int c;
	public int Sum() { return this.a + this.b + this.c; }
}
class Program {
	public static int32 Main() {
		C obj = new C();
		obj.a = 20;   // inherited from A (through B)
		obj.b = 12;   // inherited from B
		obj.c = 10;   // own
		int32 r = obj.Sum();           // 42
		A up = obj;                    // upcast C -> A
		if (up.GetA() != 20) { r = 0; }
		delete obj;
		return r;
	}
}
