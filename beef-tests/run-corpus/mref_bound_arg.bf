// expect: 50
// FV-T5 — a BOUND instance method reference passed to a generic HOF. `m.Times`
// is a function value whose `code` is a `$mrefb$<full>($self,P…){ return
// ((Scaler)$self).Times(P…); }` thunk and whose `target` is the receiver `m`
// (its body pointer). The thunk forwards `$self` as the method's `this`, so the
// bound method runs on the RIGHT object and reads the right `factor`. This
// distinguishes the bound case (target = receiver) from the static `Mathx.Square`
// case (target = null). `Times` is NON-VIRTUAL (virtual dispatch through a bound
// ref is deferred — the thunk binds the concrete `full_name`).
//   xs        = [1, 2, 3, 4]
//   m.factor  = 5
//   m.Times   → [5, 10, 15, 20]
//   Fold(+,0) → 50
class Scaler {
	public int32 factor;
	public this(int32 f) { this.factor = f; }
	public int32 Times(int32 x) { return x * this.factor; }
}
class Program {
	public static int32 Main() {
		List<int32> xs = new List<int32>();
		xs.Add(1);
		xs.Add(2);
		xs.Add(3);
		xs.Add(4);
		Scaler m = new Scaler(5);
		function int32(int32) f = m.Times;               // BOUND instance method ref
		List<int32> ys = Map<int32, int32>(xs, f);       // [5, 10, 15, 20]
		function int32(int32, int32) plus = (acc, x) => acc + x;
		int32 r = Fold<int32, int32>(ys, 0, plus);       // 5 + 10 + 15 + 20 = 50
		delete m;   // MS-T5.5: balance `new Scaler(5)` after its bound ref is done
		return r;
	}
}
