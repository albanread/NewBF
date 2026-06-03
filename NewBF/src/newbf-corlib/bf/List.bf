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

	// A fresh heap `T[]` holding a copy of the elements (length = Count). The
	// caller owns it. `new T[n]` sizes by `T`'s stride via the monomorph env, so
	// the array is packed at the element width (4 bytes for `List<int32>`).
	public T[] ToArray() {
		T[] a = new T[this.mCount];
		for (int i = 0; i < this.mCount; i++) {
			a[i] = this.mItems[i];
		}
		return a;
	}

	// Indexer: `xs[i]` reads/writes the element in place (same as Get/Set, the
	// idiomatic form). Assumes a valid index (caller-checked).
	public T this[int i] {
		get { return this.mItems[i]; }
		set { this.mItems[i] = value; }
	}

	public void Add(T v) {
		if (this.mCount >= this.mCap) { this.Grow(); }
		this.mItems[this.mCount] = v;
		this.mCount = this.mCount + 1;
	}

	// Append every element of `other` (in order). Reads through `other`'s public
	// surface, so it works for any same-typed list.
	public void AddRange(List<T> other) {
		for (int i = 0; i < other.Count(); i++) {
			this.Add(other.Get(i));
		}
	}
	// First / last element. Assume non-empty (caller-checked).
	public T First() { return this.mItems[0]; }
	public T Last() { return this.mItems[this.mCount - 1]; }

	// Remove the element at `index`, shifting later elements down one slot and
	// decrementing the count. Assumes a valid index (caller-checked).
	public void RemoveAt(int32 index) {
		for (int32 i = index; i < this.mCount - 1; i++) {
			this.mItems[i] = this.mItems[i + 1];
		}
		this.mCount = this.mCount - 1;
	}
	// Drop all elements (count to 0) while keeping the buffer for reuse.
	public void Clear() {
		this.mCount = 0;
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

	// Insert `value` at `index`, shifting elements [index..] up one slot.
	// Assumes 0 <= index <= count (caller-checked).
	public void Insert(int32 index, T value) {
		if (this.mCount >= this.mCap) { this.Grow(); }
		for (int i = this.mCount; i > index; i--) {
			this.mItems[i] = this.mItems[i - 1];
		}
		this.mItems[index] = value;
		this.mCount = this.mCount + 1;
	}
	// Reverse the elements in place.
	public void Reverse() {
		int i = 0;
		int j = this.mCount - 1;
		while (i < j) {
			T tmp = this.mItems[i];
			this.mItems[i] = this.mItems[j];
			this.mItems[j] = tmp;
			i = i + 1;
			j = j - 1;
		}
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

// Higher-order functions over List<T> — the payoff of lambdas/closures. These
// are free static generics, called bare (`Map<int,int>(xs, f)`): a generic
// *method* on the generic *type* List<T> (i.e. `xs.Map<R>(f)`) isn't supported
// yet (global-name mangling), so the receiver is an explicit `self` parameter.
// The `function R(T) f` parameter is callable inside the body (the higher-order
// payoff), so each method just walks the list applying `f`. Non-capturing
// lambdas and method references work as the function argument today; capturing
// closures await the uniform function-value representation.
class Functional {
	// A new list holding `f` applied to each element of `self`.
	public static List<R> Map<T, R>(List<T> self, function R(T) f) {
		List<R> r = new List<R>();
		for (int i = 0; i < self.Count(); i++) {
			r.Add(f(self.Get(i)));
		}
		return r;
	}

	// A new list of the elements of `self` for which `pred` holds.
	public static List<T> Filter<T>(List<T> self, function bool(T) pred) {
		List<T> r = new List<T>();
		for (int i = 0; i < self.Count(); i++) {
			T x = self.Get(i);
			if (pred(x)) { r.Add(x); }
		}
		return r;
	}

	// Left-fold: thread `seed` through `f(acc, elem)` over every element.
	public static A Fold<T, A>(List<T> self, A seed, function A(A, T) f) {
		A acc = seed;
		for (int i = 0; i < self.Count(); i++) {
			acc = f(acc, self.Get(i));
		}
		return acc;
	}
}
