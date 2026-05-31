// expect: 3210
// Milestone A: a String-shaped class written in Beef, run end to end. Owns a
// char8* buffer (manual new/delete), appends chars into it via field-pointer
// indexing, and reads them back through instance methods.
//
// Append 'H'(72) 'i'(105) '!'(33): len=3, sum=210 -> 3*1000 + 210 = 3210.
class MiniString {
	char8* buf;
	int32 len;

	public this() {
		this.buf = malloc(16);
		this.len = 0;
	}
	public ~this() { free(this.buf); }

	public void Append(char8 c) {
		this.buf[this.len] = c;
		this.len = this.len + 1;
	}
	public int32 Length() { return this.len; }
	public char8 CharAt(int32 i) { return this.buf[i]; }
}

class Program {
	public static int32 Main() {
		MiniString s = new MiniString();
		s.Append('H');
		s.Append('i');
		s.Append('!');

		int32 sum = 0;
		for (int32 i = 0; i < s.Length(); i++) {
			sum = sum + s.CharAt(i);
		}
		int32 r = s.Length() * 1000 + sum;
		delete s;
		return r;
	}
}
