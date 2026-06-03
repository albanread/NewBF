// expect: 42
// Tuples: a `(int32, int32)` value lowers to a synthetic value struct with
// fields named "0"/"1". The literal `(30, 12)` target-types to the declared
// shape (the i64 literals coerce to the i32 fields); `t.0`/`t.1` read the fields
// through the ordinary struct-member path.
class Program {
	public static int32 Main() {
		(int32, int32) t = (30, 12);
		return t.0 + t.1;   // 42
	}
}
