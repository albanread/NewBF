// expect: 1
// MX-T3 — param-bind-once (mixins.md §3.3 step 6): a mixin arg with a side effect
// is evaluated EXACTLY ONCE. `Bump()` increments a static counter and returns it;
// `UseTwice!(Bump())` binds its param `v` ONCE to the evaluated arg, then the body
// references `v` twice. If the arg were re-evaluated per use, `Counter.N` would be
// 2; param-bind-once keeps it at 1. The mixin yields `v - v` (== 0) so the value
// path is exercised without affecting the counter; Main returns `Counter.N`.
class Counter {
	public static int32 N = 0;
}
class Program {
	static int32 Bump() {
		Counter.N += 1;
		return Counter.N;
	}
	static mixin UseTwice(int32 v) => v - v;
	public static int32 Main() {
		int32 ignore = UseTwice!(Bump());
		return Counter.N;   // exactly 1 — the arg was evaluated once
	}
}
