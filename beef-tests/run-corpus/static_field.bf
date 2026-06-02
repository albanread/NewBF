// expect: 30
// A class `static` field is a mutable module global, addressed as Type.Field:
// default 0, assigned, compound-assigned, read — no instance needed.
class Counter {
	public static int32 Total;
}
class Program {
	public static int32 Main() {
		Counter.Total = 10;     // store to the global
		Counter.Total += 20;    // load + add + store (compound on a static)
		return Counter.Total;   // load -> 30
	}
}
