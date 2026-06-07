// expect: 30
// Field-receiver branch of instance generic-method resolution (GM-A3a/A3b): the
// receiver is a `this`-field (`this.mBox`), not a local/`this`/`new`. The
// collector resolves the field's owner from its declared type (`Box`), and the
// call site reaches the same body via `struct_base(this.mBox)`. Holder.Use()
// calls this.mBox.Get<int32>(30), so Main returns 30.
class Box {
	public T Get<T>(T x) { return x; }
}
class Holder {
	Box mBox;
	public this() { mBox = new Box(); }
	public int32 Use() { return this.mBox.Get<int32>(30); }
}
class Program {
	public static int32 Main() {
		Holder h = new Holder();
		int32 r = h.Use();   // 30
		delete h;            // MS-T5.5: balance the `new Holder()` (behavior-neutral)
		return r;
	}
}
