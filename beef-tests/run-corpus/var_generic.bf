// expect: 42
// The Box<int> instantiation appears only in expression position (a `var`
// local has no type annotation), so it's collected from the `new` expression,
// not a typed declaration. Exercises expr-position monomorphization.
class Box<T> {
	T mValue;
	public this(T v) { this.mValue = v; }
	public T Get() { return this.mValue; }
}
class Program {
	public static int32 Main() {
		var a = new Box<int>(42);
		int32 r = a.Get();
		delete a;
		return r;
	}
}
