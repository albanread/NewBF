// expect: 42
// CB-T5: emission composes with vtables. `Dog`'s `override Speak()` is NOT in
// the source — a `[Comptime, EmitGenerator]` emits it. Because the type graph
// (and every vtable) is recomputed from the full source set each round, the
// emitted override is spliced as `extension Dog { … }`, feeds into Dog's
// virtuals/vimpls on the rebuild, and joins the vtable. `a` is statically
// `Animal` but holds a `Dog`; `a.Speak()` dispatches through the vtable to the
// EMITTED override (42), not Animal's body (7) — proving emitted virtuals
// participate in dynamic dispatch exactly like hand-written ones.
class Animal {
	public virtual int32 Speak() { return 7; }
}

class Dog : Animal {
	[Comptime, EmitGenerator]
	public static void Generate() {
		Compiler.EmitTypeBody("public override int32 Speak() { return 42; }");
	}
}

class Program {
	public static int32 Main() {
		Animal a = new Dog();
		int32 r = a.Speak();   // virtual dispatch -> emitted Dog.Speak == 42
		delete a;
		return r;
	}
}
