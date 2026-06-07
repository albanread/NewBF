// expect: 1
// RF-T5: GetType() is DYNAMIC — it reads the ACTUAL object's $header, not the
// static receiver type. A base-typed reference (`Animal a`) pointing at a
// DERIVED object (`new Dog()`) reports the DERIVED type's id, NOT the base's.
// This is the proof it isn't just static typeof: typeof(Animal) would differ.
[Reflect] class Animal { public int32 mLegs; }
[Reflect] class Dog : Animal { public int32 mAge; }
class Program {
	public static int32 Main() {
		Animal a = new Dog();
		// a.GetType() must be Dog (the runtime object), not Animal (the static ref).
		return (a.GetType().GetTypeId() == typeof(Dog).GetTypeId()
		     && a.GetType().GetTypeId() != typeof(Animal).GetTypeId()) ? 1 : 0;
	}
}
