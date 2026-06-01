// expect: 49
// Polymorphism: two objects behind the *same* static type (Animal) dispatch to
// different implementations by runtime type — x is an Animal (7), y is a Dog
// (42), each Speak() routed through its own object's vtable.
class Animal {
	public virtual int Speak() { return 7; }
}
class Dog : Animal {
	public override int Speak() { return 42; }
}
class Program {
	public static int32 Main() {
		Animal x = new Animal();
		Animal y = new Dog();
		int32 r = x.Speak() + y.Speak();   // 7 + 42 == 49
		delete x;
		delete y;
		return r;
	}
}
