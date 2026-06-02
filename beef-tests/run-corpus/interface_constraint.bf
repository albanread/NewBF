// expect: 100
// Interface as a generic constraint — the dominant Beef interface pattern (the
// corpus survey found ~300 of these vs. ~21 interface-typed dynamic dispatches).
// `Use<T>(T) where T : IFace` calls `val.Get()`; monomorphizing T = Holder makes
// the call resolve statically to Holder.Get. (Dynamic dispatch through an
// interface-typed value is a separate, rarer feature — itables, deferred.)
interface IFace {
	int32 Get();
}
class Holder : IFace {
	public int32 Get() { return 100; }
}
class Program {
	public static int32 Use<T>(T val) where T : IFace { return val.Get(); }
	public static int32 Main() {
		Holder h = new Holder();
		int32 r = Use<Holder>(h);
		delete h;
		return r;
	}
}
