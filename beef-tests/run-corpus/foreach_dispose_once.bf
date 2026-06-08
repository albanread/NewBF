// expect: 1
// IT-T1: exactly-once `Dispose()` on the `break` edge. The enumerator carries a
// pointer to a counter; `Dispose()` increments `*mCounter`. `Main` runs a
// 3-element `foreach` with a mid-loop `break`, then returns the counter — which
// must be EXACTLY 1 (one loop, one Dispose, on the break edge). A MISSED Dispose
// (0) or a double-Dispose (2) both fail. The Stomp guard would not catch a
// missed call (it is a skipped call, not a double-free), so the counter is the
// direct detector. The Dispose hook lives in the loop's own `scope_allocs` frame
// (below the loop's captured depth), so `break`'s `free_scopes_down_to(depth)`
// leaves it alone and the single `exit`-block `free_scope_top` runs it once.
struct DispEnum {
	int32* mItems;
	int32 mCount;
	int32 mIndex;
	int32* mCounter;

	public bool MoveNext() {
		this.mIndex = this.mIndex + 1;
		return this.mIndex < this.mCount;
	}
	public int32 Current { get { return this.mItems[this.mIndex]; } }
	public void Dispose() { this.mCounter[0] = this.mCounter[0] + 1; }
}

struct DispBag {
	int32* mItems;
	int32 mCount;
	int32* mCounter;

	public DispEnum GetEnumerator() {
		DispEnum e = ?;
		e.mItems = this.mItems;
		e.mCount = this.mCount;
		e.mIndex = -1;
		e.mCounter = this.mCounter;
		return e;
	}
}

class Program {
	public static int32 Main() {
		int32* buf = Internal.Malloc(3 * 4);
		buf[0] = 5;
		buf[1] = 6;
		buf[2] = 7;
		int32* counter = Internal.Malloc(4);
		counter[0] = 0;
		DispBag bag = ?;
		bag.mItems = buf;
		bag.mCount = 3;
		bag.mCounter = counter;
		int32 sum = 0;
		for (var x in bag) {
			sum += x;
			if (x == 6) { break; }
		}
		int32 disposed = counter[0];
		Internal.Free(buf);
		Internal.Free(counter);
		return disposed;   // exactly 1 Dispose on the break edge
	}
}
