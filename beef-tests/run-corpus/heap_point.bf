// expect: 42
// A heap class with two fields: allocate, set both, sum, free.
class Point { public int32 x; public int32 y; }
class Program {
	public static int32 Main() {
		Point p = new Point();
		p.x = 30;
		p.y = 12;
		int32 sum = p.x + p.y;
		delete p;
		return sum;
	}
}
