// MS-T6 positive fixture: a provable leak. `p` is a user-written `new Node()`
// local that is read (`p.value`) but never deleted, moved (returned/aliased), or
// dropped (field-stored/captured/address-taken) on the only path to the body
// exit — so it is still `Owned` at the fall-through end. The delete-flow pass
// must emit exactly ONE leak diagnostic (pointing at the `new Node()` site).
class Node {
    public int32 value;
}

class Program {
    public static int32 Main() {
        let p = new Node();   // ← provable leak: never freed
        return p.value;
    }
}
