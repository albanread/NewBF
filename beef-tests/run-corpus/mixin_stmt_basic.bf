// expect: 30
// MX-T3 — the model EXPAND case: a STATEMENT mixin that mutates a caller local.
// `AddTen!()` references the free name `total`, which resolves in the CALLER's
// live scope (the splice reuses the live Lowerer where `total` is bound), so the
// widened free-name gate (mixins.md §3.4) admits it. The body's `total += 10`
// runs against the caller's slot; called twice → 10 + 10 + 10 = 30.
class Program {
	static mixin AddTen() {
		total += 10;
	}
	public static int32 Main() {
		int32 total = 10;
		AddTen!();
		AddTen!();
		return total;   // 10 + 10 + 10 = 30
	}
}
