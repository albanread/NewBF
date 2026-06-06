// expect: 1591
// corlib String.Append(int): decimal rendering into a String, selected over the
// char8/String Append overloads by the argument type. Build "-1024" then append
// 7 → "-10247": length 6, last char '7' (= 55). Result = 6*256 + 55 = 1591.
class Program {
	public static int32 Main() {
		String s = new String();
		s.Append(-1024);              // "-1024" (len 5)
		s.Append(7);                  // "-10247" (len 6)
		int32 len = (int32)s.Length();
		char8 last = s.CharAt(5);     // '7' = 55
		delete s;
		return len * 256 + last;      // 6*256 + 55 = 1593
	}
}
