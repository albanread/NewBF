// CT-T3 positive: a generic method `Use<T> where T : struct` is instantiated
// with an in-program reference **class** (`RefClass`), which is provably not a
// value type → EXACTLY ONE constraint diagnostic.
//
// Kept OUT of the auto-collected beef-tests corpus (it is *expected* to
// diagnose), exactly like tests/ownership/*.bf.
namespace ConstraintTests
{
	class RefClass
	{
		public int mX = 0;
	}

	class Holder
	{
		public static void Use<T>(T val) where T : struct
		{
		}

		public static void Caller()
		{
			RefClass r = scope RefClass();
			// `RefClass` is a reference class — provably not a value struct →
			// violates `T : struct`.
			Use<RefClass>(r);
		}
	}
}
