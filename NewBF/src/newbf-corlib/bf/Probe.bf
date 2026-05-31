// NewBF corlib — prelude probe.
//
// Proves the standard-library prelude is prepended to every program and usable
// with no local definition. This is scaffolding: it's replaced by the real
// System.* types (Internal, Object, String, …) as the corlib is ported — see
// docs/STDLIB.md.
class Probe {
	public int32 Answer() { return 42; }
}
