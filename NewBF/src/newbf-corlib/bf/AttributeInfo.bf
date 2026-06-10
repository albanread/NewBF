// NewBF corlib — System.Reflection.AttributeInfo (custom attributes v1, CA-T2).
//
// A value `struct` (like `FieldInfo`/`MethodInfo`, deliberately NOT a class —
// see reflection.md §4.5) whose lowered layout is BYTE-IDENTICAL to the
// `%struct.AttributeInfo` aggregate the backend emits in `emit_metadata`:
//
//   %struct.AttributeInfo = type { i32, i32, ptr }
//   ;                            mAttrTypeId  mArgCount  mArgs(i64*)
//
// `Type.GetCustomAttribute(i)` reads `Type.mAttributes[i]` — a typed-pointer
// index into the policy-gated `[k x %AttributeInfo]` array the backend emits for
// a `[Reflect]` type (CA-T4) — and returns this value by copy. Because the
// corlib struct's ABI size matches `%struct.AttributeInfo` (16 bytes: i32+i32=8
// + ptr=8), the `AttributeInfo* mAttributes` index strides correctly over that
// array. The corlib-`AttributeInfo`-layout-vs-`%struct.AttributeInfo` unit test
// pins this ABI (symmetric with the FieldInfo/MethodInfo layout tests).
//
// A reflected attribute's metadata: the dense reflection type-id of the
// attribute CLASS, the count of const-folded scalar ctor args, and an `int64*`
// pointing at the uniform `[n x i64]` arg array (custom-attributes.md §2.4 — an
// int/bool arg is sign-extended to i64; a string arg is the `.rodata` char8*
// `ptrtoint`'d into the i64 slot, reinterpreted by `GetStrArg`).
//
// In CA-T2 no AttributeInfo instances are emitted yet (`mAttributes` is null on
// every Type); CA-T4 populates the table. The accessors below are pure field
// reads with the same out-of-range sentinel discipline as `Type.GetField`, so
// they verify standalone today.
struct AttributeInfo {
	int32 mAttrTypeId;
	int32 mArgCount;
	int64* mArgs;

	// The attribute class's dense reflection type-id (round-trips against
	// `typeof(AttrClass).GetTypeId()`); -1 for the empty/sentinel AttributeInfo.
	public int32 GetTypeId() { return this.mAttrTypeId; }
	// The number of const-folded scalar ctor args recorded for this attribute.
	public int32 GetArgCount() { return this.mArgCount; }

	// The i-th arg read as an int64 (the raw i64 slot). For an out-of-range `i`
	// (i < 0 or i >= count) or a missing arg array (`mArgs == null`), returns 0
	// rather than dereferencing out of bounds — the same sentinel discipline as
	// `Type.GetField` (never faults).
	public int64 GetIntArg(int32 i) {
		if (this.mArgs == null) { return 0; }
		if (i < 0) { return 0; }
		if (i >= this.mArgCount) { return 0; }
		return this.mArgs[i];
	}

	// The i-th arg reinterpreted as a `char8*` (the i64 slot holds a `ptrtoint`'d
	// .rodata pointer for a string arg; this reads it back). For an out-of-range
	// `i` or a missing arg array, returns null. The caller must know the static
	// arg type (`GetIntArg` vs `GetStrArg`) — v1 records no per-arg type tag
	// (custom-attributes.md §2.4).
	public char8* GetStrArg(int32 i) {
		if (this.mArgs == null) { return null; }
		if (i < 0) { return null; }
		if (i >= this.mArgCount) { return null; }
		return (char8*)this.mArgs[i];
	}
}
