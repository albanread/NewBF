// expect: 1
// RF-T4 differential strip test: a [Reflect(.Fields)] class emits its FieldInfo
// array (field count 2); an unmarked class strips it (count 0). Both halves are
// pinned in one program — 0 alone could be a broken Type, so this differential
// proves policy gating is observable, not that reflection is simply absent.
[Reflect(.Fields)] class Marked   { public int32 mX; public int32 mY; }
                   class Unmarked { public int32 mX; public int32 mY; }
class Program {
	public static int32 Main() {
		return (typeof(Marked).GetFieldCount() == 2 && typeof(Unmarked).GetFieldCount() == 0) ? 1 : 0;
	}
}
