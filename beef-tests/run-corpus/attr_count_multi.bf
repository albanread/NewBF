// expect: 2
// CA-T4: two user-class attributes on one type, order preserved. The built-in
// [Reflect] marker (which gates surfacing) is skipped by sema, not counted, so
// [Reflect, A, B] surfaces exactly 2 attributes.
class A : Attribute { }
class B : Attribute { }
[Reflect, A, B] class C { public int32 mX; }
class Program {
	public static int32 Main() {
		return typeof(C).GetCustomAttributeCount();
	}
}
