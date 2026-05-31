// expect: 5276
// Milestone A (depth): a *real* growable String written in Beef — constructed
// from a string literal, computing its own length and copying it, and growing
// its heap buffer when Append outgrows capacity. Uses only manual new/delete +
// pointer indexing (param `s[i]`, field `this.buf[i]`, local `nb[i]`) + char
// literals — no memcpy/strlen needed.
//
// "Hi" (H=72,i=105) + '!','!','!' (33 each) -> len 5, sum 276 -> 5*1000+276.
class Str {
	char8* buf;
	int len;
	int cap;

	public this(char8* s) {
		int n = 0;
		while (s[n] != 0) { n = n + 1; }   // strlen, by hand
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
		Str s = new Str("Hi");
		s.Append('!');
		s.Append('!');
		s.Append('!');

		int sum = 0;
		for (int i = 0; i < s.Length(); i++) { sum = sum + s.CharAt(i); }
		int r = s.Length() * 1000 + sum;
		delete s;
		return r;
	}
}
