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
	// Indexer: `s[i]` is the i-th char (read-only). Assumes a valid index.
	public char8 this[int i] { get { return this.mPtr[i]; } }
	// The raw buffer, for length-based I/O (e.g. Console.Write over WriteFile).
	public char8* Ptr() { return this.mPtr; }

	// Concatenation: `a + b` builds a new owned String of a's bytes then b's.
	// Selected over any other `+` because both operands are String.
	public static String operator+(String a, String b) {
		String r = new String();
		r.Append(a);
		r.Append(b);
		return r;
	}
	// `a + c` appends a single char to a copy of `a`.
	public static String operator+(String a, char8 c) {
		String r = new String();
		r.Append(a);
		r.Append(c);
		return r;
	}

	// Value equality: same length and same bytes.
	public bool Equals(String other) {
		if (this.mLength != other.Length()) { return false; }
		for (int i = 0; i < this.mLength; i++) {
			if (this.mPtr[i] != other.CharAt(i)) { return false; }
		}
		return true;
	}

	// Index of the first occurrence of `c`, or -1 if not present.
	public int32 IndexOf(char8 c) {
		for (int32 i = 0; i < this.mLength; i++) {
			if (this.mPtr[i] == c) { return i; }
		}
		return -1;
	}
	// Whether `c` occurs anywhere in the string.
	public bool Contains(char8 c) {
		return this.IndexOf(c) >= 0;
	}

	// Whether this string begins with `prefix` (byte-for-byte).
	public bool StartsWith(String prefix) {
		int n = prefix.Length();
		if (n > this.mLength) { return false; }
		for (int i = 0; i < n; i++) {
			if (this.mPtr[i] != prefix.CharAt(i)) { return false; }
		}
		return true;
	}
	// Whether this string ends with `suffix` (byte-for-byte).
	public bool EndsWith(String suffix) {
		int n = suffix.Length();
		if (n > this.mLength) { return false; }
		int off = this.mLength - n;
		for (int i = 0; i < n; i++) {
			if (this.mPtr[off + i] != suffix.CharAt(i)) { return false; }
		}
		return true;
	}

	// A new String of `len` chars starting at `start`. Assumes valid arguments
	// (caller-checked); builds the result through the same Append path as Grow.
	public String Substring(int32 start, int32 len) {
		String r = new String();
		for (int32 i = 0; i < len; i++) {
			r.Append(this.mPtr[start + i]);
		}
		return r;
	}

	// Index of the first occurrence of `needle` (byte-for-byte), or -1. The empty
	// needle matches at 0. Naive O(n*m) scan — fine for the corlib slice.
	public int32 IndexOf(String needle) {
		int32 n = needle.Length();
		if (n == 0) { return 0; }
		for (int32 i = 0; i + n <= this.mLength; i++) {
			bool hit = true;
			for (int32 j = 0; j < n; j++) {
				if (this.mPtr[i + j] != needle.CharAt(j)) { hit = false; break; }
			}
			if (hit) { return i; }
		}
		return -1;
	}

	// A new String with ASCII letters upper-/lower-cased; other bytes pass through.
	public String ToUpper() {
		String r = new String();
		for (int32 i = 0; i < this.mLength; i++) {
			char8 c = this.mPtr[i];
			if (c >= 'a' && c <= 'z') { c = c - 32; }
			r.Append(c);
		}
		return r;
	}
	public String ToLower() {
		String r = new String();
		for (int32 i = 0; i < this.mLength; i++) {
			char8 c = this.mPtr[i];
			if (c >= 'A' && c <= 'Z') { c = c + 32; }
			r.Append(c);
		}
		return r;
	}

	static bool IsWs(char8 c) {
		return c == ' ' || c == '\t' || c == '\n' || c == '\r';
	}
	// A new String with leading and/or trailing ASCII whitespace removed.
	public String Trim() {
		int32 lo = 0;
		while (lo < this.mLength && String.IsWs(this.mPtr[lo])) { lo = lo + 1; }
		int32 hi = this.mLength;
		while (hi > lo && String.IsWs(this.mPtr[hi - 1])) { hi = hi - 1; }
		return this.Substring(lo, hi - lo);
	}
	public String TrimStart() {
		int32 lo = 0;
		while (lo < this.mLength && String.IsWs(this.mPtr[lo])) { lo = lo + 1; }
		return this.Substring(lo, this.mLength - lo);
	}
	public String TrimEnd() {
		int32 hi = this.mLength;
		while (hi > 0 && String.IsWs(this.mPtr[hi - 1])) { hi = hi - 1; }
		return this.Substring(0, hi);
	}

	// Split into substrings on each occurrence of `sep`. Always returns
	// (separator count + 1) parts; adjacent separators yield empty parts. The
	// caller owns the returned `String[]` and each `String` in it. Builds the
	// result through `Substring`, so each part is its own owned buffer.
	public String[] Split(char8 sep) {
		int32 count = 1;
		for (int32 i = 0; i < this.mLength; i++) {
			if (this.mPtr[i] == sep) { count = count + 1; }
		}
		String[] parts = new String[count];
		int32 idx = 0;
		int32 start = 0;
		for (int32 i = 0; i <= this.mLength; i++) {
			if (i == this.mLength || this.mPtr[i] == sep) {
				parts[idx] = this.Substring(start, i - start);
				idx = idx + 1;
				start = i + 1;
			}
		}
		return parts;
	}

	// A new String with every `from` replaced by `to`; length is unchanged.
	public String Replace(char8 from, char8 to) {
		String r = new String();
		for (int32 i = 0; i < this.mLength; i++) {
			char8 c = this.mPtr[i];
			if (c == from) { c = to; }
			r.Append(c);
		}
		return r;
	}
	// The number of times `c` occurs in the string.
	public int32 Count(char8 c) {
		int32 n = 0;
		for (int32 i = 0; i < this.mLength; i++) {
			if (this.mPtr[i] == c) { n = n + 1; }
		}
		return n;
	}
	// Index of the last occurrence of `c`, or -1 if not present.
	public int32 LastIndexOf(char8 c) {
		for (int32 i = this.mLength - 1; i >= 0; i--) {
			if (this.mPtr[i] == c) { return i; }
		}
		return -1;
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
