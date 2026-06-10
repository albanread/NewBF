// IT-T2 parser fixture: `yield return e;` / `yield break;` statements. Before
// IT-T2 `yield` was a reserved keyword with NO `stmt()` arm, so `yield x` mis-
// parsed as a bogus expression statement; this fixture pins that both forms now
// parse cleanly (the parser-corpus gate requires 0 diagnostics on every file).
namespace YieldFixture
{
	class Generators
	{
		// Straight-line `yield return`.
		public List<int32> Counting()
		{
			yield return 1;
			yield return 2;
			yield return 3;
		}

		// `yield break` mid-way, inside control flow (a `for` loop + an `if`).
		public List<int32> Upto(int32 n)
		{
			for (int32 i = 0; i < n; i++)
			{
				if (i > 4)
					yield break;
				yield return i * i;
			}
		}

		// An immediate `yield break;` — the empty sequence.
		public List<int32> Nothing()
		{
			yield break;
		}

		// `yield return` of a richer expression (a call, a parenthesized sum).
		public List<int32> Mixed(int32 a, int32 b)
		{
			yield return (a + b);
			yield return Square(a);
			if (b > a)
				yield return b - a;
			else
				yield break;
		}

		public int32 Square(int32 x)
		{
			return x * x;
		}
	}
}
