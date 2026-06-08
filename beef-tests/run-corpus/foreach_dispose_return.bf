// expect: 1
// IT-T1: exactly-once `Dispose()` on the `return`-through edge (the §3.2 fix).
// A `return` inside a `foreach` body does NOT branch through the loop's `exit`
// block — `Stmt::Return` runs `free_all_scopes` then `ret`. So the Dispose hook
// must be registered in the `scope_allocs` frame `free_all_scopes` walks, or a
// `return`-out would skip Dispose entirely (counter == 0). The helper `Run`
// returns from MID-iteration; `Main` reads the counter AFTER `Run` returns —
// exactly 1 proves Dispose fired on the return edge. (Reading the counter inside
// `Run`'s own `return` would see 0, since the return value is captured before
// `free_all_scopes` runs — hence the count is observed in the caller.)
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
	// Runs the loop and `return`s out mid-iteration. The enumerator's Dispose
	// must still fire (via `free_all_scopes`) on this return edge.
	static void Run(DispBag bag) {
		for (var x in bag) {
			if (x == 6) { return; }
		}
	}

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
		Run(bag);
		int32 disposed = counter[0];
		Internal.Free(buf);
		Internal.Free(counter);
		return disposed;   // exactly 1 Dispose on the return-through edge
	}
}
