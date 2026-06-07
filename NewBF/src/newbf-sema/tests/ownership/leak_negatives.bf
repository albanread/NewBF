// MS-T6 NEGATIVE fixture: every form of a `new` whose ownership is resolved must
// stay SILENT (no leak diagnostic). Each method exercises one disposition:
//   * Deleted        — `delete p` balances the `new`
//   * Scoped         — `scope T()` is auto-freed at scope exit (never a leak)
//   * Moved (return) — `return p` hands ownership to the caller
//   * Moved (alias)  — `q = p` aliases ownership to another binding
//   * Dropped (field)— `this.f = p` / `obj.f = p` stores ownership into a field
//   * Dropped (addr) — `&p` / `ref p` / `out p` takes the address
//   * Dropped (lambda)— a capturing lambda escapes ownership un-followably
// The pass must emit ZERO leak diagnostics across this whole file.
class Node {
    public int32 value;
    public Node next;
}

class Sink {
    public Node held;
    public void Keep(Node n) { this.held = n; }
}

class Program {
    static void Use(Node n) { }
    static void Take(ref Node n) { }

    // Deleted on the path to exit → balanced → silent.
    static int32 Deleted() {
        let p = new Node();
        delete p;
        return 0;
    }

    // A `scope` binding is auto-freed → never a leak → silent.
    static int32 Scoped() {
        Node p = scope Node();
        return p.value;
    }

    // Returned → ownership moves to the caller → silent.
    static Node MovedByReturn() {
        let p = new Node();
        return p;
    }

    // Aliased to another tracked binding → moved → silent.
    static int32 MovedByAlias() {
        let p = new Node();
        let q = p;       // p moves into q
        delete q;        // q (now the owner) is freed
        return 0;
    }

    // Stored into a field → ownership escapes to the field → Dropped → silent.
    static int32 DroppedByFieldStore() {
        let p = new Node();
        let s = new Sink();
        s.held = p;      // field-store: p escapes into s.held
        delete s;
        return 0;
    }

    // Stored into a field via a method call (Beef by-ref) then the holder owns it.
    static int32 DroppedByMethodKeep() {
        let p = new Node();
        let s = new Sink();
        s.Keep(p);       // p stays Owned through the call, but s now references it
        delete s;        // s frees it (we model `s.Keep(p)` as keeping p Owned —
        delete p;        // so an explicit delete still balances it: no leak, no FP)
        return 0;
    }

    // Address-taken (`ref p`) → escapes un-followably → Dropped → silent.
    static int32 DroppedByRef() {
        Node p = new Node();
        Take(ref p);
        return 0;
    }

    // Captured by a closure → escapes un-followably → Dropped → silent.
    static int32 DroppedByCapture() {
        let p = new Node();
        function int32() f = () => p.value;
        return f();
    }

    // Passed as an argument (by reference) keeps `Owned`, then deleted → balanced.
    static int32 ArgPassThenDeleted() {
        let p = new Node();
        Use(p);          // by-ref borrow — p stays Owned
        delete p;        // balanced → silent
        return 0;
    }
}
