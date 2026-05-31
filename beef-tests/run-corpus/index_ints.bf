// expect: 60
// Pointer indexing over an int32* buffer (4-byte stride) with a variable index:
// fill a[i]=i*10 in a loop, then sum. 0+10+20+30 = 60.
class Program {
	public static int32 Main() {
		int32* a = malloc(16);
		for (int32 i = 0; i < 4; i++) {
			a[i] = i * 10;
		}
		int32 sum = 0;
		for (int32 i = 0; i < 4; i++) {
			sum += a[i];
		}
		free(a);
		return sum;
	}
}
