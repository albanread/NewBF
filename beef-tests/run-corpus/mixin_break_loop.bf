// expect: 3
// MX-T4 — `break` inside a spliced mixin body targets the CALLER's innermost loop
// (mixins.md §3.6). `BreakOut!()` body is `break;`; spliced inside Main's `while`
// it exits THAT loop (the caller's `self.loops.last()`), not a callee. The loop
// increments `i` each turn and calls the mixin once `i == 3`, so the loop ends
// early with `i == 3` — proving the break crossed the splice boundary into the
// caller's loop. (The empty-loop guard does NOT fire here: `self.loops` is
// non-empty at the splice point, so the mixin expands.)
class Program {
	static mixin BreakOut() {
		break;
	}
	public static int32 Main() {
		int32 i = 0;
		while (i < 100) {
			if (i == 3) {
				BreakOut!();   // breaks the caller's `while` loop
			}
			i += 1;
		}
		return i;   // 3 — loop ended early via the spliced break
	}
}
