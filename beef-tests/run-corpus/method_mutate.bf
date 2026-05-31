// expect: 15
// A void method that mutates a field through `this`, called several times;
// a no-arg constructor `this()`.
class Acc {
	public int32 sum;
	public this() { this.sum = 0; }
	public void Add(int32 x) { this.sum = this.sum + x; }
	public int32 Total() { return this.sum; }
}
class Program {
	public static int32 Main() {
		Acc a = new Acc();
		a.Add(4);
		a.Add(5);
		a.Add(6);
		int32 r = a.Total();
		delete a;
		return r;
	}
}
