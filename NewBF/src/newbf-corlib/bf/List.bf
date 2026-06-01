// NewBF corlib — List<T>: a growable generic array over Internal.
//
// Monomorphized per element type; the buffer is a raw T* indexed with T's
// stride, so List<int32> steps by 4 bytes and List<int> by 8 — the element
// type flows from the monomorph env into the typed-pointer machinery. v1
// over-allocates 8 bytes per slot (the max scalar width) pending a sizeof(T)
// operator; growth copies element-by-element through the typed pointer, so no
// byte-level memcpy (and no sizeof) is needed for the copy.
class List<T> {
	T* mItems;
	int mCount;
	int mCap;

	public this() {
		this.mCap = 4;
		this.mItems = Internal.Malloc(this.mCap * 8);
		this.mCount = 0;
	}
	public ~this() { Internal.Free(this.mItems); }

	public int Count() { return this.mCount; }
	public T Get(int i) { return this.mItems[i]; }

	public void Add(T v) {
		if (this.mCount >= this.mCap) { this.Grow(); }
		this.mItems[this.mCount] = v;
		this.mCount = this.mCount + 1;
	}

	// Index of the first element equal to `value` (by ==), or -1 if absent.
	public int32 IndexOf(T value) {
		for (int32 i = 0; i < this.mCount; i++) {
			if (this.mItems[i] == value) { return i; }
		}
		return -1;
	}
	// Whether `value` occurs anywhere in the list.
	public bool Contains(T value) {
		return this.IndexOf(value) >= 0;
	}

	void Grow() {
		int nc = this.mCap * 2;
		T* nb = Internal.Malloc(nc * 8);
		for (int i = 0; i < this.mCount; i++) {
			nb[i] = this.mItems[i];
		}
		Internal.Free(this.mItems);
		this.mItems = nb;
		this.mCap = nc;
	}
}
