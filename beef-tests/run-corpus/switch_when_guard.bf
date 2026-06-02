// expect: 42
// `when` guards on a plain value `switch`: `case v when guard:` matches only if
// the value equals the case label *and* the guard holds. A matched-but-guard-
// failed arm falls through to later arms.
//   n = 2, flag = false
//   case 2 when flag:  value matches, guard false → fall through
//   case 2:            value matches               → r = 42
class Program {
	public static int32 Main() {
		int32 n = 2;
		bool flag = false;
		int32 r = 0;
		switch (n) {
		case 1: r = 1;
		case 2 when flag: r = 999;
		case 2: r = 42;
		default: r = -1;
		}
		return r; // 42
	}
}
