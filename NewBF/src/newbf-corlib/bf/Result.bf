// NewBF corlib — System.Result<T, E> / Result<T>: a generic tagged-union, either
// an `Ok(payload)` or an `Err(error)` (MX-T5, mixins.md §3.7). Monomorphized per
// `(T, E)` into a value struct `{$disc:i32, $p0:T, $p1:E}` — the same machinery as
// `Option<T>` and any layoutable payload enum, with the type params resolved
// through the instantiation's env (`enum_is_layoutable` gate, proven by MX-T4.5).
//
// It is the canonical Result the corpus shares: a corpus program that USES
// `Result<int32,bool>` (constructs `.Ok`/`.Err`, `switch`es, `Unwrap()`/`Value`)
// no longer needs to declare its own — exactly like `Option<int32>` rides the
// prelude `Option<T>`. Because monomorph keys are by SIMPLE NAME
// (`mangle_generic` → `Result$i32$bool`, namespace-agnostic) and the generic-decl
// index is FIRST-WINS (`index_generic_decls`), this prelude `Result` is the one
// definition the whole corpus resolves to.
//
// `Value`/`Unwrap`'s `.Err` arm returns `default` (the zeroed `T`) — v1 has NO
// `Internal.FatalError` (mixins.md §3.7 defers the fatal path to MX-T8). A struct
// payload `T` is a documented edge (MX-T4.5: `zero_of` on a struct yields `undef`);
// scalar `T` is correct, which is the v1 contract.
//
// Construct it qualified — `Result<int32,bool>.Ok(5)` / `.Err(false)` — or target-
// typed (`.Ok(x)`/`.Err(x)` against the declared/return type), and read the payload
// with `switch` / `case .Ok(let v)`, or via `Unwrap()` / the `Value` property.
enum Result<T, E>
{
	case Ok(T value);
	case Err(E error);

	// The `Ok` payload, or the zeroed `T` on `Err` (v1: no FatalError). A generic
	// enum INSTANCE method that `switch (this)` and returns a payload — the shape
	// MX-T4.5 proved monomorphizes + dispatches on a `this` `Ref`.
	public T Unwrap()
	{
		switch (this)
		{
		case .Ok(var v): return v;
		case .Err(var e): return default;
		}
	}

	// The `Ok` payload as a get-only property, or the zeroed `T` on `Err`. Same
	// `.Err → default` semantics as `Unwrap` (v1: no FatalError).
	public T Value
	{
		get
		{
			switch (this)
			{
			case .Ok(var v): return v;
			case .Err(var e): return default;
			}
		}
	}
}

// Single-type-param convenience: `Result<T>` is `Ok(T)` or a payloadless `Err`
// (the error type defaulted away). Its `Value`/`Unwrap` mirror the two-param form.
enum Result<T>
{
	case Ok(T value);
	case Err;

	public T Unwrap()
	{
		switch (this)
		{
		case .Ok(var v): return v;
		case .Err: return default;
		}
	}

	public T Value
	{
		get
		{
			switch (this)
			{
			case .Ok(var v): return v;
			case .Err: return default;
			}
		}
	}
}
