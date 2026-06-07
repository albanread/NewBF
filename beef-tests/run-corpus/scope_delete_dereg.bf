// expect: 1
// MS-T4: an explicit `delete` of a `scope`-bound local de-registers it from the
// scope tracking, so the automatic frame cleanup does NOT free it again. The
// dtor must therefore fire EXACTLY ONCE (from the manual delete) — not twice.
// Under the Stomp guard a missed de-registration would be a double-free → abort,
// so a passing value here also proves the de-registration worked.
class Tracked {
	public static int32 sDtors = 0;
	public ~this() { Tracked.sDtors += 1; }
}
class Program {
	public static int32 Main() {
		{
			Tracked t = scope Tracked();
			delete t;            // manual delete → dtor #1; de-registers t
		}                        // scope exit must NOT free t again
		return Tracked.sDtors;   // 1 (exactly once)
	}
}
