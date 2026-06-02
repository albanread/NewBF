// expect: 21
// A computed (get-only) property on a payload enum. §52 made method-bearing
// enums layoutable; this extends that to *computed* properties (auto-properties
// stay int-backed — they'd need a backing field the fixed union layout can't
// hold). The getter destructures `this` with the case-test operator.
//   a = Some(7) → a.IsSome true,  a.IsNone false → r += 1, then +20
//   b = None    → b.IsSome false, b.IsNone true  → (skip),  +0  ... see encoding
enum IntOpt {
	case Some(int32 value);
	case None;

	public bool IsSome {
		get { return this case .Some(let v); }
	}
	public bool IsNone {
		get { return this not case .Some(let v); }
	}
}
class Program {
	public static int32 Main() {
		IntOpt a = IntOpt.Some(7);
		IntOpt b = IntOpt.None;
		int32 r = 0;
		if (a.IsSome) { r = r + 1; }    // 1
		if (b.IsNone) { r = r + 20; }   // 21
		if (a.IsNone) { r = r + 100; }  // skip
		if (b.IsSome) { r = r + 100; }  // skip
		return r; // 1 + 20 = 21
	}
}
