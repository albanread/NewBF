// expect: 42
// CA-T5: a const-folded primitive (int) ctor arg round-trips through the
// attribute metadata. `Priority(42)` folds 42 → `Const::Int(42, I64)` (CA-T1's
// `attr_arg_const`), lands in the uniform `[n x i64]` arg array (CA-T4's
// emission, the value sign-extended into the i64 slot), and is read back by
// `AttributeInfo.GetIntArg(0)` (CA-T2's accessor, the raw i64 slot), narrowed to
// int32. 42 <= 255 so the AOT exit-code truncation note (MEMORY) is not tripped;
// the JIT run-corpus harness checks the full i32 regardless.
//
// Priority is a CLASS (v1 attribute = class, so it has a dense type-id), no
// [Reflect] needed; only the annotated type Job is [Reflect], where the FIELDS
// gate decides whether attributes surface.
//
// NOTE: the AttributeInfo is bound to a local (`AttributeInfo a = …`) before
// `GetIntArg(0)` — calling a method directly on a by-value struct rvalue returned
// by another call (`typeof(Job).GetCustomAttribute(0).GetIntArg(0)`) is a
// pre-existing lowering gap (R5: the by-value-struct receiver collapses to undef).
// Binding to a local is the idiomatic form and exercises the same emitted table.
class Priority : Attribute { public this(int32 p) { } }
[Reflect, Priority(42)] class Job { public int32 mX; }
class Program {
	public static int32 Main() {
		AttributeInfo a = typeof(Job).GetCustomAttribute(0);
		return (int32)a.GetIntArg(0);   // 42 — the folded ctor arg (i64 slot, narrowed)
	}
}
