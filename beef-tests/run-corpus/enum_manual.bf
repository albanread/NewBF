// expect: 6
// The generic-value-struct enumerator ABI, proven in ISOLATION (IT-T0 / R7).
//
// `List<int32>.GetEnumerator()` returns a `ListEnumerator<int32>` BY VALUE — the
// first generic value struct with state-mutating instance methods to run on the
// executable corlib path. We enumerate it MANUALLY (no `foreach` yet — that's
// IT-T1): `e` is a single mutable local whose `mIndex` increment inside
// `MoveNext()` must PERSIST across calls. If the increment were lost (a reloaded
// copy per call), this loop would hang or read element 0 forever; reaching 6
// proves the value-struct `this`/state ABI works under JIT/Stomp.
//   list = [1, 2, 3]; sum over MoveNext/Current = 1 + 2 + 3 = 6
class Program {
	public static int32 Main() {
		List<int32> list = new List<int32>();
		list.Add(1);
		list.Add(2);
		list.Add(3);
		int32 sum = 0;
		var e = list.GetEnumerator();
		while (e.MoveNext()) {
			sum += e.Current;
		}
		delete list;
		return sum;   // 1 + 2 + 3 = 6
	}
}
