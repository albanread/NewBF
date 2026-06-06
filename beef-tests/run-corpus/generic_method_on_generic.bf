// expect: 42
// GM-B1: an *instance* generic method whose OWNER is itself a generic type.
// `Box<T>` is a generic class; `Pick<R>` is an instance generic method on it.
// Instantiating `Box<int32>` and calling `bx.Pick<int32>(7)` must resolve to a
// monomorph mangled at the owner-mono prefix — `Box$i32.Pick$i32` — and be
// emitted with the COMBINED type-param env: the owner monomorph's bindings
// (`T -> int32`, from its MonoRecord) followed by the method's own (`R ->
// int32`, from the call's type-args). The body genuinely exercises BOTH: it
// reads the owner field `this.mValue` (type `T`) and the method parameter `x`
// (type `R`). `mValue = 35`, `Pick<int32>(7)` returns `x + this.mValue` = 42.
class Box<T> {
	T mValue;
	public this(T v) { this.mValue = v; }
	// Combined-env method: `R x` (method type-param) + `this.mValue` (owner T).
	public R Pick<R>(R x) { return x + this.mValue; }
}
class Program {
	public static int32 Main() {
		Box<int32> bx = new Box<int32>(35);
		int32 r = bx.Pick<int32>(7);   // 7 + 35
		delete bx;
		return r;                       // 42
	}
}
