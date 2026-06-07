// expect: 2
// RF-T7: a [Reflect(.Methods)] class exposes its declared methods by index. The
// MethodInfo array is queryable, so GetMethodCount() returns 2 (the two declared
// methods Area/Width — only user-declared methods are reflected, no inherited
// Object methods or the ctor). GetMethod(0).GetName() is readable and, since the
// recorded methods are sorted by (name, symbol), names the first method "Area";
// Internal.StrEq (a char8*-vs-char8* compare) matches it. Both halves are pinned:
// the count must be 2 AND the name must match — a broken/garbage Type satisfies
// neither, so this is a differential, not a bare count. Symmetric with RF-T6's
// reflect_field_count_marked / reflect_field_name (fields → methods).
[Reflect(.Methods)] class Widget {
	public int32 Area()  { return 1; }
	public int32 Width() { return 2; }
}
class Program {
	public static int32 Main() {
		int32 count = typeof(Widget).GetMethodCount();
		MethodInfo m0 = typeof(Widget).GetMethod(0);
		bool nameOk = Internal.StrEq(m0.GetName(), "Area");
		return (count == 2 && nameOk) ? count : 0;
	}
}
