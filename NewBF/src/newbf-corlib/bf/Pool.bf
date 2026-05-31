// NewBF corlib — Pool: generational handles (a slotmap).
//
// The safe-reference idea borrowed from Locus's GC handles (docs/GC.md §12),
// but demoted to an *optional manual primitive* — not a collector. A handle is
// a pointer-sized `int` packing `(generation << 16) | index`. `Get` resolves a
// handle to its object while the slot is live, and returns null once `Free`
// bumps that slot's generation. So a stale handle (use-after-free) surfaces as
// a detectable null instead of a dangling pointer — safety over raw malloc,
// with no GC and without giving up raw pointers anywhere else.
//
// v1 is a bump slotmap (no slot reuse, no growth); the generation field already
// gives the safety property. Reuse/growth and a typed `Handle<T>` wrapper wait
// on generics.
class Pool {
	int* mObjs;   // object pointers, stored as ints (int is pointer-sized)
	int* mGens;   // per-slot generation
	int mCap;
	int mCount;   // next slot to hand out

	public this() {
		this.mCap = 1024;
		this.mObjs = Internal.Malloc(this.mCap * 8);
		this.mGens = Internal.Malloc(this.mCap * 8);
		this.mCount = 0;
		for (int i = 0; i < this.mCap; i++) {
			this.mGens[i] = 0;
		}
	}
	public ~this() {
		Internal.Free(this.mObjs);
		Internal.Free(this.mGens);
	}

	// Register an object, returning a generational handle.
	public int Alloc(void* obj) {
		int idx = this.mCount;
		this.mCount = this.mCount + 1;
		this.mObjs[idx] = obj;          // void* -> int (ptrtoint on store)
		int gen = this.mGens[idx];
		return (gen << 16) | idx;
	}

	// Resolve a handle to its object, or null if the handle is stale (Freed).
	public void* Get(int h) {
		int idx = h & 0xFFFF;
		int gen = h >> 16;
		if (gen != this.mGens[idx]) {
			return null;
		}
		return this.mObjs[idx];         // int -> void* (inttoptr on return)
	}

	// Invalidate the slot: every existing handle to it becomes stale.
	public void Free(int h) {
		int idx = h & 0xFFFF;
		this.mGens[idx] = this.mGens[idx] + 1;
	}
}
