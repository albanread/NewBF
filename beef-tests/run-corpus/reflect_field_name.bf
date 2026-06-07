// expect: 1
// RF-T6: a [Reflect(.Fields)] type's first field is queryable by index, and its
// FieldInfo carries the real field name. typeof(Point).GetField(0).GetName()
// resolves to the char8* name of the first declared field ("mX"); Internal.StrEq
// (a char8*-vs-char8* compare) matches it. Proves GetField + FieldInfo.GetName
// over the emitted, policy-gated FieldInfo array.
[Reflect(.Fields)] class Point { public int32 mX; public int32 mY; }
class Program {
	public static int32 Main() {
		FieldInfo f = typeof(Point).GetField(0);
		return Internal.StrEq(f.GetName(), "mX") ? 1 : 0;
	}
}
