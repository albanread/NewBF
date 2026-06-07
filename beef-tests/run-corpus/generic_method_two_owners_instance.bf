// expect: 6
// Owner disambiguation for the *instance* path (GM-A3b + the §107 fix): two
// classes each declare a same-named instance generic method `Id<T>`. Called on
// distinct receivers, each mangles under its own owner (`Pos.Id$i32` vs
// `Neg.Id$i32`) and dispatches with the right `this`, so the two results stay
// distinct. Pos.Id(4) + Neg.Id(2) = 6.
class Pos {
	public T Id<T>(T x) { return x; }
}
class Neg {
	public T Id<T>(T x) { return x; }
}
class Program {
	public static int32 Main() {
		Pos p = new Pos();
		Neg n = new Neg();
		int32 a = p.Id<int32>(4);   // 4
		int32 b = n.Id<int32>(2);   // 2
		delete p;                   // MS-T5.5: balance the `new Pos()` (behavior-neutral)
		delete n;                   // MS-T5.5: balance the `new Neg()` (behavior-neutral)
		return a + b;               // 6
	}
}
