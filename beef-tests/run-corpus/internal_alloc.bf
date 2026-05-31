// expect: 131
// The corlib `Internal` FFI floor in action: allocate via Internal.Malloc,
// copy bytes via Internal.MemCpy, free via Internal.Free — all bound to the
// CRT through [LinkName]. No bare externs in the program.
class Program {
	public static int32 Main() {
		char8* a = Internal.Malloc(4);
		char8* b = Internal.Malloc(4);
		a[0] = 65;
		a[1] = 66;
		Internal.MemCpy(b, a, 2);
		int32 r = b[0] + b[1];
		Internal.Free(a);
		Internal.Free(b);
		return r;
	}
}
