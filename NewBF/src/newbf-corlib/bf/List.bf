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

	// A by-value cursor over this list's buffer: GetEnumerator copies the buffer
	// pointer + count into a `ListEnumerator<T>` value (declared top-level below,
	// NOT nested — `index_generic_decls` is member-blind, lower.rs:655-692, so a
	// nested generic enumerator would never monomorphize). Returned by value, so
	// the caller copies it into its own slot; no heap allocation. This is the
	// enumerator `foreach` (IT-T1) will drive; today it's exercised manually by
	// `enum_manual.bf` to prove the generic-value-struct ABI in isolation (R7).
	public ListEnumerator<T> GetEnumerator() {
		ListEnumerator<T> e = ?;
		e.mItems = this.mItems;
		e.mCount = this.mCount;
		e.mIndex = -1;          // -1 before the first MoveNext
		return e;
	}

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

	// In-place ascending insertion sort using `<` on the element type. Stable and
	// O(n^2) — fine for the small lists corlib v1 targets. Requires `T` to support
	// `<` (any numeric element does); a comparable-constraint comes with the
	// generic-constraint sprint.
	public void Sort() {
		for (int i = 1; i < this.mCount; i++) {
			T key = this.mItems[i];
			int j = i - 1;
			while (j >= 0 && key < this.mItems[j]) {
				this.mItems[j + 1] = this.mItems[j];
				j = j - 1;
			}
			this.mItems[j + 1] = key;
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

	// --- Higher-order functions, the idiomatic instance form (GM-B2) ---------
	// These are generic methods on the generic owner `List<T>` (so they exercise
	// generic-methods B1) taking a uniform function-value (`Func$`) parameter (so
	// they exercise fn-values Slice A). The receiver is the implicit `this`: each
	// method reads `this.mCount`/`this.mItems` directly — no explicit `self`. A
	// capturing closure, a non-capturing lambda, or a method reference all work as
	// the function argument. `xs.Map<R>(f)` mangles to an owner-mono-prefixed
	// symbol like `@List$i32.Map$i32`. (The free static `Functional.Map/Filter/Fold`
	// below are retained for the bare-cross-class call shape; this is the payoff form.)

	// A new `List<R>` holding `f` applied to each element (in order). `R` is the
	// method's own type-param, distinct from the owner's `T`, so `Map` can change
	// the element type.
	public List<R> Map<R>(function R(T) f) {
		List<R> r = new List<R>();
		for (int i = 0; i < this.mCount; i++) {
			r.Add(f(this.mItems[i]));
		}
		return r;
	}

	// A new `List<T>` of the elements for which `pred` holds. Non-generic method
	// on the generic owner — `T` comes from the owner.
	public List<T> Filter(function bool(T) pred) {
		List<T> r = new List<T>();
		for (int i = 0; i < this.mCount; i++) {
			T x = this.mItems[i];
			if (pred(x)) { r.Add(x); }
		}
		return r;
	}

	// Left-fold: thread `seed` through `f(acc, elem)` over every element. `A` is
	// the method's own type-param (the accumulator type).
	public A Fold<A>(A seed, function A(A, T) f) {
		A acc = seed;
		for (int i = 0; i < this.mCount; i++) {
			acc = f(acc, this.mItems[i]);
		}
		return acc;
	}
}

// A by-value enumerator over a List<T>: an index cursor + a borrowed buffer
// pointer + the element count. TOP-LEVEL generic (a sibling of List<T>, NOT
// nested in it) so `index_generic_decls` collects it for monomorphization —
// that walk is member-blind (lower.rs:655-692), so a nested generic enumerator
// would never be instantiated and would lower to the `Ptr` fallback.
//
// It is a VALUE `struct` (not a class): `GetEnumerator()` returns it by value,
// the caller copies it into its own slot, and `MoveNext()` mutates `mIndex` in
// place through `this`. This is the FIRST generic value struct with
// state-mutating instance methods to run on the executable corlib path (R7);
// `enum_manual.bf` proves the ABI in isolation (manual MoveNext/Current, no
// foreach) before IT-T1 layers `foreach` on top. The empty `Dispose` is the
// scope-cleanup hook IT-T1 will call on every loop-exit edge.
struct ListEnumerator<T> {
	T* mItems;
	int mCount;
	int mIndex;       // -1 before the first MoveNext

	// Advance the cursor; true while it still points at a valid element. Mutates
	// `mIndex` through `this` — the state mutation that must persist across calls
	// (a reloaded copy would lose the increment and loop forever; R6/R7).
	public bool MoveNext() {
		this.mIndex = this.mIndex + 1;
		return this.mIndex < this.mCount;
	}
	// The element the cursor currently points at. A read-only property; its
	// getter lowers to the `get_Current` symbol IT-T1 resolves by name.
	public T Current { get { return this.mItems[this.mIndex]; } }
	// Scope-cleanup hook (no-op for this value enumerator — nothing to free).
	public void Dispose() { }
}

// Higher-order functions over List<T> — the payoff of lambdas/closures. These
// are free static generics, called bare (`Map<int,int>(xs, f)`) with the
// receiver as an explicit `self` parameter. The idiomatic instance form
// (`xs.Map<R>(f)`) now lives on `List<T>` above (generic-methods B2); these
// static carriers are retained for the bare-cross-class call shape that
// `list_hof.bf`/`closure_arg.bf` exercise. The `function R(T) f` parameter is
// callable inside the body (the higher-order payoff), so each method just walks
// the list applying `f`. Non-capturing lambdas, capturing closures, and method
// references all work as the function argument (uniform function values).
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
