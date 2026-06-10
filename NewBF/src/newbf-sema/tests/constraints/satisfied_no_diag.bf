// CT-T2 negative: a declaration with satisfiable single kind constraints emits
// ZERO constraint diagnostics. `T : class` alone, `U : struct` alone, and a
// distinct parameter carrying `class` while another carries `struct` are all
// satisfiable — only `class` AND `struct` on the SAME parameter contradicts.
namespace ConstraintTests
{
	class Holder
	{
		public static void OnlyClass<T>(T v) where T : class
		{
		}

		public static void OnlyStruct<U>(U v) where U : struct
		{
		}

		// Two different parameters: `T : class`, `TI : struct` — no contradiction.
		public static void TwoParams<T, TI>(T a, TI b) where T : class where TI : struct
		{
		}
	}
}
