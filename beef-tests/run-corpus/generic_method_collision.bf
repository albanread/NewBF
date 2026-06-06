// expect: 42
// Owner-qualified generic-method mangling (GM-A2): two different classes each
// declare a same-named, same-type-arg static generic method `Get<T>`. Before
// owner-mangling both collapsed to the single symbol `Get$i32` and the
// first-indexed decl won for ALL callers — a §107 collision. Now each resolves
// to its own owner-mangled monomorph (`Box.Get$i32`, `Sack.Get$i32`), so the
// two qualified calls return distinct values that sum to 42.
class Box {
	public static T Get<T>(T x) { return x; }
}
class Sack {
	public static T Get<T>(T x) { return x; }
}
class Program {
	public static int32 Main() {
		int32 a = Box.Get<int32>(40);    // 40
		int32 b = Sack.Get<int32>(2);    // 2
		return a + b;                    // 42
	}
}
