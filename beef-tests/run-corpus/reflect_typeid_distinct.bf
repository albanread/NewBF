// expect: 1
// RF-T4 canonical first-slice green: typeof(T) returns a real Type with a
// distinct, stable id per type. A pure differential — a null/garbage Type
// can't satisfy BOTH halves (same type ⇒ same id, different types ⇒ distinct).
[Reflect] class Dog { public int32 mAge; }
[Reflect] class Cat { public int32 mLives; }
class Program {
	public static int32 Main() {
		Type d = typeof(Dog);
		Type d2 = typeof(Dog);
		Type c = typeof(Cat);
		return (d.GetTypeId() == d2.GetTypeId() && d.GetTypeId() != c.GetTypeId()) ? 1 : 0;
	}
}
