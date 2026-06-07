// MS-T5 positive fixture: a provable double-`delete` of a user-written
// `new Node()` local with no intervening reassignment. The delete-flow pass
// must emit exactly ONE double-free diagnostic (on the second `delete p`).
class Node {
    public int32 value;
}

class Program {
    public static int32 Main() {
        let p = new Node();
        delete p;
        delete p;   // ← provable double-free
        return 0;
    }
}
