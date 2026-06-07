// expect: 42
// MX-T3 — the this-field caller-binding case (mixins.md §3.4): an instance mixin
// body reads/writes a caller field via `this`. The splice reuses the live
// Lowerer's `this_slot`, so `this.value` in the body resolves to the caller
// instance's field. `this` is an explicit reference (not a bare free name), so the
// static-`this` guard passes (the caller is an instance method). The mixin adds 2
// to `this.value` (40 → 42).
class Box {
	public int32 value = 40;
	mixin AddTwo() {
		this.value += 2;
	}
	public int32 Run() {
		AddTwo!();
		return this.value;   // 42
	}
}
class Program {
	public static int32 Main() {
		Box b = new Box();
		int32 r = b.Run();
		delete b;
		return r;   // 42
	}
}
