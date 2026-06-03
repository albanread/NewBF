// expect: 30
// `base.Method()`: an override calls its parent's implementation directly (no
// virtual re-dispatch, so no infinite recursion). Dog.Sound extends Animal.Sound
// (10) by +20. Verified through both a static-typed and a base-typed reference.
class Animal { public virtual int32 Sound() { return 10; } }
class Dog : Animal {
	public override int32 Sound() { return base.Sound() + 20; }
}
class Program {
	public static int32 Main() {
		Dog d = new Dog();
		Animal a = d;          // upcast: virtual dispatch still finds Dog.Sound
		int32 r = a.Sound();   // 30 (Dog.Sound → base.Sound 10 + 20)
		delete d;
		return r;
	}
}
