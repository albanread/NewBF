// expect: 42
// Virtual dispatch: `a` is statically Animal but holds a Dog; a.Speak() runs
// Dog's override (42), not Animal's body (7) — dispatched through the vtable
// stored in the object's $header by `new Dog()`.
class Animal {
	public virtual int Speak() { return 7; }
}
class Dog : Animal {
	public override int Speak() { return 42; }
}
class Program {
	public static int32 Main() {
		Animal a = new Dog();
		int32 r = a.Speak();   // virtual -> Dog.Speak == 42
		delete a;
		return r;
	}
}
