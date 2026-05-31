// expect: 131
// Pointer indexing over a char8* buffer (byte stride): write two bytes, read
// them back, sum. (malloc/free resolve via the CRT in the JIT.)
class Program {
	public static int32 Main() {
		char8* buf = malloc(4);
		buf[0] = 65;
		buf[1] = 66;
		int32 r = buf[0] + buf[1];
		free(buf);
		return r;
	}
}
