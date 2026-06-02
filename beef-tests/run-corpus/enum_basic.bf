// expect: 5
// Int-backed enums: cases number sequentially from 0; the enum type is int32.
enum Color { Red, Green, Blue }       // 0, 1, 2
enum Dir { North, East, South, West } // 0, 1, 2, 3
class Program {
	public static int32 Main() {
		Color c = Color.Blue;   // 2
		Dir d = Dir.West;       // 3
		return c + d;           // 2 + 3 = 5
	}
}
