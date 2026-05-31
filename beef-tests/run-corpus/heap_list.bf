// expect: 42
// A self-referential class: a reference-typed field (`next`), storing one
// object's reference into another, and chained access through it (a.next.val).
class Node { public int32 val; public Node next; }
class Program {
	public static int32 Main() {
		Node a = new Node();
		Node b = new Node();
		a.val = 10;
		b.val = 32;
		a.next = b;
		int32 r = a.val + a.next.val;
		delete b;
		delete a;
		return r;
	}
}
