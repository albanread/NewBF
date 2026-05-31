// expect: 42
// Generic *class*, monomorphized per concrete type: Box<int> (i64 payload) and
// Box<int32> (i32 payload) are distinct heap types from one template, each with
// its own ctor + method lowered with T substituted. Proves new/methods/this
// across monomorphs.
//   a = Box<int>(40); b = Box<int32>(2)  =>  a.Get() + b.Get() = 42
class Box<T> {
	T mValue;
	public this(T v) { this.mValue = v; }
	public T Get() { return this.mValue; }
}
class Program {
	public static int32 Main() {
		Box<int> a = new Box<int>(40);
		Box<int32> b = new Box<int32>(2);
		int32 r = a.Get() + b.Get();
		delete a;
		delete b;
		return r;
	}
}
