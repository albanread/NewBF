// expect: 577
// Postfix `i++` yields the OLD value (a=5) then bumps i to 6; prefix `++i`
// yields the NEW value (b=7) and leaves i at 7. So 5*100 + 7*10 + 7 = 577.
class Program {
	public static int32 Main() {
		int32 i = 5;
		int32 a = i++;
		int32 b = ++i;
		return a * 100 + b * 10 + i;
	}
}
