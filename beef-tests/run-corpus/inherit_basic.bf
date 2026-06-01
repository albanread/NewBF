// expect: 42
// Single inheritance with static dispatch: Dog : Animal inherits Animal's
// `legs` field and `Legs()` method, and adds its own `tail` + `Total()`. The
// derived layout prefixes the base's fields, so a base method reads inherited
// fields at the right offset.
//   d.legs 30 (inherited field) + d.tail 12 -> Total() 42; Legs() -> 30
class Animal {
	public int legs;
	public int Legs() { return this.legs; }
}
class Dog : Animal {
	public int tail;
	public int Total() { return this.legs + this.tail; }
}
class Program {
	public static int32 Main() {
		Dog d = new Dog();
		d.legs = 30;
		d.tail = 12;
		int32 r = d.Total();             // 42 — own method over inherited + own field
		if (d.Legs() != 30) { r = 0; }   // inherited method
		delete d;
		return r;
	}
}
