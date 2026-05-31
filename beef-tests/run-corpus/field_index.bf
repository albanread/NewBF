// expect: 42
// Indexing a pointer *field* through an object reference: `h.nums[i]`.
class Holder {
	public int32* nums;
	public this() { this.nums = malloc(16); }
	public ~this() { free(this.nums); }
}
class Program {
	public static int32 Main() {
		Holder h = new Holder();
		h.nums[0] = 40;
		h.nums[1] = 2;
		int32 r = h.nums[0] + h.nums[1];
		delete h;
		return r;
	}
}
