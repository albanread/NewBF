// NewBF corlib — System.Math (a slice). Static integer helpers; the real
// floating-point + generic surface arrives with the full corlib port.
// (Top-level for now — namespaces are folded in later; see docs/STDLIB.md.)
class Math {
	public static int32 Abs(int32 x) { if (x < 0) { return -x; } return x; }
	public static int32 Max(int32 a, int32 b) { if (a > b) { return a; } return b; }
	public static int32 Min(int32 a, int32 b) { if (a < b) { return a; } return b; }
	public static int32 Clamp(int32 v, int32 lo, int32 hi) { return Min(Max(v, lo), hi); }
	public static int32 Sign(int32 x) { return x < 0 ? -1 : (x > 0 ? 1 : 0); }
	public static float Max(float a, float b) { return a > b ? a : b; }
	public static float Min(float a, float b) { return a < b ? a : b; }
	public static float Clamp(float v, float lo, float hi) { return Min(Max(v, lo), hi); }
	public static int32 Sign(float x) { return x < 0 ? -1 : (x > 0 ? 1 : 0); }
}
