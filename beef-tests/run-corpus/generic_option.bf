// expect: 42
// Generic payload enum: the corlib `Option<T>` monomorphized to `Option<int32>`,
// constructed qualified (`Option<int32>.Some(40)` / `.None`) and destructured by
// `match`. Proves generic ADT monomorphization end-to-end — payload slots typed
// through the instantiation's `T → int32` env, per-mono `$disc`/`$p0`.
//   a = Some(40) → match binds v = 40  → r = 40
//   b = None     → match takes .None   → r = 40 + 2 = 42
class Program {
	public static int32 Main() {
		Option<int32> a = Option<int32>.Some(40);
		Option<int32> b = Option<int32>.None;
		int32 r = 0;
		switch (a) {
		case .Some(let v): r = v;
		case .None: r = -1;
		}
		switch (b) {
		case .Some(let v): r = r + v;
		case .None: r = r + 2;
		}
		return r; // 40 + 2 = 42
	}
}
