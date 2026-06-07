// MX-T4 empty-loop guard (mixins.md §3.6): a mixin whose body `break`s/`continue`s
// is spliced at a call site with NO enclosing loop in the caller. Without the
// guard the splice would reach the `Stmt::Break`/`Continue` arms with an EMPTY
// `self.loops`; those arms `if let Some(..) = self.loops.last()` — they do not
// panic, but they would silently no-op WITHOUT setting `terminated`, a degenerate
// shape. v1 DECLINES this splice up front (`self.loops.is_empty()` &&
// `body_escapes_caller_loop`) so it falls back to the existing verifiable path
// (the synthetic call). This file must lower to a VERIFIER-CLEAN module with no
// panic — that is the gate (it is a verify-corpus fixture, not run).
//
// A control case (`SafeBreak`) shows the supported shape: the same `break` body
// spliced INSIDE a loop expands normally (its `self.loops` is non-empty), so the
// guard is narrow — it declines only the genuinely loop-less escape.
class MixinBreakOutsideLoop {
	// Body escapes to a CALLER loop (bare `break`/`continue` at body top level).
	static mixin EscapeBreak() {
		break;
	}
	static mixin EscapeContinue() {
		continue;
	}

	// Spliced where there is NO enclosing loop — must DECLINE → fall back, clean.
	public static int32 NoLoopHere() {
		EscapeBreak!();      // declined (empty caller loops) → synthetic call
		EscapeContinue!();   // declined likewise
		return 0;
	}

	// A `break` body INSIDE a loop the BODY itself opens does NOT escape the
	// caller — it targets the body's own loop — so the guard must not over-fire.
	static mixin OwnLoopBreak() {
		for (int32 i = 0; i < 4; i += 1) {
			if (i == 2) {
				break;   // targets THIS body's own loop, not the caller's
			}
		}
	}

	// Control: a caller WITH an enclosing loop — the guard does not fire here.
	public static int32 InLoop() {
		int32 i = 0;
		while (i < 10) {
			OwnLoopBreak!();   // body-own loop: expands (no caller-loop escape)
			i += 1;
		}
		return i;
	}
}
