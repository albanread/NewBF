// expect: 12
// A generic method monomorphized per explicit type-arg: Identity<T>(T x) = x.
class Program {
	public static T Identity<T>(T x) { return x; }
	public static int32 Main() {
		int32 a = Identity<int32>(5);
		int32 b = Identity<int32>(7);
		return a + b;   // 5 + 7 = 12
	}
}
