// expect: 375
// Static calls into corlib `Math` (newbf-corlib/bf/Math.bf): the int32
// overloads of Min/Max/Clamp/Sign. Hand arithmetic:
//   a = Min(7, 3)        = 3
//   b = Max(7, 3)        = 7
//   c = Clamp(10, 0, 5)  = Min(Max(10,0),5) = Min(10,5) = 5
//   s = Sign(-42)        = -1
//   a*100 + b*10 + c + (s+1) = 300 + 70 + 5 + 0 = 375
class Program {
	public static int32 Main() {
		int32 a = Math.Min(7, 3);
		int32 b = Math.Max(7, 3);
		int32 c = Math.Clamp(10, 0, 5);
		int32 s = Math.Sign(-42);
		return a * 100 + b * 10 + c + (s + 1);
	}
}
