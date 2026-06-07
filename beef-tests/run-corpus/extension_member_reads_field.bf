// expect: 42
// CB-T0: an `extension` composes into the already-declared `class Foo`,
// appending a method that reads a field declared on the base type. Proves
// extension members merge into the same type id (not a duplicate) and can
// read the base's fields (not clobbered). No comptime involved.
class Foo {
	public int32 mX;
	public this(int32 init) { this.mX = init; }
}

extension Foo {
	public int32 GetX() { return this.mX; }
}

class Program {
	public static int32 Main() {
		Foo f = new Foo(42);
		int32 r = f.GetX();
		delete f;
		return r;
	}
}
