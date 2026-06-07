// expect: 7
// CB-T4 (R6 eager-link regression): a `[Comptime, EmitGenerator]` emits a member
// that is NEVER called. The generator still RAN at compile time (so the emitter +
// the `__newbf_ct_emit` shim existed during emission), but the final module must
// still JIT-link clean: `run_emission` strips the generator AND the shim, so
// `lookup("Program.Main")` finds no dangling `__newbf_ct_emit`. `Main` returns a
// value from non-emitted code, proving the program links + runs despite the dead
// emitted member.
class Widget {
	public int32 mZ;
	public this(int32 z) { this.mZ = z; }

	[Comptime, EmitGenerator]
	public static void Generate() {
		// Emitted but never called by anyone.
		Compiler.EmitTypeBody("public int32 Unused() { return this.mZ + 99; }");
	}
}

class Program {
	public static int32 Main() {
		Widget w = new Widget(123);
		delete w;
		return 7;
	}
}
