// expect: 64
// Target-typed enum construction: `.Some(x)` / `.None` resolve against the
// declared (or return) type, so the monomorph is picked even when the case name
// is ambiguous. Here BOTH `Option<int32>` and `Option<bool>` exist, so `.Some`
// is ambiguous on its own — the target type disambiguates.
//   a : Option<int32> = .Some(40)   → target picks Option<int32>
//   b : Option<int32> = .None       → likewise
//   c : Option<bool>  = .Some(true) → target picks Option<bool>
// Match each back out:
//   a → 40 ; b → +2 ; c.Some(true) → +22  ⇒ 40 + 2 + 22 = 64
class Program {
	static Option<int32> Wrap(int32 x) {
		return .Some(x); // target-typed against the return type
	}
	public static int32 Main() {
		Option<int32> a = .Some(40);
		Option<int32> b = .None;
		Option<bool> c = .Some(true);

		int32 r = 0;
		switch (a) {
		case .Some(let v): r = r + v;   // 40
		case .None: r = r - 1;
		}
		switch (b) {
		case .Some(let v): r = r + v;
		case .None: r = r + 2;          // 42
		}
		switch (c) {
		case .Some(let v): if (v) { r = r + 22; }  // 64
		case .None: r = r - 1;
		}
		Option<int32> d = Wrap(0);
		switch (d) {
		case .Some(let v): r = r + v;   // +0
		case .None: r = r - 100;
		}
		return r; // 40 + 2 + 22 + 0 = 64
	}
}
