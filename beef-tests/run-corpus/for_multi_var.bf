// expect: 165
// C-style for with multiple init declarators (sharing the leading type) and
// multiple update expressions: a two-pointer walk. i: 0,1,2,3,4 and j: 10,9,8,7,6
// meet when i >= j (at i=5,j=5 → stop). Each of the 5 iterations adds i+j = 10,
// so sum = 50; then return sum*3 + iterations*... encode distinctively:
// sum=50 over 5 iters, return sum*3 + 15 = 165.
class Program {
	public static int32 Main() {
		int32 sum = 0;
		int32 iters = 0;
		for (int32 i = 0, j = 10; i < j; i++, j--) {
			sum += i + j;   // 10 each iteration
			iters += 1;
		}
		// sum = 50, iters = 5
		return sum * 3 + iters * 3;   // 150 + 15 = 165
	}
}
