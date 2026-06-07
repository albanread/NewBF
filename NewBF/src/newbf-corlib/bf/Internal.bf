// NewBF corlib — System.Internal: the allocation / memory FFI floor. These
// body-less methods bind to the C runtime via [LinkName]; the rest of corlib
// allocates and copies through here. (See CORETYPES.md.)
class Internal {
	[LinkName("malloc")] public static extern void* Malloc(int size);
	[LinkName("free")] public static extern void Free(void* ptr);
	[LinkName("memcpy")] public static extern void* MemCpy(void* dest, void* src, int n);

	// NUL-terminated byte compare of two C-strings (`char8*`). Returns true iff
	// both run the same bytes up to and including the terminating 0. This is the
	// natural comparison for string LITERALS (which lower to `char8*`) and for
	// `Type.GetName()` (also a `char8*`) — String.Equals compares String OBJECTS,
	// not raw buffers (reflection.md §5.6). Used by the reflect_typeof_name gate.
	public static bool StrEq(char8* a, char8* b) {
		int i = 0;
		while (a[i] != 0 && b[i] != 0) {
			if (a[i] != b[i]) { return false; }
			i = i + 1;
		}
		return a[i] == b[i];
	}
}
