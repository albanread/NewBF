// expect: 42
// A target-typed bare `.Case(payload)` / `.None` as a CALL ARGUMENT against a
// payload-enum param. `IntOpt` is a method-bearing payload enum (reclassified as
// a struct), so the param type is `Struct(IntOpt)`; the pending `.Some(40)` /
// `.None` args are shape-gated by the enum's case set and back-filled against it.
//   Unwrap(.Some(40), .None) = 40 + 2 = 42
enum IntOpt {
	case Some(int32 value);
	case None;

	public int32 GetOr(int32 dflt) {
		if (this case .Some(let v)) { return v; }
		return dflt;
	}
}
class Use {
	public int32 Unwrap(IntOpt a, IntOpt b) {
		return a.GetOr(0) + b.GetOr(2);
	}
}
class Program {
	public static int32 Main() {
		Use u = new Use();
		int32 r = u.Unwrap(.Some(40), .None); // both args target-type to IntOpt
		delete u;
		return r; // 40 + 2 = 42
	}
}
