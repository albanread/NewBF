// expect: 1018
// TA-8 (b): a PENDING `.(…)` arg in a `params T[]` variadic call — in BOTH a fixed
// leading position AND the packed variadic tail. `finish_args`' variadic branch
// back-fills every pending slot first (the fixed leading slot against its declared
// param type, each tail slot against the element type `Vec2`) into a fully-concrete
// vector, THEN delegates to the unchanged `pack_variadic_args`. So `Tag` gets its
// `lead` Vec2 from `.(1,0)` (fixed) and its packed `Vec2[]` from `.(2,3)` / `.(4,5)`
// (tail), each constructed against `Vec2`.
//   Tag(.(1,0), .(2,3), .(4,5)):
//     lead = Vec2(1,0)            -> (1+0)*1000     = 1000
//     pack = [Vec2(2,3),Vec2(4,5)] -> (2+3)+(4+5)   = 5 + 9 = 14
//     total = 1000 + 14 + Sum(.(2,2)) below
//   Sum(.(2,2)) (pure variadic, single pending tail) -> 2+2 = 4
//   1014 + 4 = 1018
struct Vec2 {
	public int32 x;
	public int32 y;
	public this(int32 x, int32 y) { this.x = x; this.y = y; }
}
class Program {
	static int32 Sum(params Vec2[] vs) {
		int32 s = 0;
		for (var v in vs) { s += v.x + v.y; }
		return s;
	}
	static int32 Tag(Vec2 lead, params Vec2[] vs) {
		int32 s = (lead.x + lead.y) * 1000;
		for (var v in vs) { s += v.x + v.y; }
		return s;
	}
	public static int32 Main() {
		int32 a = Tag(.(1, 0), .(2, 3), .(4, 5)); // fixed pending + 2 tail pending → 1000 + 14 = 1014
		int32 b = Sum(.(2, 2));                    // single pending tail → 4
		return a + b;                              // 1014 + 4 = 1018
	}
}
