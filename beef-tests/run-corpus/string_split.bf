// expect: 306
// corlib String.Split(char8) → String[]: splits on a separator, returning a
// heap array of owned String parts. Exercises arrays of class references created
// and returned from a corlib method, then indexed and method-called by the user.
//   "a,bb,ccc".Split(',') → ["a","bb","ccc"]  (Count 3, lengths 1/2/3)
class Program {
	public static int32 Main() {
		String s = "a,bb,ccc";
		String[] parts = s.Split(',');
		int32 r = (int32)parts.Count * 100;   // 300
		r = r + (int32)parts[0].Length();     // + 1
		r = r + (int32)parts[1].Length();     // + 2
		r = r + (int32)parts[2].Length();     // + 3
		delete parts[0];
		delete parts[1];
		delete parts[2];
		delete parts;
		delete s;
		return r;                             // 306
	}
}
