// expect: 142
// Heterogeneous payload enum: cases carry *different* payload types at the same
// position (int32 / float / bool), so the union slot is sized to the widest
// member and each case stores/loads its own type into it. Previously such enums
// stayed int-backed; now they lay out as a tagged union.
//   I(40)    → match .I(let n)  → n         = 40
//   B(true)  → match .B(let b)  → b ? 100   = 100
//   sum = 40 + 100 + 2 (sentinel) = 142
enum Value {
	case I(int32 n);
	case F(float f);
	case B(bool b);
}
class Program {
	static int32 AsInt(Value v) {
		switch (v) {
		case .I(let n): return n;
		case .B(let b): if (b) { return 100; } return 0;
		case .F(let f): return 7;
		}
	}
	public static int32 Main() {
		Value a = Value.I(40);
		Value c = Value.B(true);
		return AsInt(a) + AsInt(c) + 2; // 40 + 100 + 2 = 142
	}
}
