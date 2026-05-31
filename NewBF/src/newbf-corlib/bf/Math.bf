// NewBF corlib — System.Math (a slice). Static integer helpers; the real
// floating-point + generic surface arrives with the full corlib port.
// (Top-level for now — namespaces are folded in later; see docs/STDLIB.md.)
class Math {
	public static int32 Abs(int32 x) { if (x < 0) { return -x; } return x; }
	public static int32 Max(int32 a, int32 b) { if (a > b) { return a; } return b; }
	public static int32 Min(int32 a, int32 b) { if (a < b) { return a; } return b; }
}
