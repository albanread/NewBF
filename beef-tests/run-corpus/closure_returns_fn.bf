// expect: 15
// FV-T7 — a function that BUILDS a capturing closure and RETURNS it as a
// function value. `MakeAdder(n)` returns `x => x + n`, a capturing closure whose
// env is `malloc`'d on the heap (it holds the captured `n`). Because the env is
// heap-allocated (and leaked — see §10), the by-value capture SURVIVES the
// return of `MakeAdder`'s frame: when the caller later calls the returned
// function value, `n` is still readable. This pins the `Func$` RETURN type
// (Slice A) end-to-end: the closure crosses a return boundary and is then called.
//   MakeAdder(5)  → closure capturing n=5
//   add5(10)      → 10 + 5 = 15
class Program {
	public static function int32(int32) MakeAdder(int32 n) {
		return x => x + n;          // CAPTURING closure; env is malloc'd (heap)
	}
	public static int32 Main() {
		function int32(int32) add5 = MakeAdder(5);
		return add5(10);            // captured n=5 survives the return → 15
	}
}
