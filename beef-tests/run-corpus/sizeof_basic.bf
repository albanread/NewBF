// expect: 22
// sizeof(T): scalar byte sizes (int32=4, int64=8, bool=1, char8=1 → 14) plus a
// value struct's inline size (Pair{int32 x; int32 y} = 8, via the IR SizeOf /
// LLVM DataLayout — the same size `new` would allocate). 14 + 8 = 22.
struct Pair {
	public int32 x;
	public int32 y;
}
class Program {
	public static int32 Main() {
		int32 sum = 0;
		sum += sizeof(int32);
		sum += sizeof(int64);
		sum += sizeof(bool);
		sum += sizeof(char8);
		sum += sizeof(Pair);
		return sum;
	}
}
