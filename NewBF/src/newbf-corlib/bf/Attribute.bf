// NewBF corlib — System.Attribute (custom attributes v1, CA-T2).
//
// The conventional base for user-declared attribute types. It is a `class`, NOT
// a value `struct` (custom-attributes.md §2.5), for two load-bearing reasons:
//
//   1. v1 attribute types must be classes — a dense reflection type-id is minted
//      only for `StructKind::Ref` (classes), so an attribute keyed by its dense
//      id (`GetCustomAttribute(i).GetTypeId() == typeof(MyAttr).GetTypeId()`)
//      must be a class to have an id at all. A `class` base also resolves to
//      `IrType::Ref`, which is the only kind base-routing records — a `struct`
//      base resolves to `IrType::Struct` and is silently dropped.
//   2. It keeps this minimal `Attribute` from colliding with future corlib
//      attribute machinery (`AttributeTargets`/`AttributeUsage`, deferred §5).
//
// v1 does NOT enforce `: Attribute` (any resolvable class name works as an
// attribute, §5); this base exists only so Beef-style `[MyAttr] : Attribute`
// declarations resolve their base rather than discard it. It is intentionally
// empty and verifies clean standalone.
class Attribute {
}
