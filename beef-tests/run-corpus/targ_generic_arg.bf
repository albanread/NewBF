// expect: 7
// TA-7: a target-typed `.(args)` value-struct-ctor shorthand passed to a GENERIC
// method whose param type is the explicit type-arg. `Identity<Vec2>` resolves its
// monomorph by the EXPLICIT type-arg `<Vec2>` (the mangled key), so the signature
// `Identity(Vec2) -> Vec2` is known here with NO overload picking on value args.
// The `has_pending` fork back-fills the `.(3,4)` against the pre-built
// `gen_method_sigs` param type `Vec2` (un-offset past the static call's absent
// `this`) and constructs it in place.
//   v = Identity<Vec2>(.(3,4)) = Vec2(3,4);  v.x + v.y = 7
struct Vec2 {
	public int32 x;
	public int32 y;
	public this(int32 x, int32 y) { this.x = x; this.y = y; }
}
class Program {
	public static T Identity<T>(T x) { return x; }
	public static int32 Main() {
		Vec2 v = Identity<Vec2>(.(3, 4)); // `.(3,4)` target-types to the `T = Vec2` param
		return v.x + v.y; // 3 + 4 = 7
	}
}
