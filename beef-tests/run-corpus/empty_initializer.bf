// expect: 102
// An empty initializer `.{}` still constructs: it applies the value struct's
// field defaults rather than being dropped. Pt's defaults (x=1, y=2) take
// effect, then a second `.{ y = 9 }` overrides y. r = (1*10+2) + (1*100 - 10) ...
// keep it simple: p1 = .{} → x=1,y=2 → 12; p2 = .{ y = 9 } → x=1,y=9 → 90... encode:
// p1.x*100 + p1.y*1 + p2.y*... → 1*100 + 2 + 0 = 102 (only p1 used distinctively).
struct Pt { public int32 x = 1; public int32 y = 2; }
class Program {
	public static int32 Main() {
		Pt p1 = .{};            // defaults: x=1, y=2
		Pt p2 = .{ y = 9 };     // x default 1, y=9
		return p1.x * 100 + p1.y + (p2.y - p2.x - 8);  // 100 + 2 + (9-1-8)=0 → 102
	}
}
