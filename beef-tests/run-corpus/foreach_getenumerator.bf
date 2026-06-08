// expect: 60
// IT-T1: the FIFTH `foreach` branch — the enumerator protocol over a USER type.
//
// `Bag` is a value struct exposing `GetEnumerator()` (returns a value-struct
// `BagEnumerator` cursor over an inline 3-slot buffer) with `MoveNext()` ->
// bool and a `Current` property. `foreach (var x in bag)` resolves none of the
// Count/Get fast path (Bag has neither), so it takes the new GetEnumerator
// branch. The load-bearing property: `BagEnumerator` is a VALUE struct whose
// `MoveNext()` mutates `mIndex` through `this` — the `e_slot` alloca address
// must be reused as `this` across every iteration, or the increment is lost and
// the loop hangs / reads slot 0 forever (R6). Summing {10,20,30} → 60 proves
// the state persists and `Current` reads through the same slot.
struct BagEnumerator {
	int32* mItems;
	int32 mCount;
	int32 mIndex;       // -1 before the first MoveNext

	public bool MoveNext() {
		this.mIndex = this.mIndex + 1;
		return this.mIndex < this.mCount;
	}
	public int32 Current { get { return this.mItems[this.mIndex]; } }
	public void Dispose() { }
}

struct Bag {
	int32* mItems;
	int32 mCount;

	public BagEnumerator GetEnumerator() {
		BagEnumerator e = ?;
		e.mItems = this.mItems;
		e.mCount = this.mCount;
		e.mIndex = -1;
		return e;
	}
}

class Program {
	public static int32 Main() {
		int32* buf = Internal.Malloc(3 * 4);
		buf[0] = 10;
		buf[1] = 20;
		buf[2] = 30;
		Bag bag = ?;
		bag.mItems = buf;
		bag.mCount = 3;
		int32 sum = 0;
		for (var x in bag) {
			sum += x;
		}
		Internal.Free(buf);
		return sum;   // 10 + 20 + 30 = 60
	}
}
