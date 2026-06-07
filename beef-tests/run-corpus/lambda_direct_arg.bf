// expect: 90
// FV-T6 — an INLINE lambda directly in call-arg position, with NO intermediate
// `function`-typed local. `xs.Map<int32>(x => x * 10)` collects the inline
// lambda a `$lambdaN` symbol (T6a, the pre-pass now walks call args) and
// target-types its `x` from the resolved `function R(T) f` callee param (T6b).
// Exercises the idiomatic instance generic HOF (`List<T>.Map<R>`/`.Filter`/
// `.Fold<A>`) with the lambda written right at the call, plus a non-generic
// instance HOF (`.Filter`) taking an inline lambda.
//   xs                = [1, 2, 3, 4]
//   .Map<int32>(*10)  → [10, 20, 30, 40]
//   .Filter(x > 15)   → [20, 30, 40]
//   .Fold<int32>(0,+) → 90
class Program {
	public static int32 Main() {
		List<int32> xs = new List<int32>();
		xs.Add(1);
		xs.Add(2);
		xs.Add(3);
		xs.Add(4);

		// Inline lambda directly as the generic-method arg (no `function` local).
		List<int32> scaled = xs.Map<int32>(x => x * 10);   // [10, 20, 30, 40]
		// Inline lambda to a NON-generic instance HOF.
		List<int32> kept = scaled.Filter(x => x > 15);     // [20, 30, 40]
		// Inline lambda (two params) to a generic instance HOF.
		int32 sum = kept.Fold<int32>(0, (acc, x) => acc + x); // 20+30+40 = 90

		return sum;
	}
}
