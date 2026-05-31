// NewBF corlib — System.String (Milestone A: separate-buffer, no append-alloc).
// Owns a char8* buffer allocated through Internal and grows it on Append. The
// single-allocation/SSO fidelity (Milestone B) arrives later — see CORETYPES.md.
class String {
	char8* mPtr;
	int mLength;
	int mCapacity;

	public this() {
		this.mCapacity = 4;
		this.mPtr = Internal.Malloc(this.mCapacity);
		this.mLength = 0;
	}
	public this(char8* s) {
		int n = 0;
		while (s[n] != 0) { n = n + 1; }
		this.mCapacity = n + 1;
		this.mPtr = Internal.Malloc(this.mCapacity);
		Internal.MemCpy(this.mPtr, s, n);
		this.mLength = n;
	}
	public ~this() { Internal.Free(this.mPtr); }

	public int Length() { return this.mLength; }
	public char8 CharAt(int i) { return this.mPtr[i]; }

	public void Append(char8 c) {
		if (this.mLength >= this.mCapacity) { this.Grow(); }
		this.mPtr[this.mLength] = c;
		this.mLength = this.mLength + 1;
	}
	void Grow() {
		int nc = this.mCapacity * 2;
		char8* nb = Internal.Malloc(nc);
		Internal.MemCpy(nb, this.mPtr, this.mLength);
		Internal.Free(this.mPtr);
		this.mPtr = nb;
		this.mCapacity = nc;
	}
}
