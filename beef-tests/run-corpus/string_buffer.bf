// expect: 131
// The String-buffer pattern, end to end: a class owning a char8* buffer, a
// constructor that allocates it, instance methods that index it via `this.`,
// and a destructor that frees it. (`delete` runs ~this -> free.)
class Buffer {
	public char8* data;
	public this() { this.data = malloc(4); }
	public void Set(int32 i, int32 v) { this.data[i] = v; }
	public int32 Get(int32 i) { return this.data[i]; }
	public ~this() { free(this.data); }
}
class Program {
	public static int32 Main() {
		Buffer b = new Buffer();
		b.Set(0, 70);
		b.Set(1, 61);
		int32 r = b.Get(0) + b.Get(1);
		delete b;
		return r;
	}
}
