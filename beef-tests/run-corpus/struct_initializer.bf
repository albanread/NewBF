// expect: 87
// Target-typed struct initializer `.{ field = value }`: previously the parser
// dropped the `{ … }` body (the struct stayed undef), so this now actually sets
// the fields. The local's declared type supplies the target. Also feeds a value
// struct with an overloaded `operator<` to confirm initialized fields flow into
// a real comparison.
struct Point { public int32 x; public int32 y; }
struct Money {
	public int32 cents;
	public static bool operator<(Money a, Money b) { return a.cents < b.cents; }
}
class Program {
	public static int32 Main() {
		Point p = .{ x = 30, y = 12 };       // 42
		Money lo = .{ cents = 100 };
		Money hi = .{ cents = 250 };
		int32 cmp = (lo < hi && !(hi < lo)) ? 45 : 0;  // 45
		return p.x + p.y + cmp;              // 42 + 45 = 87
	}
}
