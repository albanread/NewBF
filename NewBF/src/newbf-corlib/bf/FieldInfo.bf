// NewBF corlib — System.Reflection.FieldInfo (reflection v1, RF-T6).
//
// A value `struct` (like `Type`, deliberately NOT a class — see reflection.md
// §4.5) whose lowered layout is BYTE-IDENTICAL to the `%struct.FieldInfo`
// aggregate the backend emits in `emit_metadata`:
//
//   %struct.FieldInfo = type { ptr, i32, i32 }
//   ;                        mName(char8*) mOffset mTypeId
//
// `Type.GetField(i)` reads `Type.mFields[i]` — a typed-pointer index into the
// policy-gated `[k x %FieldInfo]` array the backend emits for a
// `[Reflect(.Fields)]` type — and returns this value by copy. Because the
// corlib struct's ABI size matches `%struct.FieldInfo` (16 bytes: ptr=8 +
// i32+i32=8), the `FieldInfo* mFields` index strides correctly over that array.
// The corlib-`FieldInfo`-layout-vs-`%struct.FieldInfo` unit test pins this ABI.
//
// A reflected field's metadata: its simple name (a NUL-terminated `char8*` into
// .rodata), its byte offset within the object body, and the dense type-id of the
// field's own type (0 when that type isn't reflected).
struct FieldInfo {
	char8* mName;
	int32 mOffset;
	int32 mTypeId;

	// The field's simple name (a NUL-terminated `char8*` into .rodata). Compare
	// it with `Internal.StrEq` (a char8*-vs-char8* compare), not String.Equals.
	public char8* GetName() { return this.mName; }
	// The field's byte offset within the object body (the backend DataLayout's
	// `offset_of_element` for the field's physical index).
	public int32 GetOffset() { return this.mOffset; }
	// The dense type-id of the field's own type (0 when that type isn't reflected).
	public int32 GetTypeId() { return this.mTypeId; }
}
