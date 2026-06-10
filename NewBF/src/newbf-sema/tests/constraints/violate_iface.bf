// CT-T3 positive (the flagship `Use<int32>` check): a generic method
// `Use<T> where T : IFace` is instantiated with the **primitive** `int32`. A
// primitive is a known value type that implements no in-program interface, so
// `int32` provably does not satisfy `T : IFace` → EXACTLY ONE constraint
// diagnostic.
//
// Kept OUT of the auto-collected beef-tests corpus (it is *expected* to
// diagnose), exactly like tests/ownership/*.bf — asserted via a direct-`analyze`
// `constraint_diags` helper, never run-corpus.
namespace ConstraintTests
{
	interface IFace
	{
		int Get();
	}

	class Holder
	{
		public static int Use<T>(T val) where T : IFace
		{
			return 0;
		}

		public static void Caller()
		{
			// `int32` is a primitive — provably implements no in-program
			// interface → violates `T : IFace`.
			Use<int32>(0);
		}
	}
}
