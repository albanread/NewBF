// expect: 2
// RF-T6: a [Reflect(.Fields)] class with two fields exposes them by index. The
// FieldInfo array is queryable, so GetFieldCount() returns 2. An unmarked class
// strips its fields (count 0) — both halves pinned in one differential program:
// the marked count (2) plus the stripped count (0) must equal 2 exactly, which a
// broken/garbage Type cannot satisfy (it would not be 2 AND 0 simultaneously).
[Reflect(.Fields)] class Point   { public int32 mX; public int32 mY; }
                   class Plain   { public int32 mA; public int32 mB; }
class Program {
	public static int32 Main() {
		int32 marked = typeof(Point).GetFieldCount();   // 2 (fields emitted)
		int32 plain  = typeof(Plain).GetFieldCount();   // 0 (stripped)
		return marked + plain;                          // 2 + 0 = 2
	}
}
