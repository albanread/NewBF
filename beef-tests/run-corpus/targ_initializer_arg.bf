// expect: 30
// A target-typed `.{ field = v }` object initializer as a CALL ARGUMENT against
// a VALUE-STRUCT param. The pending `.{ a = 10, b = 20 }` is shape-gated to the
// `Pt` struct (TA-1 declines a class `.{ }`, so this stays a value struct) and
// back-filled against the resolved param type, constructing the struct in place.
//   Sum(.{ a = 10, b = 20 }) = 10 + 20 = 30
struct Pt {
	public int32 a;
	public int32 b;
}
class Use {
	public int32 Sum(Pt p) { return p.a + p.b; }
}
class Program {
	public static int32 Main() {
		Use u = new Use();
		int32 r = u.Sum(.{ a = 10, b = 20 }); // `.{ … }` target-types to Pt
		delete u;
		return r; // 30
	}
}
