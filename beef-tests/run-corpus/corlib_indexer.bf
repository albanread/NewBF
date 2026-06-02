// expect: 322
// corlib indexers: `List<T>` gains a read/write `this[i]` and `String` a
// read-only `this[i]`, so collections index idiomatically (`xs[i]` not
// `xs.Get(i)`).
//   xs = [5, 10, 15]; xs[1] = 20 → [5, 20, 15]; sum via xs[i] = 40 → *5 = 200
//   "ABC": s[0]='A'(65), s[2]='C'(67) → 65 + 67 = 132 ... wait recompute below
// Compute exactly:
//   list part: (xs[0] + xs[1] + xs[2]) = 5 + 20 + 15 = 40
//   str  part: (int)s[0] + (int)s[2]   = 65 + 67       = 132
//   plus 150 sentinel → 40 + 132 + 150 = 322
class Program {
	public static int32 Main() {
		List<int32> xs = new List<int32>();
		xs.Add(5);
		xs.Add(10);
		xs.Add(15);
		xs[1] = 20;                 // write through the indexer

		int32 listsum = xs[0] + xs[1] + xs[2]; // 5 + 20 + 15 = 40

		String s = "ABC";
		int32 strsum = s[0] + s[2]; // 'A'(65) + 'C'(67) = 132

		delete xs;
		delete s;
		return listsum + strsum + 150; // 40 + 132 + 150 = 322
	}
}
