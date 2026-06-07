// expect: 9
// TA-7 collection recursion (the genuinely new mechanism, §6 / R4): a generic
// MONOMORPH used ONLY inside a PENDING arg's body must still be collected, or its
// mono is never emitted (a dangling symbol → a verify/link error). The generic
// METHOD `Pick<int32>` is referenced NOWHERE in the program EXCEPT inside the
// `.{ v = Pick<int32>(9) }` initializer entry — a target-typed `.{ }` argument to
// `Take`. (A generic method's mono is collected from its CALL-SITE expression via
// `record_method_inst`, not from any type/field, so unlike a generic-type field it
// is reachable ONLY by walking the pending arg body.) The fork lowers that `.{ }`
// against the resolved value-struct `Holder` param, which lowers `Pick<int32>(9)`
// — so `collect_insts_expr` MUST recurse into the `Initializer`'s entries (the new
// arm). Without that recursion, lowering would call an un-emitted `Pick$i32` symbol
// (dangling) and the program would not run. This was VERIFIED load-bearing:
// disabling the Initializer arm makes the mono uncollected.
//   Take(.{ v = Pick<int32>(9) }) reads h.v = 9
struct Holder {
	public int32 v;
}
class Program {
	// A generic method used ONLY inside the pending `.{ }` arg body below.
	static T Pick<T>(T x) { return x; }
	static int32 Take(Holder h) { return h.v; }
	public static int32 Main() {
		// `.{ … }` is a PENDING arg; its entry body calls `Pick<int32>` — a mono
		// referenced nowhere else, collected via the new Initializer-entry recursion.
		return Take(.{ v = Pick<int32>(9) }); // 9
	}
}
