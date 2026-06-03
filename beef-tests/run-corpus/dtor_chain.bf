// expect: 321
// Destructor chaining: `delete derived` runs destructors derived-first then down
// to the root (reverse of construction). Each dtor does Log = Log*10 + tag, so
// Leaf(3) then Mid(2) then Base(1) builds 3 → 32 → 321. (Inheritance shares a
// base dtor into a derived that declares none; the chain dedups so each runs once.)
class Tracker { public static int32 Log; }
class Base { public ~this() { Tracker.Log = Tracker.Log * 10 + 1; } }
class Mid : Base { public ~this() { Tracker.Log = Tracker.Log * 10 + 2; } }
class Leaf : Mid { public ~this() { Tracker.Log = Tracker.Log * 10 + 3; } }
class Program {
	public static int32 Main() {
		Tracker.Log = 0;
		Leaf t = new Leaf();
		delete t;            // Leaf(3), Mid(2), Base(1) → 321
		return Tracker.Log;
	}
}
