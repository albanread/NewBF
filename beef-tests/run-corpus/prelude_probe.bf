// expect: 42
// `Probe` is NOT defined here — it lives in newbf-corlib/bf/Probe.bf and is
// prepended to every program by the corlib prelude. If this resolves and runs,
// the standard-library plumbing works (composed at the AST, lowered once).
class Program {
	public static int32 Main() {
		Probe p = new Probe();
		return p.Answer();
	}
}
