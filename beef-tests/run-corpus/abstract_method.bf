// expect: 12
// `abstract` method: Animal.Speak() has no body — it only reserves a vtable
// slot. Each subclass `override`s it. A call through an Animal reference reaches
// the concrete override; the abstract base slot (null) is never called because
// Animal itself is never instantiated.
abstract class Animal {
	public abstract int32 Speak();
}
class Dog : Animal {
	public override int32 Speak() { return 4; }
}
class Cat : Animal {
	public override int32 Speak() { return 8; }
}
class Program {
	public static int32 Main() {
		Animal a = new Dog();
		Animal b = new Cat();
		int32 r = a.Speak() + b.Speak();   // 4 + 8 = 12, each via its override
		delete a;
		delete b;
		return r;
	}
}
