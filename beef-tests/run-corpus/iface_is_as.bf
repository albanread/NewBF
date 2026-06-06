// expect: 1
// `is`/`as` against an INTERFACE, and `is` whose SOURCE is itself
// interface-typed (itables.md §5/§9 T7). The runtime test reads the object's
// `$header` (offset 0) via a raw GEP, so it works for a class source AND for an
// interface-typed source (an interface id has an empty StructDef).
//   - `sq is IShape`      true  (Square implements IShape)
//   - `pl is IShape`      false (Plain implements nothing)
//   - `sq as IShape`      non-null, then dispatches Area() → 9
//   - `IShape s = sq; s is Square`  true  (interface-typed SOURCE downcast-test)
//   - `IShape s = sq; s is IShape`  true  (interface-typed SOURCE, iface target)
// The arithmetic folds every test to a single +1/-1 so the total is exactly 1:
//   +1 (sq is IShape) -1 (NOT pl is IShape, i.e. it's false so we add 1 for the
//   correct false) ... engineered as a count of correct outcomes minus a
//   constant, landing on 1. See the inline running total.
interface IShape {
	int32 Area();
}
class Square : IShape {
	public int32 Area() { return 9; }
}
class Plain {
	public int32 V() { return 0; }
}
class Program {
	public static int32 Main() {
		Square sq = new Square();
		Plain pl = new Plain();
		int32 r = 0;

		// `is IShape` against a class source: true for the implementer.
		if (sq is IShape) { r += 1; }        // true  → 1

		// `is IShape` against a non-implementer class source: false.
		if (pl is IShape) { r += 100; }      // false → 1 (no change)

		// `as IShape` is non-null then dispatches to the concrete Area().
		IShape asv = sq as IShape;
		if (asv != null) { r += asv.Area(); } // + 9 → 10

		// SOURCE is interface-typed: downcast-test back to the concrete class.
		IShape s = sq;
		if (s is Square) { r += 1; }         // true  → 11

		// SOURCE is interface-typed: test against the interface itself.
		if (s is IShape) { r += 5; }         // true  → 16

		// Fold to exactly 1: subtract the accumulated true-path total back out,
		// leaving only the single sentinel for "all five tests behaved".
		// Expected r here is 16 (1 + 9 + 1 + 5); 16 - 15 == 1.
		r = r - 15;                          // → 1

		delete sq;
		delete pl;
		return r;
	}
}
