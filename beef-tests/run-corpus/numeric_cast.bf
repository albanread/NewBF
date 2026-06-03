// expect: 1017
// Explicit numeric casts `(T)expr` — previously lowered to `undef`. They now go
// through the same `coerce` machinery as implicit conversions:
//   (int32)3.9f   → 3     (float→int truncates toward zero, fptosi)
//   (int32)1000L  → 1000  (i64→i32 trunc)
//   (float)7      → 7.0   (sitofp), then (int32)(7.0*2) → 14 (fptosi)
//   3 + 1000 + 14 = 1017
class Program {
	public static int32 Main() {
		float f = 3.9f;
		int32 a = (int32)f;

		int64 big = 1000;
		int32 b = (int32)big;

		int32 i = 7;
		float g = (float)i;
		int32 c = (int32)(g * 2.0f);

		return a + b + c; // 3 + 1000 + 14 = 1017
	}
}
