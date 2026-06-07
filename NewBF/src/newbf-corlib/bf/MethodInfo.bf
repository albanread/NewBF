// NewBF corlib — System.Reflection.MethodInfo (reflection v1, RF-T7).
//
// A value `struct` (like `Type`/`FieldInfo`, deliberately NOT a class — see
// reflection.md §4.5) whose lowered layout is BYTE-IDENTICAL to the
// `%struct.MethodInfo` aggregate the backend emits in `emit_metadata`:
//
//   %struct.MethodInfo = type { ptr, ptr, i32 }
//   ;                          mName(char8*) mSymbol(char8*) mParamCount
//
// `Type.GetMethod(i)` reads `Type.mMethods[i]` — a typed-pointer index into the
// policy-gated `[m x %MethodInfo]` array the backend emits for a
// `[Reflect(.Methods)]` type — and returns this value by copy. Because the
// corlib struct's ABI size matches `%struct.MethodInfo` (24 bytes: ptr=8 +
// ptr=8 + i32+pad=8), the `MethodInfo* mMethods` index strides correctly over
// that array. The corlib-`MethodInfo`-layout-vs-`%struct.MethodInfo` unit test
// pins this ABI (symmetric with RF-T6's FieldInfo layout test).
//
// A reflected method's metadata: its simple name (a NUL-terminated `char8*`
// into .rodata), its mangled symbol (also a `char8*`), and its explicit
// (source-level) parameter count — `this` excluded for instance methods.
struct MethodInfo {
	char8* mName;
	char8* mSymbol;
	int32 mParamCount;

	// The method's simple name (a NUL-terminated `char8*` into .rodata). Compare
	// it with `Internal.StrEq` (a char8*-vs-char8* compare), not String.Equals.
	public char8* GetName() { return this.mName; }
	// The method's mangled symbol (the emitted function name, a `char8*`).
	public char8* GetSymbol() { return this.mSymbol; }
	// The explicit (source-level) parameter count — `this` excluded for an
	// instance method.
	public int32 GetParamCount() { return this.mParamCount; }
}
