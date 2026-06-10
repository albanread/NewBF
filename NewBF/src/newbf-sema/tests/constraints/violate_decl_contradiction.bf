// CT-T2 positive: a generic parameter constrained as BOTH `class` and `struct`
// across one declaration's where-clauses is an unsatisfiable contradiction — no
// type is both a reference class and a value struct. Expected: exactly ONE
// constraint diagnostic.
//
// Kept OUT of the auto-collected beef-tests corpus (it is *expected* to
// diagnose), exactly like tests/ownership/*.bf.
namespace ConstraintTests
{
	class Holder
	{
		// `class` on one clause, `struct` on another — same parameter `T`.
		public static void Both<T>(T v) where T : class where T : struct
		{
		}
	}
}
