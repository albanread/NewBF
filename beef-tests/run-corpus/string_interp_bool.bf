// expect: 9
// Interpolation with a bool hole: String.Append(bool) renders true/false, so
// $"{a},{b}" with a=true, b=false → "true,false" (length 10). The last char is
// 'e' (= 101). Return length - 1 = 9 (kept small; the point is it built the
// 10-char rendering rather than skipping the bool holes).
class Program {
	public static int32 Main() {
		bool a = true;
		bool b = false;
		String s = $"{a},{b}";       // "true,false"
		int32 len = (int32)s.Length();
		delete s;
		return len - 1;              // 10 - 1 = 9
	}
}
