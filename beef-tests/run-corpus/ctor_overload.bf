// expect: 4321
// Constructor overloading by arity: the same class has both `this()` and
// `this(char8* s)`, and `new Str()` / `new Str("Yo")` each pick the right one.
//   a = new Str();  a.Append('X')      -> "X"   (len 1, 'X'=88)
//   b = new Str("Yo"); b.Append('!')   -> "Yo!" (len 3, 89+111+33=233)
//   (1+3)*1000 + (88+233) = 4321
class Str {
	char8* buf;
	int len;
	int cap;

	public this() {
		this.cap = 4;
		this.buf = malloc(this.cap);
		this.len = 0;
	}
	public this(char8* s) {
		int n = 0;
		while (s[n] != 0) { n = n + 1; }
		this.cap = n + 1;
		this.buf = malloc(this.cap);
		for (int i = 0; i < n; i++) { this.buf[i] = s[i]; }
		this.len = n;
	}
	public ~this() { free(this.buf); }

	public int Length() { return this.len; }
	public char8 CharAt(int i) { return this.buf[i]; }
	public void Append(char8 c) {
		if (this.len >= this.cap) { this.Grow(); }
		this.buf[this.len] = c;
		this.len = this.len + 1;
	}
	void Grow() {
		int nc = this.cap * 2;
		char8* nb = malloc(nc);
		for (int i = 0; i < this.len; i++) { nb[i] = this.buf[i]; }
		free(this.buf);
		this.buf = nb;
		this.cap = nc;
	}
}

class Program {
	public static int32 Main() {
		Str a = new Str();
		a.Append('X');
		Str b = new Str("Yo");
		b.Append('!');

		int total = a.Length() + b.Length();
		int sum = 0;
		for (int i = 0; i < a.Length(); i++) { sum = sum + a.CharAt(i); }
		for (int i = 0; i < b.Length(); i++) { sum = sum + b.CharAt(i); }
		int r = total * 1000 + sum;
		delete a;
		delete b;
		return r;
	}
}
