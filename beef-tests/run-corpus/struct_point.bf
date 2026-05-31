// expect: 7
// Value struct: write two fields on a stack-local, read them back, add.
struct Point { public int32 x; public int32 y; }
class Program {
	public static int32 Main() {
		Point p = ?;
		p.x = 3;
		p.y = 4;
		return p.x + p.y;
	}
}
