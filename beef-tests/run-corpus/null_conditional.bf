// expect: 47
// Null-conditional `a?.field`: evaluates the base once and null-guards the load,
// yielding the field's default (null/0) when the base is null. Exactly correct
// for reference chains (`a?.next?.val`), which short-circuit to null/0. Here a
// non-null node reads its value and a null node's `?.` yields 0.
class Node { public int32 val; public Node next; }
class Program {
	public static int32 Main() {
		Node a = new Node();
		a.val = 42;
		a.next = null;
		int32 here = a?.val;          // 42 (a non-null)
		Node gone = null;
		int32 missing = gone?.val;    // 0 (gone null → default)
		Node n2 = a?.next;            // null (a.next is null)
		int32 chained = a?.next?.val; // 0 (short-circuits at the null link)
		int32 alive = (n2 == null) ? 5 : 99;  // 5
		delete a;
		return here + missing + chained + alive;  // 42 + 0 + 0 + 5 = 47
	}
}
