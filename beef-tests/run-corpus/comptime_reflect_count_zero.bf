// expect: 7
// CR-T3: the strip-differential companion to comptime_reflect_field_count.bf. An
// UNMARKED type (no `[Reflect(.Fields)]`) has its fields stripped, so
// `typeof(Plain).GetFieldCount()` reflects 0 — yet the `%struct.Type` global STILL
// exists (only the FieldInfo array is policy-gated), so the read returns a real 0,
// not a fault. The generator emits a member returning `0 + 7 = 7`, proving the
// generator observes the POLICY-GATED metadata at comptime: a marked type would
// emit a different constant (its real field count). This is the comptime-emit side
// of the runtime `reflect_strip_vs_marked.bf` differential.
//
// Same load-bearing details as the marquee: `n` is `int` (i64) so `n + 7` is i64
// and `Append(int)` (decimal) is the unambiguous overload; `delete s` exactly once
// (R10 — no double-free faults the compiler under Stomp); the single emitted member
// is byte-stable so the dedup converges (R11).
class Plain {                                          // NOT [Reflect(.Fields)] → fields stripped
	public int32 mA;
	public int32 mB;

	[Comptime, EmitGenerator]
	public static void Generate() {
		int n = typeof(Plain).GetFieldCount();         // 0 (stripped; Type global still present)
		String s = new String("public int32 Code() { return ");
		s.Append(n + 7);                               // (i64) 0 + 7 = 7 → Append(int)
		s.Append("; }");
		Compiler.EmitTypeBody(s);
		delete s;                                      // exactly once
	}
}

class Program {
	public static int32 Main() {
		Plain p = new Plain();
		int32 r = p.Code();                            // the EMITTED member returns 7
		delete p;
		return r;
	}
}
