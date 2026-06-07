// NewBF corlib — System.Compiler (comptime emission surface, CB-T3/CB-T4).
//
// The minimal compile-time emission API a `[Comptime, EmitGenerator]` method
// uses to append Beef *source text* to its owning type (comptime-breadth §3.2,
// §5.2). v1 is **primitives-only** — no `Type`, no `StringView`, no `typeof`
// (none of which the emission path lowers yet); the user calls
// `Compiler.EmitTypeBody(text)` and sema (CB-T3) injects the owner-id literal,
// rewriting the call to the host runtime shim `__newbf_ct_emit(ownerId, ptr,
// len)`. The shim is bound into the comptime JIT as an ORC absolute symbol
// (newbf-comptime/emit.rs) and drains emitted text into a thread-local sink the
// CB-T4 fixpoint loop reads.
//
// IMPORTANT: `EmitTypeBody`'s body below is **never executed**. CB-T3 rewrites
// every `Compiler.EmitTypeBody(text)` call inside an emit generator into a
// direct `__newbf_ct_emit(...)` call, so the call never reaches this method at
// runtime — the body exists purely so the call PARSES and type-checks. The
// `[LinkName("__newbf_ct_emit")]` extern declares the shim symbol with the exact
// C ABI sema lowers to (`void(i32, char8*, i32)`), matching the host
// `__newbf_ct_emit(i32 owner_type_id, *const u8 ptr, i32 len)` signature.
//
// Rides the prelude exactly like Type.bf: a duplicate of a corpus `Compiler`
// type is skipped by `register_type_struct` (first/prelude wins), and `analyze`
// (which never loads the prelude) sees no cross-file duplicate — so adding this
// keeps the verify corpus at 154/154.
static class Compiler {
	// The host runtime shim. Resolved by an absolute-symbol definition in the
	// comptime JIT (newbf-comptime); never a process export. `char8*` + `int32`
	// match how sema lowers `text.Ptr` / `text.Len` and the host shim's
	// `(*const u8, i32)` parameters.
	[LinkName("__newbf_ct_emit")]
	public static extern void Comptime_Emit(int32 ownerTypeId, char8* textPtr, int32 textLen);

	// The user-facing emit op. The owner-id arg is INJECTED by sema (CB-T3) — a
	// `[Comptime, EmitGenerator]` body writes `EmitTypeBody(text)` and sema
	// rewrites it to `__newbf_ct_emit(<owner-id literal>, text.Ptr, text.Len)`.
	// This body is therefore unused (see the file header); it is a stub purely
	// so the call parses + verifies.
	[Comptime]
	public static void EmitTypeBody(String text) { }
}
