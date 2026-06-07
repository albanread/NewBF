// MS-T5 positive fixture: an explicit `delete` of a `scope`-bound binding. The
// scope-lifetime cleanup ALSO frees `x`, so the explicit `delete x` is a
// guaranteed double free. The pass must emit exactly ONE diagnostic.
class Node {
    public int32 value;
}

class Program {
    public static int32 Main() {
        Node x = scope Node();
        delete x;   // ← scope cleanup will free it too → double-free
        return 0;
    }
}
