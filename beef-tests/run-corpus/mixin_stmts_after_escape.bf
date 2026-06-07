// expect: 42
// MX-T4 — stack discipline across an ESCAPING splice (mixins.md §3.6, R5). The
// `MaybeBail!(c)` mixin body conditionally `return`s from the caller; when the
// condition holds, the splice escapes and `self.terminated` is set so the
// statement after the call is correctly dead. Crucially the post-splice stack
// truncation runs UNCONDITIONALLY (even when terminated), so a LATER mixin call
// in the same caller is NOT desynced: `Plus!(40, 2)` (a value-yielding mixin
// spliced AFTER the conditional escape's join) computes 42 against a correctly
// balanced scope/defer/mixin stack.
//
// Two facets in one program: (1) inside the `if`, the `return 99;` escape path's
// trailing dead code is skipped; (2) on the taken (non-escaping, c == 0) path the
// later `Plus!` splice still lowers correctly — the desync the unconditional
// truncation prevents would corrupt this second splice.
class Program {
	static mixin MaybeBail(int32 c) {
		if (c != 0) {
			return 99;   // escapes the caller when c != 0
		}
	}
	static mixin Plus(int32 a, int32 b) => a + b;
	public static int32 Main() {
		int32 c = 0;
		MaybeBail!(c);          // c == 0 → does NOT escape; control continues
		int32 r = Plus!(40, 2); // a LATER splice — must work (no stack desync)
		return r;               // 42
	}
}
