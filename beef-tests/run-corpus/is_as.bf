// expect: 31
// `is`/`as` runtime type tests. Each class with virtual methods carries a vtable
// in its object header; `obj is T` checks the header against T's vtable and every
// vtable-bearing subclass of T (all known at compile time), and `obj as T` yields
// the reference when that matches, else null. A Dog viewed as an Animal is an
// Animal and a Dog, not a Bird.
class Animal { public virtual int32 Legs() { return 4; } }
class Dog : Animal { public override int32 Legs() { return 4; } }
class Bird : Animal { public override int32 Legs() { return 2; } }
class Program {
	public static int32 Main() {
		Animal a = new Dog();
		int32 r = 0;
		if (a is Animal) { r += 1; }    // true  → 1
		if (a is Dog) { r += 2; }       // true  → 3
		if (a is Bird) { r += 100; }    // false → 3
		Dog d = a as Dog;               // non-null
		if (d != null) { r += d.Legs(); }  // + 4 → 7
		Bird b = a as Bird;             // null
		if (b == null) { r += 8; }      // → 15
		r += 16;                        // → 31
		delete a;
		return r;
	}
}
