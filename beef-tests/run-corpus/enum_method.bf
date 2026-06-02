// expect: 47
// Methods on a payload enum. `IntOpt` is a tagged union (`{$disc, $p0:i32}`) that
// now *also* carries an instance method — the method-bearing enum is reclassified
// as a struct (no base ⇒ layoutable) and its method lowers with `this` as a
// pointer to the body. The body uses the `if x case .Some(let v)` case-test to
// destructure `this`. This is the idiomatic shape of corlib `Option`'s
// `GetValueOrDefault`.
//   a = Some(42) → GetOr(0)  → `this case .Some(let v)` true → 42
//   b = None     → GetOr(5)  → case-test false             → 5
enum IntOpt {
	case Some(int32 value);
	case None;

	public int32 GetOr(int32 dflt) {
		if (this case .Some(let v)) {
			return v;
		}
		return dflt;
	}
}
class Program {
	public static int32 Main() {
		IntOpt a = IntOpt.Some(42);
		IntOpt b = IntOpt.None;
		return a.GetOr(0) + b.GetOr(5); // 42 + 5 = 47
	}
}
