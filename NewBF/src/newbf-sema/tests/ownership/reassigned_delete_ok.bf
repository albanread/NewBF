// MS-T5 NEGATIVE fixture: `delete p; p = new Node(); delete p;`. The
// reassignment between the two deletes RESETS the lattice (the second `delete`
// frees a fresh allocation), so this is NOT a double free. The pass must emit
// NO diagnostic.
class Node {
    public int32 value;
}

class Program {
    public static int32 Main() {
        let p = new Node();
        delete p;
        p = new Node();
        delete p;
        return 0;
    }
}
