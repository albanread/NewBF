// expect: 1
// MS-T4: a `scope` object is auto-freed on the block's FALLTHROUGH exit, and its
// dtor fires EXACTLY ONCE (a counter bumped by the dtor proves both "fired" and,
// under the Stomp guard, "not twice" — a double-free would abort the harness).
class Tracked {
	public static int32 sDtors = 0;
	public ~this() { Tracked.sDtors += 1; }
}
class Program {
	public static int32 Main() {
		{
			Tracked t = scope Tracked();   // dominating scope alloc (Direct)
		}                                  // fallthrough exit → dtor runs once
		return Tracked.sDtors;             // 1
	}
}
