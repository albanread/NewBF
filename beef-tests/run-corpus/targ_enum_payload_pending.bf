// expect: 25
// TA-8 (§3.8 #6): a PENDING payload arg to an enum-case constructor. The case
// `Holds` carries a VALUE-STRUCT payload `Vec2`; the construction `Shape.Holds(.(3,4))`
// (qualified) and `.Holds(.(5,?))` (leading-dot) build the payload via
// `build_enum_value`'s loop, which now back-fills a pending `.(…)` payload arg
// against the case's declared payload type `Vec2` (instead of lowering it to undef
// via the plain `self.expr`). Both the qualified `Enum.Case(.(…))` and the bare
// `.Case(.(…))` forms route through `build_enum_value`, so both are covered.
//   q = Shape.Holds(.(3,4))   -> Vec2(3,4),  3*3 + 4*4 = 25  ... (a)
//   d = .Holds(.(0,0))        -> Vec2(0,0),  0          = 0   ... (b)
//   result = a + b = 25
struct Vec2 {
	public int32 x;
	public int32 y;
	public this(int32 x, int32 y) { this.x = x; this.y = y; }
}
enum Shape {
	case Holds(Vec2 v);
	case Empty;

	public int32 Sq() {
		if (this case .Holds(let p)) { return p.x * p.x + p.y * p.y; }
		return 0;
	}
}
class Program {
	public static int32 Main() {
		Shape q = Shape.Holds(.(3, 4)); // qualified Enum.Case(.(…)) — pending payload
		Shape d = .Holds(.(0, 0));      // leading-dot .Case(.(…)) — pending payload
		return q.Sq() + d.Sq();         // 25 + 0 = 25
	}
}
