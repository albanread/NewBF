// expect: 42
// Instance generic-method dispatch (GM-A3b): a concrete class declares an
// *instance* generic method `Id<T>(T x) = x`. The call `b.Id<int32>(...)` on a
// declared-typed local receiver resolves the receiver's owner (`Box`) the same
// way the collector did (R4), mangles `Box.Id$i32` as an instance method, and
// prepends `b` as the `this` receiver. Two calls on the same local sum to 42.
class Box {
	public T Id<T>(T x) { return x; }
}
class Program {
	public static int32 Main() {
		Box b = new Box();
		int32 a = b.Id<int32>(40);   // 40
		int32 c = b.Id<int32>(2);    // 2
		return a + c;                // 42
	}
}
