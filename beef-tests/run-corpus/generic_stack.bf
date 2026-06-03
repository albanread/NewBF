// expect: 150
// A user generic `Stack<T>` backed by a corlib `List<T>` — exercises nested
// generic monomorphization (`Stack<int32>` must drag in `List<int32>`).
//   push 10, 20, 30 ; pop → 30 ; pop → 20 ; count now 1
//   r = a + b + c*100 = 30 + 20 + 1*100 = 150
class Stack<T> {
	List<T> items;
	public this() { this.items = new List<T>(); }
	public ~this() { delete this.items; }

	public void Push(T v) { this.items.Add(v); }
	public T Pop() {
		int32 last = this.items.Count() - 1;
		T v = this.items[last];
		this.items.RemoveAt(last);
		return v;
	}
	public int Count() { return this.items.Count(); }
}
class Program {
	public static int32 Main() {
		Stack<int32> s = new Stack<int32>();
		s.Push(10);
		s.Push(20);
		s.Push(30);

		int32 a = s.Pop();   // 30
		int32 b = s.Pop();   // 20
		int32 c = s.Count(); // 1

		delete s;
		return a + b + c * 100; // 30 + 20 + 100 = 150
	}
}
