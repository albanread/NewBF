// expect: 55
// Algebraic data types: a payload enum (tagged union) + `match` with a binding
// pattern. `IntOpt` is `{ Some(int32), None }` — it lowers to a value struct
// `{$disc:i32, $p0:i32}`. A case *constructs* that struct (discriminant + payload);
// `switch` tests the discriminant and *binds* the payload in the matched arm.
//   a = Some(42) → matches .Some(let v), v = 42 → r = 42
//   b = None     → matches .None              → r = 42 + 13 = 55
enum IntOpt {
	case Some(int32 value),
	case None
}
class Program {
	public static int32 Main() {
		IntOpt a = IntOpt.Some(42);
		int32 r = 0;
		switch (a) {
		case .Some(let v): r = v;
		case .None: r = -1;
		}
		IntOpt b = IntOpt.None;
		switch (b) {
		case .Some(let v): r = r + v;
		case .None: r = r + 13;
		}
		return r; // 42 + 13 = 55
	}
}
