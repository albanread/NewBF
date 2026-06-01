// NewBF corlib — Handle<T>: a typed, generation-checked reference into a Pool.
//
// The type-safe form of the safe-reference idea (docs/GC.md §12): it wraps a
// raw Pool handle (gen<<16|idx) and resolves it back to a `T`, returning null
// once the slot is Freed. Built purely from generics + Pool — no GC, raw
// pointers preserved. (A generic *class*, so it needs no generic methods: the
// `T` flows from the monomorph into Get's return type.)
class Handle<T> {
	int mRaw;

	public this(int raw) { this.mRaw = raw; }

	// Resolve to the live T, or null if the underlying slot has been Freed.
	public T Get(Pool p) { return p.Get(this.mRaw); }

	// The packed raw handle, e.g. to Free it through the Pool.
	public int Raw() { return this.mRaw; }
}
