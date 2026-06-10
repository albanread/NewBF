// expect: 2
// CR-T3: THE comptime-reflection marquee — reflection-driven codegen. A
// `[Comptime, EmitGenerator]` generator reads `typeof(Pair).GetFieldCount()` AT
// COMPILE TIME (a reflection-metadata read in the emission sandbox JIT, off the
// `%struct.Type` global the sandbox clone's `emit_metadata` built) and EMITS a
// member whose body RETURNS that count. The value 2 is computable only if the
// generator saw the two reflected fields and emitted a member that re-resolves
// (spliced as `extension Pair { … }`, re-analyzed, re-lowered) and runs — i.e.
// the generated method's return value is itself a compile-time reflection read.
//
// Widening note (CR-T1's gotcha): `GetFieldCount()` is `int32`, but binding it to
// an `int` (i64) local widens it so the `s.Append(int)` DECIMAL overload is the
// unambiguous pick. A bare `int32` arg ties `Append(char8)` (score 1) with
// `Append(int)` (score 1) and first-wins selects `Append(char8)` — emitting the
// char code, not "2". `int n` makes `Append(int)` an exact (score-2) match.
//
// Memory (R10): the generator's `new String` object body routes through
// `newbf_alloc` → the Stomp ledger DURING COMPILATION (the run-corpus harness runs
// the whole pipeline, including this sandbox generator, under GuardMode::Stomp). It
// `delete s` EXACTLY ONCE so the dtor frees the buffer with no double-free that
// would fault the compiler. The emitted text is byte-stable round-to-round (a
// single idempotent member from a deterministic reflection read), so the `seen`
// dedup converges (R11).
[Reflect(.Fields)]
class Pair {
	public int32 mA;
	public int32 mB;

	[Comptime, EmitGenerator]
	public static void Generate() {
		// Reflect at COMPILE TIME: count this type's fields. typeof(Pair) is a
		// Ref(Type) rvalue, so GetFieldCount() resolves directly (no value-struct
		// chain). The Type global lives in the sandbox clone.
		int n = typeof(Pair).GetFieldCount();          // 2 (widened to int = i64)
		// Build the member source from the reflected count.
		String s = new String("public int32 FieldCount() { return ");
		s.Append(n);                                   // "...return 2"  (Append(int), decimal)
		s.Append("; }");                               // literal auto-wraps to String
		Compiler.EmitTypeBody(s);                      // runtime String, NOT a literal
		delete s;                                      // exactly once → no double-free
	}
}

class Program {
	public static int32 Main() {
		Pair p = new Pair();
		int32 r = p.FieldCount();                      // the EMITTED member returns 2
		delete p;
		return r;
	}
}
