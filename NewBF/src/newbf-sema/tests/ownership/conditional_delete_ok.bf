// MS-T5 NEGATIVE fixture: a `delete` on only one branch followed by an
// unconditional `delete`. Because the binding is `Deleted` on only SOME paths,
// the conservative join drops it to untracked before the second `delete`, so
// NO double-free is claimed (it is not provable on every path).
class Node {
    public int32 value;
}

class Program {
    public static int32 Main(int32 c) {
        let p = new Node();
        if (c > 0) {
            delete p;
        }
        delete p;   // not provably a double-free (the `if` may not have run)
        return 0;
    }
}
