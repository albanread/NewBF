// expect: 3
// Class virtual slots and interface slots COEXIST without clobbering
// (itables.md §8/6, slot-base math). `Widget` has its own `virtual V()` (a class
// vtable slot, index 0) AND implements `IShape.Area()` (an appended interface
// slot, base >= the class block). We call V() through the class type (1) and
// Area() through the interface (2); 1 + 2 == 3 proves the interface block sits
// strictly AFTER the class virtual slot and neither overwrites the other.
interface IShape {
	int32 Area();
}
class Widget : IShape {
	public virtual int32 V() { return 1; }
	public int32 Area() { return 2; }
}
class Program {
	public static int32 Main() {
		Widget w = new Widget();
		IShape s = w;
		int32 r = w.V() + s.Area();   // 1 (class vslot) + 2 (iface slot) == 3
		delete w;
		return r;
	}
}
