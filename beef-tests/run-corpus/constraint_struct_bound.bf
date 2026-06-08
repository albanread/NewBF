// expect: 9
// GC-T0 — struct-bound generic constraint, pinned as a non-regression. The `where
// T : struct` clause is NOT enforced today; dispatch works because monomorphizing
// T = Counter (a value struct) makes `val.Value()` resolve statically to Counter's
// concrete method table. This pins that the satisfied `struct`-kind constraint
// accepts a value type and the method call resolves on the concrete struct (the
// "constraint static path"). A later GC task adds the violation diagnostic; this
// program is its green baseline.
struct Counter {
	public int32 n;
	public int32 Value() { return this.n; }
}
class Program {
	public static int32 Use<T>(T val) where T : struct { return val.Value(); }
	public static int32 Main() {
		Counter c = ?;
		c.n = 9;
		return Use<Counter>(c);   // T = Counter: val.Value() -> Counter.Value -> 9
	}
}
