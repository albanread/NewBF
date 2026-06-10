// NewBF corlib — System.Type (reflection v1, RF-T4).
//
// A value `struct` (deliberately NOT a class — see reflection.md §4.5): a class
// instance carries a `$header` at field 0, which would shift every accessor's
// field index by one relative to the headerless `%struct.Type` constant the
// backend emits (`emit_metadata`). As a `struct`, its lowered layout has no
// `$header`, so its field order is BYTE-IDENTICAL to the emitted aggregate:
//
//   %struct.Type = type { i32, i32, i32, i32, i32, ptr, ptr, ptr, i32, ptr }
//   ;             mSize mTypeId mFlags mFieldCount mMethodCount mName mFields mMethods
//   ;                                          mAttrCount(i32) mAttributes(AttributeInfo*)
//
// `typeof(T)` returns `Ref(Type)` — a pointer to the per-type `%struct.Type`
// constant the backend emits — and these accessors `field_addr` through it. The
// corlib-`Type`-layout-vs-`%struct.Type` unit test pins this ABI contract.
//
// v1 surface: name + id + size always resolve (the always-on TYPE policy);
// the field table (mFields, gated by [Reflect(.Fields)]) is queryable at RF-T6
// via GetFieldCount/GetField; the method table (mMethods, [Reflect(.Methods)])
// is wired by RF-T7. `mFieldCount`/`mMethodCount` are 0 when stripped, so
// `GetFieldCount()` observes the strip differential.
//
// `mFields`/`mMethods` are typed `FieldInfo*`/`MethodInfo*` (not `void*`): each
// points at the policy-gated `[k x %FieldInfo]` / `[m x %MethodInfo]` array the
// backend emits, so `this.mFields[i]` / `this.mMethods[i]` stride by the
// element's ABI size (FieldInfo 16 bytes, MethodInfo 24 bytes — identical to
// `%struct.FieldInfo` / `%struct.MethodInfo`) and read the i-th entry by copy.
// The ABI is unchanged — both `void*` and the typed pointer lower to a bare
// `ptr`; the typed form just lets us index it.
struct Type {
	int32 mSize;
	int32 mTypeId;
	int32 mFlags;
	int32 mFieldCount;
	int32 mMethodCount;
	char8* mName;
	FieldInfo* mFields;
	MethodInfo* mMethods;
	// Custom attributes (CA-T2). APPENDED at indices 8/9 — existing field indices
	// (mFieldCount@3, mFields@6, mMethods@7) are unchanged. `mAttributes` is null
	// and `mAttrCount` 0 everywhere in CA-T2 (no attribute data emitted yet;
	// CA-T4 populates them from the policy-gated `[k x %AttributeInfo]` array).
	int32 mAttrCount;
	AttributeInfo* mAttributes;

	// The type's simple name (a NUL-terminated `char8*` into .rodata). Compare
	// it with `Internal.StrEq` (a char8*-vs-char8* compare), not String.Equals.
	public char8* GetName() { return this.mName; }
	// The object instance size in bytes (the backend's `get_size`).
	public int32 GetSize() { return this.mSize; }
	// The dense runtime type-id (stable per type; distinct across types).
	public int32 GetTypeId() { return this.mTypeId; }
	// The number of reflected fields (0 unless the type is [Reflect(.Fields)]).
	public int32 GetFieldCount() { return this.mFieldCount; }
	// The number of reflected methods (0 unless [Reflect(.Methods)]).
	public int32 GetMethodCount() { return this.mMethodCount; }

	// The i-th reflected field's metadata (RF-T6). Indexes the policy-gated
	// `[k x %FieldInfo]` array `mFields` points at; `mFields[i]` strides by
	// FieldInfo's ABI size. For an out-of-range `i` (i < 0 or i >= count) or a
	// stripped/unmarked type (`mFields == null`), returns a sentinel/empty
	// FieldInfo (NUL name, offset 0, typeId -1) rather than dereferencing out of
	// bounds — never faults (reflection.md §9 RF-T6).
	public FieldInfo GetField(int32 i) {
		FieldInfo empty;
		empty.mName = null;
		empty.mOffset = 0;
		empty.mTypeId = -1;
		if (this.mFields == null) { return empty; }
		if (i < 0) { return empty; }
		if (i >= this.mFieldCount) { return empty; }
		return this.mFields[i];
	}

	// The i-th reflected method's metadata (RF-T7 — symmetric with GetField).
	// Indexes the policy-gated `[m x %MethodInfo]` array `mMethods` points at;
	// `mMethods[i]` strides by MethodInfo's ABI size. For an out-of-range `i`
	// (i < 0 or i >= count) or a stripped/unmarked type (`mMethods == null`),
	// returns a sentinel/empty MethodInfo (NUL name+symbol, paramCount -1) rather
	// than dereferencing out of bounds — never faults (reflection.md §9 RF-T7).
	public MethodInfo GetMethod(int32 i) {
		MethodInfo empty;
		empty.mName = null;
		empty.mSymbol = null;
		empty.mParamCount = -1;
		if (this.mMethods == null) { return empty; }
		if (i < 0) { return empty; }
		if (i >= this.mMethodCount) { return empty; }
		return this.mMethods[i];
	}

	// The number of custom attributes surfaced on this type (CA-T2). 0 unless the
	// type is [Reflect]-marked (the FIELDS policy gates attribute surfacing in
	// v1); 0 everywhere in CA-T2 (no attribute data emitted yet — CA-T4 fills it,
	// so this observes the strip differential exactly like GetFieldCount).
	public int32 GetCustomAttributeCount() { return this.mAttrCount; }

	// The i-th custom attribute's metadata (CA-T2 — symmetric with GetField).
	// Indexes the policy-gated `[k x %AttributeInfo]` array `mAttributes` points
	// at; `mAttributes[i]` strides by AttributeInfo's ABI size. For an
	// out-of-range `i` (i < 0 or i >= count) or a stripped/unmarked type
	// (`mAttributes == null`), returns a sentinel/empty AttributeInfo
	// (mAttrTypeId -1, no args) rather than dereferencing out of bounds — never
	// faults.
	public AttributeInfo GetCustomAttribute(int32 i) {
		AttributeInfo empty;
		empty.mAttrTypeId = -1;
		empty.mArgCount = 0;
		empty.mArgs = null;
		if (this.mAttributes == null) { return empty; }
		if (i < 0) { return empty; }
		if (i >= this.mAttrCount) { return empty; }
		return this.mAttributes[i];
	}
}
