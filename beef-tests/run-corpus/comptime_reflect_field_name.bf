// expect: 1
// CR-T4: THE name-driven comptime-reflection marquee — reflection field NAMES drive
// codegen. A `[Comptime, EmitGenerator]` generator reads the first field's NAME
// (`typeof(Tagged).GetField(0).GetName()` → a `char8*`) AT COMPILE TIME and EMITS a
// predicate member whose behavior depends on that name. The value 1 is computable
// only if the generator saw the reflected field name "mX" in the emission sandbox
// JIT (off the `%struct.Type`/FieldInfo globals the sandbox clone's `emit_metadata`
// built) and emitted a member that re-derives the SAME name at runtime and compares.
//
// R5 — the value-struct method-chain trap (the highest-probability bug). `GetField(i)`
// returns a value-struct `FieldInfo` BY VALUE; a chained
// `typeof(T).GetField(0).GetName()` lowers the rvalue receiver to `undef`. So BOTH the
// generator code AND the EMITTED runtime text BIND a `FieldInfo` LOCAL first:
//   FieldInfo f = typeof(Tagged).GetField(0); char8* nm = f.GetName();
// The emitted text below splices a method that binds `FieldInfo f` before `.GetName()`.
//
// StrEq caveat (CR-T2): `Internal.StrEq` compares two `char8*` — the reflected name
// (a NUL-terminated `char8*` from the FieldInfo metadata) vs a `char8*` string LITERAL
// ("mX"). NOT against a built String's `Ptr()` (the String buffer isn't NUL-terminated).
//
// Quoting: the emit text is built with a `String` builder (CR-T0's runtime-String
// EmitTypeBody path) + `Append(char8*)` (CR-T2) for the reflected NAME + `Append`'d
// literal parts. The emitted member text contains a NESTED Beef string literal (the
// "mX" the runtime StrEq compares against) — the generator emits the opening `"`,
// Appends the reflected name char8* (= "mX") between the quotes, then emits `"`.
//
// Memory (R10): the generator's `new String` object body routes through `newbf_alloc`
// → the Stomp ledger DURING COMPILATION (the run-corpus harness runs the whole
// pipeline, including this sandbox generator, under GuardMode::Stomp). It `delete s`
// EXACTLY ONCE so the dtor frees the buffer with no double-free that would fault the
// compiler. The emitted text is byte-stable round-to-round (a single idempotent member
// from a deterministic reflection read), so the `seen` dedup converges (R11).
[Reflect(.Fields)]
class Tagged {
	public int32 mX;

	[Comptime, EmitGenerator]
	public static void Generate() {
		// The emitted method binds a FieldInfo LOCAL (not a chained rvalue, R5),
		// re-derives the field name at RUNTIME, and StrEqs it against the literal
		// the generator read at COMPILE TIME — both must be "mX". The trailing open
		// quote `\"` begins the nested Beef string literal the reflected name fills.
		String s = new String(
			"public bool FirstFieldIsMX() { FieldInfo f = typeof(Tagged).GetField(0); return Internal.StrEq(f.GetName(), \"");
		// Generator-side: bind a FieldInfo LOCAL too (R5 — never chain off the rvalue).
		FieldInfo gf = typeof(Tagged).GetField(0);
		s.Append(gf.GetName());                          // Append(char8*) — the reflected NAME ("mX")
		s.Append("\"); }");                              // close the nested literal + the method body
		Compiler.EmitTypeBody(s);                        // runtime String, NOT a literal
		delete s;                                        // exactly once → no double-free
	}
}

class Program {
	public static int32 Main() {
		Tagged t = new Tagged();
		bool ok = t.FirstFieldIsMX();                    // the EMITTED predicate: first field IS "mX"
		delete t;
		return ok ? 1 : 0;
	}
}
