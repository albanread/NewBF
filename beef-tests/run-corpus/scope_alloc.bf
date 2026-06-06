// expect: 909
// `scope T()` allocates with the enclosing block's lifetime: the ctor and field
// defaults run (like `new`), and the instance is auto-freed (dtor + free) at
// scope exit — no manual `delete`. A nested block allocates two Tracked objects
// (each ctor +1, dtor -1 on sLive); after the block both are freed, so sLive is
// back to 0. r = 9*100 + (live inside was 2)*... we encode: inside-live*... no:
// simply prove dtors ran — live==0 after — and that field default v=9 applied.
class Tracked {
	public static int32 sLive = 0;
	public int32 v = 9;
	public this() { Tracked.sLive += 1; }
	public ~this() { Tracked.sLive -= 1; }
}
class Program {
	public static int32 Main() {
		int32 insideV = 0;
		{
			Tracked a = scope Tracked();
			Tracked b = scope Tracked();
			insideV = a.v;              // 9 (field default applied)
			// here Tracked.sLive == 2
		}
		// after scope: a and b auto-freed → sLive == 0
		return insideV * 100 + Tracked.sLive + 9;   // 900 + 0 + 9 = 909
	}
}
