// CT-T3 positive: a generic method `Use<T> where T : class` is instantiated
// with an in-program value **struct** (`ValStruct`), which is provably not a
// reference type → EXACTLY ONE constraint diagnostic.
//
// Kept OUT of the auto-collected beef-tests corpus (it is *expected* to
// diagnose), exactly like tests/ownership/*.bf.
namespace ConstraintTests
{
	struct ValStruct
	{
		public int mX = 0;
	}

	class Holder
	{
		public static void Use<T>(T val) where T : class
		{
		}

		public static void Caller()
		{
			ValStruct v = .();
			// `ValStruct` is a value struct — provably not a reference class →
			// violates `T : class`.
			Use<ValStruct>(v);
		}
	}
}
