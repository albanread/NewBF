// expect: 1
// RF-T4: typeof(Dog).GetName() resolves to the type's simple name string, and
// Internal.StrEq (a char8*-vs-char8* compare) matches it against "Dog".
[Reflect] class Dog { public int32 mAge; }
class Program {
	public static int32 Main() {
		return Internal.StrEq(typeof(Dog).GetName(), "Dog") ? 1 : 0;
	}
}
