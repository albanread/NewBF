// MS-T5 NEGATIVE fixture: a single, balanced `delete` of an owned `new Node()`
// — the correct manual-memory pattern. The delete-flow pass must emit NO
// diagnostic.
class Node {
    public int32 value;
}

class Program {
    public static int32 Main() {
        let p = new Node();
        delete p;
        return 0;
    }
}
