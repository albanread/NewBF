// expect: 6
// Explicit `= N` sets the running counter; the next unannotated case follows.
enum Code { A = 5, B }   // A = 5, B = 6
class Program {
	public static int32 Main() {
		// (Code.B - Code.A) + Code.A = (6 - 5) + 5 = 6
		return (Code.B - Code.A) + Code.A;
	}
}
