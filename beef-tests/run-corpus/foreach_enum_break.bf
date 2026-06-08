// expect: 30
// IT-T1: `break` out of an enumerator `foreach`. Same value-struct `Bag` /
// `BagEnumerator` as foreach_getenumerator.bf. The body adds each element until
// it sees 30, then `break`s BEFORE adding it: 10 + 20 = 30. Pins the
// break-to-`exit` edge — `break` runs `free_scopes_down_to(depth)` (which does
// NOT touch the enumerator's Dispose frame, sitting below `depth`) then branches
// to `exit`, where the Dispose hook fires once. The committed semantics (stop at
// the third element) give an unambiguous 30.
struct BagEnumerator {
	int32* mItems;
	int32 mCount;
	int32 mIndex;

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
			if (x == 30) { break; }
			sum += x;
		}
		Internal.Free(buf);
		return sum;   // 10 + 20 = 30
	}
}
