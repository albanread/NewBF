// expect: 7
// GC-T0 — class-bound generic constraint, pinned as a non-regression. The `where
// T : Animal` clause is NOT enforced today; dispatch works because monomorphizing
// T = Dog makes `val.Speak()` resolve statically to Dog's concrete method table
// (the inherited/derived class layout). This pins that the satisfied class-bound
// call keeps lowering correctly (the "constraint static path"). A later GC task
// adds the violation diagnostic; this program is its green baseline.
class Animal {
	public virtual int32 Speak() { return 0; }
}
class Dog : Animal {
	public override int32 Speak() { return 7; }
}
class Program {
	public static int32 Use<T>(T val) where T : Animal { return val.Speak(); }
	public static int32 Main() {
		Dog d = new Dog();
		int32 r = Use<Dog>(d);   // T = Dog: val.Speak() -> Dog.Speak -> 7
		delete d;
		return r;
	}
}
