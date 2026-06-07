// Verify-corpus pin for GM-A3a/A3b: concrete-owner *instance* generic methods.
// `obj.M<T>(args)` on a class receiver lowers to a real instance call (a leading
// `this`, owner-mangled symbol). This file must lower to a verifiable LLVM
// module (the verify corpus is a 100% ratchet) and build a contradiction-free
// definition graph (the sema corpus, same ratchet).
namespace Tests
{
	class Box
	{
		public int32 mV;

		public this()
		{
			this.mV = 5;
		}

		// A concrete-owner instance generic method dispatched with a `this`.
		public T Id<T>(T x)
		{
			return x;
		}

		// A bare same-class instance generic call (owner = enclosing type,
		// prepend the current `this`) plus a `this`-field read in the body.
		public int32 Run()
		{
			return Id<int32>(this.mV);
		}
	}

	class GenericMethodsInstance
	{
		// Local-receiver, field-receiver, and `new`-receiver instance calls.
		public static int32 Use()
		{
			Box b = new Box();
			int32 a = b.Id<int32>(40);
			int32 c = new Box().Id<int32>(2);
			int32 r = a + c + b.Run();
			delete b;   // MS-T5.5: balance the `new Box()` (behavior-neutral)
			return r;
		}
	}
}
