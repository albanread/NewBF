// expect: 73
// Object initializer on a class: `new T(args) { field = value }` runs the ctor
// then stores the listed fields through the new reference. Here the ctor sets a
// base value and the initializer overrides/sets two fields on top.
class Widget {
	public int32 w;
	public int32 h;
	public int32 tag;
	public this() { this.tag = 7; }
}
class Program {
	public static int32 Main() {
		Widget x = new Widget() { w = 40, h = 26 };
		int32 r = x.w + x.h + x.tag;   // 40 + 26 + 7 = 73
		delete x;
		return r;
	}
}
