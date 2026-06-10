// expect: 1
// CA-T4 differential strip (like reflect_strip_vs_marked): a [Reflect]-marked
// type surfaces its attributes (count 1); an unmarked type strips them
// (count 0). Both halves are pinned in one program — 0 alone could be a broken
// Type, so this differential proves the FIELDS gate (which v1 piggybacks for
// attributes) is observable, not that attributes are simply absent.
class MyAttr : Attribute { }
[Reflect, MyAttr] class Marked   { public int32 mX; }
          [MyAttr] class Unmarked { public int32 mX; }   // no [Reflect] ⇒ attrs stripped
class Program {
	public static int32 Main() {
		return (typeof(Marked).GetCustomAttributeCount() == 1
		     && typeof(Unmarked).GetCustomAttributeCount() == 0) ? 1 : 0;
	}
}
