// NewBF corlib — System.Reflection stubs (reflection v1, RF-T7).
//
// The reflection facade namespace. v1 surface (reflection.md §9 RF-T7):
//
//   * `MethodInfo` — name-only reflected-method metadata. This is NOT
//     redeclared here: it already exists as the ABI metatype `MethodInfo`
//     (bf/MethodInfo.bf, the value `struct` whose layout mirrors the emitted
//     `%struct.MethodInfo`). Type registration is keyed by SIMPLE name and
//     dedups duplicates (lower.rs `register_type_struct`), so a namespaced
//     `System.Reflection.MethodInfo` would COLLIDE with — and be silently
//     dropped in favor of — the global metatype. We therefore reconcile by
//     letting the single global `MethodInfo` BE `System.Reflection.MethodInfo`
//     (the metatype is the facade), rather than declaring a second one here.
//
//   * `BindingFlags` — the member-binding filter stub. A flag enum (the
//     standard Beef surface), reserved for a future `Type.GetMethod(name,
//     BindingFlags)` overload. v1 records the flags only; no member-filtering
//     query consumes them yet. A distinct name (no metatype collision), so it
//     parses, verifies, and rides the prelude with the rest of corlib.
namespace System.Reflection
{
	enum BindingFlags
	{
		Default  = 0,
		Instance = 1,
		Static   = 2,
		Public   = 4,
		NonPublic = 8,
	}
}
