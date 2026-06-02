// NewBF corlib — Option<T>: a generic tagged-union, either a payload `Some` or an
// empty `None`. Monomorphized per element type T into a value struct
// `{$disc:i32, $p0:T}` (the same machinery as a non-generic payload enum, with T
// resolved through the instantiation's type-param env).
//
// Construct it qualified — `Option<int>.Some(x)` / `Option<int>.None` — and
// destructure with `switch` / `case .Some(let v)`. (Target-typed `.Some(x)` and
// methods like `GetValueOrDefault` / `Unwrap` await target-type propagation and
// enum-method lowering — both queued follow-ons.)
enum Option<T>
{
	case Some(T value);
	case None;
}
