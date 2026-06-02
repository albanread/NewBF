// expect: 3
// `when` guards on a payload-enum `match`: an arm matches only if its
// discriminant *and* its guard hold, and the guard can read the arm's payload
// binding. A guarded arm that matches the case but fails the guard falls through
// to later arms (here, to `default`).
//   a = Some(5)  → .Some(let v) when v > 10  fails (5 ≤ 10) → next arm
//                  .Some(let v) when v > 0    holds          → r = 1
//   b = Some(50) → .Some(let v) when v > 10  holds           → r = r + 2
// total = 1 + 2 = 3
enum IntOpt {
	case Some(int32 value);
	case None;
}
class Program {
	static int32 Classify(IntOpt o) {
		switch (o) {
		case .Some(let v) when v > 10: return 2;
		case .Some(let v) when v > 0:  return 1;
		default: return 0;
		}
	}
	public static int32 Main() {
		IntOpt a = IntOpt.Some(5);
		IntOpt b = IntOpt.Some(50);
		return Classify(a) + Classify(b); // 1 + 2 = 3
	}
}
