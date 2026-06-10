// CT-T2/CT-T3 negative (the zero-false-positive pin): every supported
// constraint, at BOTH the declaration level and the instantiation level, is
// SATISFIED → ZERO constraint diagnostics.
//
// CT-T2: `T : class` alone, `U : struct` alone, and a distinct parameter
// carrying `class` while another carries `struct` are all satisfiable — only
// `class` AND `struct` on the SAME parameter contradicts.
//
// CT-T3: each satisfied instantiation must NOT diagnose:
//   * `UseIFace<Impl>`  — `Impl` transitively implements `IFace` (via `IMid`).
//   * `UseIFace<Holder>`— `Holder : IFace` directly.
//   * `UseClass<Holder>`— `Holder` is a reference class (`T : class`).
//   * `UseStruct<Val>`  — `Val` is a value struct (`T : struct`).
//   * `UseStruct<int32>`— a primitive is a value type (`T : struct`).
//   * `UseBase<Derived>`— `Derived` transitively derives from `Base`.
namespace ConstraintTests
{
	interface IFace
	{
		int Get();
	}

	interface IMid : IFace
	{
	}

	class Base
	{
	}

	class Derived : Base
	{
	}

	struct Val
	{
		public int mX = 0;
	}

	class Impl : IMid
	{
		public int Get() { return 1; }
	}

	class Holder : IFace
	{
		public int Get() { return 2; }

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

		public static void UseIFace<T>(T v) where T : IFace
		{
		}

		public static void UseClass<T>(T v) where T : class
		{
		}

		public static void UseStruct<T>(T v) where T : struct
		{
		}

		public static void UseBase<T>(T v) where T : Base
		{
		}

		public static void Caller()
		{
			Impl impl = scope Impl();
			Holder h = scope Holder();
			Val val = .();
			Derived d = scope Derived();

			// Transitive implements: Impl : IMid : IFace — satisfied.
			UseIFace<Impl>(impl);
			// Direct implements: Holder : IFace — satisfied.
			UseIFace<Holder>(h);
			// Holder is a reference class — `T : class` satisfied.
			UseClass<Holder>(h);
			// Val is a value struct — `T : struct` satisfied.
			UseStruct<Val>(val);
			// int32 is a primitive value type — `T : struct` satisfied.
			UseStruct<int32>(0);
			// Transitive base: Derived : Base — satisfied.
			UseBase<Derived>(d);
		}
	}
}
