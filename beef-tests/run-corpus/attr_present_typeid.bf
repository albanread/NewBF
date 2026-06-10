// expect: 1
// CA-T4 canonical first-slice green: the attribute on C is MyAttr; its recorded
// dense attr-type-id MUST equal typeof(MyAttr)'s id (the round-trip), and the
// count is 1. A null/garbage AttributeInfo can't satisfy this differential.
// MyAttr is a CLASS (v1 attribute = class), no [Reflect] needed (every class is
// reflectable regardless of policy); only the annotated type C is [Reflect],
// where the FIELDS gate decides whether attributes surface.
//
// NOTE: the AttributeInfo is bound to a local (`let a = …`) before `GetTypeId()`
// — calling a method directly on a struct rvalue returned by another call
// (`t.GetCustomAttribute(0).GetTypeId()`) is a pre-existing lowering gap
// unrelated to CA-T4 (the by-value-struct receiver collapses to undef). Binding
// to a local is the idiomatic form and exercises the same emitted table.
class MyAttr : Attribute { }
[Reflect, MyAttr] class C { public int32 mX; }
class Program {
	public static int32 Main() {
		let t = typeof(C);
		let a = t.GetCustomAttribute(0);
		return (t.GetCustomAttributeCount() == 1
		     && a.GetTypeId() == typeof(MyAttr).GetTypeId()) ? 1 : 0;
	}
}
