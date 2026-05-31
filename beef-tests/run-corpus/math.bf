// expect: 16
// Static calls into the corlib `Math` type (defined in newbf-corlib/bf/Math.bf,
// not here): Abs(-7)=7 + Max(3,5)=5 + Min(10,4)=4 = 16.
class Program {
	public static int32 Main() {
		return Math.Abs(-7) + Math.Max(3, 5) + Math.Min(10, 4);
	}
}
