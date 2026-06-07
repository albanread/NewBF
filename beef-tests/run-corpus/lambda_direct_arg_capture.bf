// expect: 69
// FV-T6 — a CAPTURING inline lambda directly in call-arg position. The lambda
// `x => x + b` captures the outer local `b` and is written right at the generic
// HOF call (no intermediate `function`-typed local). Capture works because the
// lambda's creation site is the call expression, which lowers inside the
// enclosing method body where `b` is live — so `detect_captures` reads it and
// the lambda becomes a `$Func {code, target=env}` exactly as a captured lambda
// assigned to a local does. A second capturing inline lambda (`x => x * k`) pins
// multi-call capture.
//   xs                  = [1, 2, 3, 4]
//   b = 10, k = 3
//   .Map<int32>(x + b)  → [11, 12, 13, 14]   (b captured)
//   .Map<int32>(x * k)  → [33, 36, 39, 42]   (k captured)
//   .Filter(x > 35)     → [36, 39, 42]
//   .Fold<int32>(0, +)  → 36 + 39 + 42 = 117
//   return 117 - 48     → 69                  (kept ≤ 255 for AOT-safety)
class Program {
	public static int32 Main() {
		List<int32> xs = new List<int32>();
		xs.Add(1);
		xs.Add(2);
		xs.Add(3);
		xs.Add(4);

		int32 b = 10;
		int32 k = 3;
		// Capturing inline lambda (captures `b`) directly as the generic-method arg.
		List<int32> step1 = xs.Map<int32>(x => x + b);   // [11, 12, 13, 14]
		// Capturing inline lambda (captures `k`).
		List<int32> step2 = step1.Map<int32>(x => x * k); // [33, 36, 39, 42]
		List<int32> kept = step2.Filter(x => x > 35);     // [36, 39, 42]
		int32 sum = kept.Fold<int32>(0, (acc, x) => acc + x); // 117

		int32 c = 48;
		return sum - c; // 117 - 48 = 69
	}
}
