// NewBF corlib — System.Internal: the allocation / memory FFI floor. These
// body-less methods bind to the C runtime via [LinkName]; the rest of corlib
// allocates and copies through here. (See CORETYPES.md.)
class Internal {
	[LinkName("malloc")] public static extern void* Malloc(int size);
	[LinkName("free")] public static extern void Free(void* ptr);
	[LinkName("memcpy")] public static extern void* MemCpy(void* dest, void* src, int n);
}
