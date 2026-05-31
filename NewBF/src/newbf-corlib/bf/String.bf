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
	// The raw buffer, for length-based I/O (e.g. Console.Write over WriteFile).
	public char8* Ptr() { return this.mPtr; }

	// Value equality: same length and same bytes.
	public bool Equals(String other) {
		if (this.mLength != other.Length()) { return false; }
		for (int i = 0; i < this.mLength; i++) {
			if (this.mPtr[i] != other.CharAt(i)) { return false; }
		}
		return true;
	}

	public void Append(char8 c) {
		if (this.mLength >= this.mCapacity) { this.Grow(); }
		this.mPtr[this.mLength] = c;
		this.mLength = this.mLength + 1;
	}
	// Append-overloaded by argument type: this appends a whole String by
	// delegating to Append(char8) per character — selected over Append(char8)
	// because `other` is a String, not a char8.
	public void Append(String other) {
		for (int i = 0; i < other.Length(); i++) {
			this.Append(other.CharAt(i));
		}
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
