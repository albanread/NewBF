// expect: 1
// CA-T5: a const-folded string ctor arg round-trips through the attribute
// metadata. `Named("hi")` folds the literal → `Const::Str("hi")` (CA-T1's
// `attr_arg_const` Str arm), lands in the uniform `[n x i64]` arg array as a
// `ptrtoint`/`const_to_int` of the `.rodata` cstr (CA-T4's emission, §2.4 — the
// i64 slot holds the char8* address), and is read back by
// `AttributeInfo.GetStrArg(0)` (CA-T2's accessor, which `inttoptr`s the slot to
// char8*). The result is compared against the "hi" literal via `Internal.StrEq`
// (two char8*, NOT a String buffer — CR-T2's note; String.Equals compares String
// OBJECTS, not raw buffers).
//
// Named is a CLASS (v1 attribute = class, has a dense type-id), no [Reflect]
// needed; only the annotated type Widget is [Reflect], where the FIELDS gate
// decides whether attributes surface.
//
// NOTE: the AttributeInfo is bound to a local (`AttributeInfo a = …`) before
// `GetStrArg(0)` — the same R5 by-value-struct-rvalue-receiver workaround as
// attr_present_typeid.bf / attr_int_arg.bf.
class Named : Attribute { public this(char8* n) { } }
[Reflect, Named("hi")] class Widget { public int32 mX; }
class Program {
	public static int32 Main() {
		AttributeInfo a = typeof(Widget).GetCustomAttribute(0);
		return Internal.StrEq(a.GetStrArg(0), "hi") ? 1 : 0;
	}
}
