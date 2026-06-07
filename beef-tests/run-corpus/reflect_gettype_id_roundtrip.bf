// expect: 1
// RF-T5: recv.GetType() reads the object's $header (ClassVData.mType, the
// runtime type-id) → __newbf_type_by_id → Type*. For a concrete `new Dog()`
// the runtime id MUST equal the static typeof(Dog) id (the header and the
// registry agree). A pure roundtrip: only a correct dynamic lookup matches.
[Reflect] class Dog { public int32 mAge; }
class Program {
	public static int32 Main() {
		Dog p = new Dog();
		return (p.GetType().GetTypeId() == typeof(Dog).GetTypeId()) ? 1 : 0;
	}
}
