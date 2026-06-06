// Verify-corpus pin for IT-T1..T7: dynamic interface dispatch through an
// interface-TYPED value (not the generic-constraint path). An interface type
// lowers to a `Ref(iface_id)` and `s.Area()` dispatches through the receiver's
// vtable at a global per-interface slot base; default methods, multi-interface,
// and `is`/`as` against an interface all lower to verifiable LLVM. This file
// must lower to a verifiable module + a contradiction-free def graph (both
// corpora are 100% ratchets).
namespace Tests
{
	interface IShape
	{
		int32 Area();
		// A default interface method whose body calls a sibling abstract method
		// through `this` (interface-dispatched).
		int32 Doubled()
		{
			return Area() * 2;
		}
	}

	interface ITagged : IShape
	{
		int32 Tag();
	}

	class Square : IShape
	{
		int32 mSide;

		public this(int32 side)
		{
			this.mSide = side;
		}

		public int32 Area()
		{
			return this.mSide * this.mSide;
		}
	}

	class Tile : ITagged
	{
		public int32 Area()
		{
			return 4;
		}

		public int32 Tag()
		{
			return 7;
		}
	}

	class Use
	{
		// Dispatch through an interface-typed parameter.
		public static int32 AreaOf(IShape s)
		{
			return s.Area();
		}

		// Default-method dispatch + a sibling-call-through-this.
		public static int32 DoubledOf(IShape s)
		{
			return s.Doubled();
		}

		// `is`/`as` against an interface, incl. the transitive base (ITagged : IShape).
		public static int32 Classify(IShape s)
		{
			if (s is ITagged)
			{
				return 1;
			}
			return 0;
		}

		public static int32 Run()
		{
			Square sq = new Square(3);
			IShape s = sq;            // upcast (free)
			int32 a = Use.AreaOf(s);  // 9
			int32 d = Use.DoubledOf(s); // 18 (default calling sibling Area)

			Tile t = new Tile();
			ITagged it = t;
			int32 ta = Use.AreaOf(it); // 4 (IShape view of an ITagged)
			int32 c = Use.Classify(it); // 1 (Tile is ITagged)

			delete sq;
			delete t;
			return a + d + ta + c;    // 9 + 18 + 4 + 1 = 32
		}
	}
}
