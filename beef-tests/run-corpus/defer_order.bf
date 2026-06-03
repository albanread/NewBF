// expect: 321
// `defer` runs at the enclosing block's exit in reverse (LIFO) order. The inner
// block queues three defers; on block exit they fire 3,2,1, building 321 into
// `acc`, which the outer scope then returns. Tests block-scoped LIFO ordering.
class Program {
	public static int32 Main() {
		int32 acc = 0;
		{
			defer { acc = acc * 10 + 1; }
			defer { acc = acc * 10 + 2; }
			defer acc = acc * 10 + 3;   // non-block defer form
		}                                // exit → 3, 2, 1 → 321
		return acc;
	}
}
