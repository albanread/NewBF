# Dynamic Interface Dispatch — Interface-Typed Values

## 1. Problem & goal

NewBF can call interface methods only through the **generic-constraint** form, where monomorphization erases the interface entirely:

```beef
// WORKS today (§46): T is monomorphized to Holder, so val.Get() resolves
// statically to Holder.Get.
static int32 Use<T>(T val) where T : IFace { return val.Get(); }
```

It **cannot** call a method through an interface-*typed* value — a parameter, local, field, or return of an interface type:

```beef
interface IShape { int32 Area(); }
class Square : IShape { public int32 Area() { return 9; } }

class Program {
    // BROKEN today: `s` lowers to an opaque Ptr; s.Area() returns undef(i32).
    static int32 AreaOf(IShape s) { return s.Area(); }
    static int32 Main() {
        Square sq = new Square();
        IShape s = sq;          // upcast: today a no-op, identity is lost
        int32 r = AreaOf(s);    // expected 9, today garbage
        delete sq;
        return r;
    }
}
```

**Why it's wrong, concretely.** An `IShape`-typed param resolves via `lower_ty_env` to `primitive("IShape")` → `IrType::Ptr` (the unknown-name fallback). At the call site `s.Area()`, `struct_base` (lower.rs:4955) sees a bare `Ptr` operand and returns `None` (the `Some(_) => None` arm at 4962). The methods-keyed instance block at 5854 is never entered, so control falls to the catch-all at lower.rs:5899–5900 returning `(undef(IrType::I64), IrType::I64)`. The call is silently dropped; the result is undefined. The §46 survey counted ~21 such interface-typed-value sites in the corpus.

**Goal.** Make a method call through an interface-typed value dispatch to the concrete implementation, for the common shape: a (single-inheritance) class implementing one or more **non-generic** interfaces with **abstract** (body-less, no `abstract` keyword) instance methods, plus **default** interface methods (instance methods with a body in the interface). Cover: upcasting class→interface; passing/returning/storing/loading interface values; `is`/`as` against an interface; interaction with the existing class vtable and the `$header` (`IrType::Ptr`) at offset 0. Boxing of value structs to interfaces is explicitly **out of scope for v1** (see §6, §10).

The target capability: the example above returns 9, through a real indirect call, with the LLVM verifier green and no regression to the 152/152 verify, 152/152 parser, or ~160 run-corpus gates.

## 2. Current state (grounded in file:line)

- **`IrType`** (newbf-ir/src/ty.rs:14–36): `Copy` enum `{ Void, Bool, Int, Float, Ptr, Struct(StructId), Ref(StructId) }`. `Ref(id)` lowers to LLVM `ptr` but carries nominal class info. No interface representation.
- **`StructKind`** (lower.rs:36–40): `enum { Value, Ref }`, `#[derive(Clone, Copy)]`. **`struct_kind`** (42–48) returns `Some(Value)`/`Some(Ref)` for struct/class; **`_ => None`** for interface/enum/extension. Interfaces are never registered as types.
- **`ty_of`** (lower.rs:227–234): matches **only** `{ Value => Struct, Ref => Ref }` — **non-exhaustive the moment `StructKind` gains a variant** (a hard compile error to fix in T1).
- **`register_type_struct`** (lower.rs:932–964): only registers types where `struct_kind` is `Some` and non-generic; pushes a fixed set of parallel-vec slots (`defs/kinds/prefixes/ctors/dtors/methods/field_elems/bases/virtuals/vslots/vimpls`) and a `by_name` entry. Interfaces get no `StructId` today. **No mono-path duplicate of this push exists**: monos are filled by `fill_members_at` over ids already minted in `record_inst` — but any new parallel vec must be defaulted via `#[derive(Default)]` so every id-minting site stays in lockstep (see §5).
- **`fill_fields_at`** (lower.rs:1337–1353): a class (`matches!(kind, StructKind::Ref)`) gets a `$header : IrType::Ptr` field at offset 0; a value struct gets none.
- **Base handling** (`fill_members_at`, lower.rs:1472–1487): the base-recording loop is **already guarded** by `if matches!(kind, StructKind::Ref)` (1477) — so an *interface subject* won't record bases. **But** a *class subject* listing an interface base (`class X : IFace, Base`) resolves that base via `lower_ty_env`; once interfaces are registered (T1) it resolves to `Ref(iface_id)` and the `if let IrType::Ref(bid) … break` at 1479–1483 would record the **interface** as the class's single inheritance base — corrupting `apply_inheritance`. This is the central T1 hazard (§5).
- **Method registration** (lower.rs:1517–1605): a body-less member is dropped at 1551–1554 (`None => continue`) **unless** it is `abstract` — and `is_abstract` (1541–1544) requires an **explicit `Modifier::Abstract`**. Beef interface methods are abstract by being in an interface (no `abstract` keyword), so today they are dropped from `methods[]` entirely. `is_virtual` (1592–1597) + `is_instance` + (abstract or bodied) pushes a `(name, symbol)` into `virtuals[id]`. `explicit_iface` is parsed (ast.rs:843,876) and read by the model graph (build.rs:276–296) but **the lowerer's `Member::Method` arm destructures it into `..` and never reads it** — explicit impls register under the bare final-segment name.
- **`MethodSig`** (owned, no AST borrow): `{ full_name, ret, params (this-leading for instance), is_instance, variadic }`.
- **vtables** — `apply_vtables`/`compose_vtable` (lower.rs:515–543): base-first, memoized; inherits base slots, `override` replaces a slot, a new `virtual` appends. `vslots` (name→slot), `vimpls` (slot→symbol). Emission: a vtable global is emitted **only if `vimpls[i]` is non-empty** (lower.rs:1941–1948). `new` stores the vtable global into `$header` **only if `vimpls[id]` is non-empty**, else null (lower.rs:5182–5189). Virtual dispatch (lower.rs:5884–5895) is **nested inside** the methods-keyed guard at 5854–5859: `if let Some((body_ptr,owner_id)) = struct_base(..) && let Some(sig) = methods[owner_id].get(mname).and_then(pick_overload..)`. For an interface receiver that `methods` lookup fails (abstract methods aren't in `methods`), so this whole block — and any code placed "inside it before 5884" — is unreachable.
- **`is`/`as`** (lower.rs:5608–5683): `type_test` (5625–5647) builds an OR-chain of `$header == ClassX$vtable` over `is_subtype_of(c, tid) && !vimpls[c].is_empty()`; it reads the header via `field_addr(obj, oid, 0)` (5633). `is_subtype_of` (5609–5617) walks the class `bases` chain **only**. `type_id_of` (5650–5656) resolves a name to a `StructId` via `by_name`. `lower_is`/`lower_as` gate on `IrType::Ref(oid)`.
- **`lower_ty_env`**: a bare unknown name → `primitive(name)` → `Ptr`. A *generic* instantiation (`IFaceD<int16>`) resolves via the mangled name; generic interfaces aren't registered, so it stays `Ptr`.
- **`coerce`** (lower.rs:6068–6134): `from == to` is identity (6069); **any pointer-like → pointer-like is already a no-op reinterpret** (6128, `(a,b) if a.is_pointer() && b.is_pointer() => v`). `Ref(class)→Ref(iface)` therefore already coerces by identity — **no new arm is needed** (a gated duplicate would be dead code).
- **LLVM vtable emission** (newbf-llvm/src/lower.rs:218–238): each vtable is emitted as its **own** global typed `ptr_ty.array_type(entries.len())`; **an unresolved entry name already becomes `const_null`** (228). There is **no cross-vtable length requirement**, and dispatch's `elem_addr` is a raw GEP on an opaque `ptr` that never references the array type.
- **def graph** (model.rs): `TypeKindD::Interface` exists; `TypeDef.bases` and `explicit_iface` are captured.
- **Upstream Beef** (BfResolvedTypeUtils.h:1637–1653): `BfTypeInterfaceEntry { mStartInterfaceTableIdx, mStartVirtualIdx }` — interface methods are laid into the **class vtable**, offset by `mStartVirtualIdx`, found from the concrete type via runtime type-data. Object-header-vdata model; the object ptr is unchanged across an upcast.

## 3. Approach

**Chosen design: object-header vdata with interface methods appended to the class vtable. The interface-typed IR value is the plain object pointer (`IrType::Ref(interface_id)`); the interface identity is the `StructId` of the (now-registered) interface, and dispatch goes through the receiver's existing `$header` vtable at a slot computed from `(interface, method)`.**

Concretely, in four moves:

1. **Register interfaces as a new `StructKind::Interface`.** `struct_kind` returns `Some(Interface)` for `TypeKind::Interface`. An interface gets a `StructId`, a `by_name` entry, a prefix, and a method table — but **no field layout** (no `$header`, no instance fields; it is never instantiated). `IrType::Ref(iface_id)` is the type of an interface-typed value: still a plain `ptr` to *some object body*, carrying the interface's nominal id at the sema level.

2. **Capture interface bases separately, and compose per-(class, interface) slot ranges into the class vtable.** Add `iface_bases: Vec<Vec<StructId>>` (the interfaces a class implements, transitively flattened, dedup'd) and `imethods: Vec<Vec<(String, MethodSig)>>` (per interface, its **instance, non-generic** methods in declaration order, base-interface methods first — the slot signature). After `apply_vtables`, run **`apply_itables`**: for each class, for each implemented interface, append the interface's methods to that class's `vimpls`/`vslots` as additional vtable slots at a globally-fixed base. Each interface slot's impl symbol is the class's matching method (`pick_overload` on the same name, including inherited methods), else the interface's own default-body symbol, else an empty/null placeholder.

3. **Dispatch through the appended vtable slots.** When `struct_base` yields `(body_ptr, owner_id)` and `owner_id`'s kind is `Interface`, dispatch directly (a **separate branch before** the methods-keyed block at 5854): the slot is globally fixed for `(interface, method)`, so the concrete class is not needed — `header → vtable[iface_slot_base[I] + method_idx] → call_indirect`, identical in shape to virtual dispatch.

4. **Upcast is a no-op; `is`/`as` reuse vtable tags.** `Square → IShape` keeps the same pointer; only the sema type changes from `Ref(square_id)` to `Ref(ishape_id)`. `coerce` already treats pointer-likes interchangeably (6128). `x is IShape` / `x as IShape` reuse a header-tag test, widening its target set to "every class whose `iface_bases` contains `IShape`."

This keeps interface values 8 bytes, makes upcasting free, requires **no new `IrType` variant** (preserving `Copy`), adds **no new IR instruction** (reuses `FieldAddr`/`Load`/`ElemAddr`/`CallIndirect`/`GlobalAddr`), and reuses the entire vtable emission + verifier-safe dispatch pattern. The only new IR-level artifact is **wider vtable globals** (already `VtableDef`).

### Alternatives considered & rejected

- **Fat pointer `{ obj_ptr, itable_ptr }` (a new `IrType::ItableRef` or a 2-word struct).** *Rejected.* Every interface value becomes 16 bytes, breaking the uniform 8-byte pointer ABI for params/returns/fields; upcasting becomes a value-construction site (an SSA-dominance trap); and it forks `coerce`, `Load`/`Store` of interface fields, and the calling convention. Beef itself rejected fat pointers for the same ABI-uniformity reason.
- **Separate per-(class, interface) itable globals loaded from a widened header struct.** *Rejected for v1.* Needs the `$header` to become a struct `{ vtable, itable[] }` and a runtime interface-id→itable lookup at a call site where the concrete class is unknown — exactly the complexity Beef avoids by folding interface methods into the single class vtable.
- **Interface methods at per-class offsets (Beef's `mStartVirtualIdx`).** *Rejected for v1.* Beef resolves the per-class offset from the concrete type via runtime type-data. NewBF has no runtime type-data table yet. A **global per-interface slot base** sidesteps it entirely, at the cost of every implementer reserving the union of its interfaces' slots (acceptable: single inheritance, few interfaces per class, no generic interfaces in v1). Per-class offsets become viable once a type-data table exists (a follow-on).

## 4. Representation & IR changes

**No change to `IrType`** (stays `Copy`). An interface-typed value is `IrType::Ref(iface_id)` where `iface_id`'s kind is `Interface`. `is_pointer` already includes `Ref(_)`, so all coercion/ABI treats it as `ptr`.

**`StructKind` gains a variant** (lower.rs:36–40):
```rust
#[derive(Clone, Copy)]
enum StructKind { Value, Ref, Interface }
```
`struct_kind` returns `Some(StructKind::Interface)` for `TypeKind::Interface`. **`ty_of` (227) gains the arm `StructKind::Interface => IrType::Ref(id)`** (interfaces are pointer-like; the match is otherwise non-exhaustive). Interfaces are registered with an **empty `StructDef`** (no `$header`, no fields). `fill_fields_at`'s `$header` insertion stays gated on `Ref`, so an interface never gets a header field. `struct_base` must never `FieldAddr` through an interface id (it uses a raw header GEP — §5).

**`StructTable` new fields** (all owned data — no lifetime; `#[derive(Default)]` so every id-minting site stays in lockstep):
```rust
/// Per class id: the interfaces it implements, transitively flattened and
/// dedup'd, deterministic order. Empty for value structs and interfaces.
iface_bases: Vec<Vec<StructId>>,
/// Per interface id: its instance, NON-GENERIC method slot signature,
/// (name, sig) in declaration order (base-interface methods first). Drives
/// slot layout and the method->index lookup at dispatch.
imethods: Vec<Vec<(String, MethodSig)>>,
/// Per interface id: a default-body symbol per slot (`Some` for a default
/// interface method, `None` for an abstract one), parallel to `imethods`.
idefaults: Vec<Vec<Option<String>>>,
/// Explicit interface implementations: (class id, iface id, method name)
/// -> the impl MethodSig. Consulted by apply_itables before the implicit
/// same-name pick_overload. Filled from Member::Method.explicit_iface.
explicit_impls: HashMap<(StructId, StructId, String), MethodSig>,
/// Global per-interface vtable slot base: interface id -> first vtable slot
/// every implementer reserves for it. Stable across all implementers.
iface_slot_base: HashMap<StructId, usize>,
```
These are `Default`-constructed by `#[derive(Default)]` on `StructTable`. **No parallel-vec `push` is needed in `register_type_struct`** for the `HashMap`s; for the per-id `Vec` fields (`iface_bases`, `imethods`, `idefaults`) a `push(Default::default())` is added at the **single** id-minting site (932–957) — there is no second mono id-minting site that bypasses it (monos reuse ids minted in `record_inst`, which goes through the same registration path; audit confirms this in T1).

**Vtable layout / ABI.** A class's vtable global (`Class$vtable`) is laid out as:
```
[ 0 .. nclass )                      class virtual/override/abstract slots (unchanged, §27/§30)
[ iface_slot_base[I0] .. +len(I0) )  interface I0's methods, globally-fixed base
[ iface_slot_base[I1] .. +len(I1) )  interface I1's methods, globally-fixed base
...
```
**Global slot-base assignment (unambiguous).** After `apply_vtables` has fully composed *every* class's `vimpls` (so all class-vtable lengths are final, including monomorphized generic classes), compute `N = max over ALL ids of vimpls[c].len()`. Walk interfaces in `StructId` order with a cursor `base = N`; for each interface `I`, set `iface_slot_base[I] = base` and advance `base += imethods[I].len()`. This guarantees `iface_slot_base[I] >= vimpls[c].len()` for **every** class `c` (its class block is `[0, nclass) ⊆ [0, N)`), so no interface block ever overlaps a class block or another interface block in any implementer. A `debug_assert!(iface_slot_base[I] >= vimpls[c].len())` before writing each slot pins this.

**Padding is for runtime bounds-safety, not the verifier.** Each `Class$vtable` global is emitted with its own `array_type(len)` (newbf-llvm:234); there is **no uniform-length requirement** and dispatch's `elem_addr` is a raw GEP on an opaque `ptr`. The real hazard is indexing `vtable[iface_slot_base[I] + k]` past a class's own (short) array at **runtime** (a GEP on an opaque ptr won't error; it reads adjacent memory). So: grow each implementer's `vimpls` to at least `max over its implemented I of (iface_slot_base[I] + len(I))`, filling any gap (between `nclass` and its first used iface block, and between non-contiguous iface blocks) with an **empty-string placeholder**, which `emit_vtables` already lowers to `const_null` (228). **No `$abort` extern and no global-uniform padding are needed** (both were over-scoped in the draft). An unimplemented required interface slot also gets a null placeholder; calling it segfaults cleanly (acceptable for v1) and is paired with a sema diagnostic at composition (§5, T3).

## 5. Sema / parser / codegen changes

**Parser / AST.** No changes. Interfaces, interface bases, abstract and default interface methods, and explicit `IFace.Method` implementations already parse. `(IFaceA)cba` cast syntax already parses.

**lower.rs — registration (T1).**
- `StructKind` (36–40): add `Interface`.
- `struct_kind` (42–48): add the `Interface` arm.
- `ty_of` (227–234): add `StructKind::Interface => IrType::Ref(id)`. **Audit every `match`/`matches!` on `StructKind`** so each compiles and treats `Interface` as pointer-like but never gives it a `$header` or base composition: `fill_fields_at` (1347, stays `Ref`-only — interface gets no header), the base loop (1477, stays `Ref`-only — see next bullet), `register_type_struct`, `new_class_id`/size paths (an interface is never `new`'d).
- `register_type_struct` (932–964): register interfaces too (empty `StructDef`, `kinds[id]=Interface`, push the per-id `Vec` parallel slots incl. `iface_bases`/`imethods`/`idefaults`). Runs in the existing name pass so interface ids exist before any base resolves.
- **Base-routing fix, landed atomically in T1:** guard the base-recording loop at 1479 with `&& matches!(self.kinds[bid.0 as usize], StructKind::Ref)` so a class listing an interface base does **not** record the interface as its inheritance base. Interface-kind bases are handled by `collect_iface_bases` (T2). Without this guard, registering interfaces (T1) immediately corrupts `apply_inheritance` for `class X : IFace, Base` — so T1 is **not** behavior-neutral and the guard must ship with it.

**lower.rs — interface members & bases (T2).**
- New `fill_iface_members` (or an interface arm in `fill_members_at`): for an interface id, record **every instance, non-generic** method into `imethods[id]` with a full `MethodSig` (this-leading `Ref(iface_id)`, ret/params via `lower_ty_env`), in declaration order, base-interface methods first. **Explicitly filter out `static` and generic interface methods** (`Self`-returning `IParsable.Parse`, `IFaceD<T>.GetVal`, `IEntity.GetComponent<T>`) — they stay on the static/constraint path and must **not** consume slot indices, or every implementer's layout desyncs.
  - **Abstract interface methods (no `abstract` keyword) must be recorded** despite the body-less `None => continue` at 1551–1554; `fill_iface_members` records them into `imethods` directly (it does not reuse the class-method gate). They are dispatched only indirectly through the slot, so their `full_name` need never be emitted.
  - A **default** interface method (bodied) is recorded in `imethods` with `idefaults[id][k] = Some({IFace.prefix}{Method})`; an abstract one with `idefaults[id][k] = None`. **Default bodies are NOT put into `methods[iface]` in v1** — doing so would let a class's call to a default it overrides resolve to a direct non-virtual call on the wrong body (the T4/T6 interleave hazard). Defaults reach a class only through the itable slot. (The generic-constraint case for defaults is a documented follow-on, not v1.)
- New `collect_iface_bases`: for each **class** id, resolve each base via `lower_ty_env`; an `Interface`-kind base goes into `iface_bases[id]` flattened with that interface's own transitive interface bases (dedup'd); a `Ref`-kind base is the single class base (already recorded by the guarded loop). **Value structs and interfaces themselves are skipped** (a value struct that lists an interface base has no `$header`/vtable to dispatch through — boxing is out of scope; it must not enter `iface_bases` and must not get itable slots).
- Capture `explicit_iface`: in the `Member::Method` arm (1517), read `explicit_iface` instead of `..`; when present and it resolves to an interface id, also record the method into `explicit_impls[(class_id, iface_id, name)]`. (The method still registers under its bare name in `methods[class]` as today; the explicit map only disambiguates itable resolution.)

**lower.rs — itable composition (`apply_itables`, new, called in `StructTable::build` immediately after `apply_vtables` at 221, i.e. after `apply_inheritance` so `methods[class]` already includes inherited methods).**
1. Compose each interface's transitive `imethods` (base-first), memoized like `compose_vtable`.
2. Compute `N = max over ALL ids of vimpls[c].len()`; assign `iface_slot_base` globally per §4 (walk interfaces in id order from cursor `N`).
3. For each **class** id, for each `iface in iface_bases[class]`, for each `(name, isig)` in `imethods[iface]` at index `k`: resolve the impl symbol —
   1. `explicit_impls[(class, iface, name)]` if present; else
   2. `pick_overload(methods[class].get(name), isig.params[1..], /*members=*/true)` (this includes **inherited** class methods, since `apply_inheritance` ran first); else
   3. the interface default `idefaults[iface][k]`; else
   4. an empty-string placeholder (→ null slot) **plus a sema diagnostic** "class C does not implement I.name".
   - **ABI assertion:** before wiring a chosen impl, assert its non-pointer param/return IR types equal `isig`'s (pointer params are ABI-identical and may differ in nominal id). On mismatch, emit a diagnostic and write the null placeholder rather than a type-incompatible `call_indirect` target. Grow `vimpls[class]` to `iface_slot_base[iface] + k + 1` (filling gaps with empty strings) and write the symbol at `iface_slot_base[iface] + k`.
4. After all classes are composed, each `vimpls[class]` is long enough for every slot it indexes; no global-uniform padding.

**lower.rs — `struct_base` (4955–4974) (T4).** Add, in the `Some((place, IrType::Ref(id)))` arm (4958) and the non-lvalue `Ref` arm (4967), no change to the load itself — the load already produces the body pointer and returns `(body, id)` regardless of whether `id` is a class or interface. The arm already works for an interface `Ref`; **the only requirement is that interface ids now reach here** (guaranteed once `lower_ty_env` returns `Ref(iface_id)`, which follows from T1 registration). No code change beyond confirming the `Ref` arm is not gated on class-ness. (The `Some(_) => None` arm at 4962 is for non-pointer lvalues; an interface lvalue is `Ref`, so it never lands there.)

**lower.rs — interface dispatch (`lower_method_call`, a SEPARATE branch BEFORE the methods-keyed block at 5854) (T5).**
```rust
// Interface dispatch: the receiver's static type is an interface. The slot is
// globally fixed for (interface, method), so the concrete class is not needed.
if let Some((body_ptr, owner_id)) = self.struct_base(base, src)
    && matches!(self.structs.kinds[owner_id.0 as usize], StructKind::Interface)
    && let Some(midx) = self.structs.imethods[owner_id.0 as usize]
        .iter().position(|(n, _)| n == mname)
{
    let sig = self.structs.imethods[owner_id.0 as usize][midx].1.clone();
    let base_slot = self.structs.iface_slot_base[&owner_id];
    // header at offset 0 via a raw pointer-indexed GEP (interfaces have no
    // $header field, so use elem_addr from body_ptr, NOT field_addr):
    let hdr = self.fb.elem_addr(body_ptr.clone(), IrType::Ptr, Value::int(0, IrType::I64));
    let vtbl = self.fb.load(hdr, IrType::Ptr);
    let slotp = self.fb.elem_addr(vtbl, IrType::Ptr,
        Value::int((base_slot + midx) as i128, IrType::I64));
    let fnptr = self.fb.load(slotp, IrType::Ptr);
    let mut call_args = vec![body_ptr];
    let mut pidx = 1;
    for (v, t) in arg_vals { let pt = sig.params.get(pidx).copied().unwrap_or(t);
        call_args.push(self.coerce(v, t, pt)); pidx += 1; }
    let r = self.fb.call_indirect(fnptr, call_args, sig.ret);
    return (r, sig.ret);
}
```
This branch sources its `sig` from `imethods` (the methods-keyed block at 5854 would never fire for an abstract interface method). `struct_base` is called once here; the existing block at 5854 calls it again for the non-interface path (cheap, and avoids reordering the existing code). The header GEP uses `elem_addr(body_ptr, Ptr, 0)` — a raw pointer-indexed load — so it works even though interface ids have an empty `StructDef`.

**lower.rs — `lower_ty_env`.** Already correct once interfaces are in `by_name`: `ty_of(name)` returns `Ref(iface_id)`. A *generic* interface name (`IFaceD<int16>`) stays `Ptr` (unregistered) — out of scope; that call falls through to the current undef fallback, which verifies (§6/§10).

**lower.rs — upcast.** Assigning/returning/storing a `Square` where an `IShape` is expected flows through `coerce(v, Ref(square_id), Ref(iface_id))`, which is **already** a no-op (6128). **Do not add the gated `Ref(class)→Ref(iface)` arm the draft proposed — it is dead code.** Explicit `(IFaceA)expr` cast: the existing pointer-reinterpret cast path already yields the value unchanged for a `Ref→Ref` cast; confirm in T4 (no new code expected).

**lower.rs — `is`/`as` (T7).**
- `type_id_of` already returns interface ids via `by_name` — no change.
- `type_test` (5625–5647): (a) **read the header via the raw GEP** `elem_addr(obj, IrType::Ptr, 0)` + `load Ptr`, **not** `field_addr(obj, oid, 0)`, so it works when the **source** value is interface-typed (an empty-StructDef `oid`). (b) When `tid` is an **interface**, change the target filter from `is_subtype_of(c, tid)` to `self.structs.iface_bases[c].contains(&tid) && !vimpls[c].is_empty()` (using the transitively-flattened `iface_bases`, so a class implementing only `IB : IA` is found when testing `is IA`). Keep the class-subtype path for class `tid`.
- `lower_is`/`lower_as` already gate on `Ref(oid)` (works for class or interface sources). `as IFace` returns `Ref(iface_id)` (pointer-like; the typed-null `select` verifies).

**lower.rs — default interface method bodies (T6).** Emit each default-bodied interface method as a real free function `{IFace.prefix}{Method}` with `this : Ref(iface_id)`, through the existing `lower_method` path. **The `this_ty` passed must be `Ref(iface_id)` and the methods table in scope must let an unqualified sibling call inside the default body resolve to interface dispatch on `this`** — i.e. an unqualified `FuncA(...)` inside `IFaceB.FuncB` must dispatch through the same interface vtable (FuncA being abstract in `IFaceA`, base of `IFaceB`), not a direct call. Reconcile the emitted symbol with the symbol `apply_itables` writes into the slot (both `{IFace.prefix}{Method}`) so the slot resolves to a real function, and avoid a double-emit if `lower_type_at` already emits bodied interface members (assert the slot symbol resolves, not null).

**newbf-llvm.** **No change.** No new instruction; `emit_vtables` already nulls unresolved entries (228) and arrays are independently sized. Longer vtables are plain constant arrays. `GlobalAddr`/`CallIndirect`/`ElemAddr`/`Load` already exist.

**SSA-dominance correctness.** The dispatch sequence (header-GEP → load → slot-GEP → fnptr-load → call_indirect) is emitted **inline at the call site in a single basic block**, exactly like the existing virtual call, so every value dominates its use trivially. Upcast is a no-op (no new value). `is`/`as` reuse the verified `type_test`/`select` shape. No new phi, no new block — the §83 "dominate all uses" trap is avoided by construction.

## 6. Interactions

- **Class vtables (§27/§30):** unchanged in shape; interface slots are *appended* after the class virtual slots. A method that is both an `override` (class vslot) and an interface impl occupies its class slot **and** is referenced (same symbol) from the interface slot — pinned by a dedicated program (§8).
- **Monomorphization (§13/§46):** the generic-constraint path is the dominant case and **must not regress**. `Use<Holder>` resolves `val.Get()` directly on `Holder`; that path never consults `iface_bases`/itable slots. `N` (the global vtable max) iterates **all** ids `0..defs.len()` including monomorphized generic classes, so `iface_slot_base` is stable across the whole table. Interfaces are not monomorphized in v1.
- **Generic interface-typed values:** `IFaceD<int16> ifd = cd; ifd.GetVal()` (Interfaces.bf:462) — the generic interface is unregistered, so `lower_ty_env` returns `Ptr`, `struct_base` returns `None`, and the call stays on the **undef fallback**. The verify corpus stays green for Interfaces.bf because the Ptr-fallback path is **valid IR**, not because those sites dispatch. This is explicitly v1 behavior; `imethods` skips generic interface methods.
- **Value-struct → interface (boxing):** out of scope. A value struct listing an interface base (`struct StructA : IFaceB`, Interfaces.bf:34) must **not** enter `iface_bases` and gets no itable slots; its interface uses stay on the generic-constraint static path (already works). `apply_itables` tolerates such a struct without panicking (it is skipped).
- **`delete` through an interface value:** no `$dtor` slot exists in `imethods` in v1. `delete` on an interface-typed value falls through to current (likely-wrong) behavior; v1 test programs only `delete` concrete refs. (A guard/diagnostic is a follow-on.)
- **Target-typing:** `IShape s = sq;` is a typed local-init; the target `Ref(iface_id)` is known and the RHS `Ref(square_id)` coerces by identity. No target-typed constructor applies (you can't `new IShape()`); the `try_target_typed_*` chain falls through to `expr` + coerce, which is correct.
- **Comptime:** comptime lowers the same IR; interface dispatch is a normal `call_indirect` through a vtable global. The MEMORY note (JIT FP-constant-pool limit) does not apply (no float constants).
- **AOT:** vtable globals and `call_indirect` already AOT-link. Longer vtables with null placeholders are plain constant arrays. The 8-bit exit-code caveat means value-checks > 255 use the JIT run-corpus harness.
- **Cross-file simple-name interface collisions:** `by_name` is keyed on the simple name, first-wins. `interface IFaceA` exists in both Interfaces.bf and Reflection.bf; one wins the id. This is a pre-existing class-level ambiguity that interface dispatch newly *depends* on (`imethods[iface]` must be the right interface). Out of scope to fully fix (qualified naming is a bigger change); v1 mitigates by keeping all **new** interface fixtures' names globally unique. Noted for follow-on.

## 7. Risks & mitigations

- **Dispatch placed where it can never run.** *Mitigation:* the interface-dispatch block is a **separate top-level branch before** the methods-keyed guard at 5854, driven by `imethods` (not `methods`), gated on `kinds[owner_id]==Interface`. Pinned behaviorally by T5 programs (undef would return garbage, not the expected value).
- **T1 corrupting inheritance via interface bases.** *Mitigation:* the base-routing guard (`matches!(kinds[bid], Ref)`) ships **atomically in T1**; T1's acceptance names Interfaces.bf as an explicit regression, not just the aggregate count.
- **Non-exhaustive `match StructKind`.** *Mitigation:* T1 adds the `Interface` arm to `ty_of` and audits every `match`/`matches!` site (compile error otherwise — caught immediately).
- **Runtime out-of-bounds vtable index.** *Mitigation:* `iface_slot_base[I] = N + cumulative` with `N = max over ALL ids`; `debug_assert!(iface_slot_base[I] >= vimpls[c].len())`; each `vimpls[class]` grown to cover its highest used slot; gaps null-filled.
- **ABI mismatch between interface sig and concrete impl.** *Mitigation:* `apply_itables` asserts non-pointer param/return types match (pointers are ABI-identical); on mismatch, a sema diagnostic + null slot rather than a wrong-typed `call_indirect`.
- **Explicit-impl resolution had nothing to key on.** *Mitigation:* `explicit_impls[(class, iface, name)]` is populated by reading `explicit_iface` in registration and consulted first in `apply_itables`.
- **Default-method dispatch picking the wrong body.** *Mitigation:* defaults are not in `methods[iface]`; a class reaches a default only through its itable slot, so an override wins. Sibling unqualified calls inside a default body dispatch through `this`'s interface vtable.
- **`type_test` invalid GEP for an interface source.** *Mitigation:* `type_test` reads the header via the raw `elem_addr(obj, Ptr, 0)`, never `field_addr` through an empty interface StructDef.
- **`coerce` dead-code arm.** *Mitigation:* do **not** add the proposed `Ref(class)→Ref(iface)` arm; 6128 already covers it.
- **sema must not depend on newbf-llvm.** *Mitigation:* every change is in newbf-sema + newbf-ir; newbf-llvm needs **no** change.
- **Regressing the generic-constraint path.** *Mitigation:* `interface_constraint.bf → 100` and Interfaces.bf in the verify corpus must stay green at every task boundary.

## 8. Testing strategy

**Gates that must stay green at every task boundary:** verify corpus 152/152, parser corpus 152/152, run-corpus ~160. Each new run-corpus program is single-file and inline-defines its own interfaces/classes (no corlib interface dependency); `malloc`/`free` resolve as for existing `new`-using programs.

**New run-corpus programs** (`e:/NewBF/beef-tests/run-corpus/`, each `Program.Main → int32`, `// expect:`):

1. `iface_dispatch_basic.bf` — `expect: 9`. The §1 example; the implementer has **no `virtual` method** (so it pins the interface-only-class header/vtable-emission path).
2. `iface_dispatch_param.bf` — `expect: 42`. `int32 F(IShape s){return s.Area();}` summed over two implementers (areas 9 and 33).
3. `iface_dispatch_polymorphic.bf` — `expect: 1`. Locals of `IShape` holding different concrete types; one call site dispatches to different impls.
4. `iface_multi.bf` — `expect: 7`. A class implementing `IA`/`IB`; call through each interface-typed view of the same object.
5. `iface_field_return.bf` — `expect: 12`. Store an interface value in a class field and return an interface from a method; dispatch through both.
6. `iface_vtable_coexist.bf` — `expect: 3`. A class with a real `virtual` method **and** a distinct interface impl; call both, prove neither clobbers the other.
7. `iface_virtual_is_impl.bf` — `expect: 4`. One method that is **both** `virtual`/`override` (class slot) **and** satisfies an interface (interface slot) — the same symbol in two slots; call both ways.
8. `iface_inherited_impl.bf` — `expect: 5`. `class C : Base, IFace` where `IFace.M` is satisfied **purely by an inherited `Base.M`** (pins the post-`apply_inheritance` ordering).
9. `iface_default_method.bf` — `expect: 100`. Default-body `int32 D(){return 100;}`; a class that does not override it; call `((I)obj).D()`.
10. `iface_default_calls_sibling.bf` — `expect: 30`. A default body that calls a **sibling abstract** interface method through `this` (e.g. `D(){ return A()*3; }` with `A()` abstract returning 10) — pins the hardest part of T6.
11. `iface_default_override.bf` — `expect: 7`. Same default, class overrides it to 7; dispatch picks the override.
12. `iface_is_as.bf` — `expect: 1`. `obj is IShape` true/false; `obj as IShape` non-null then dispatches; include a case where the `is` **source is itself interface-typed** (`IShape s; if (s is Square) …`).
13. `iface_inherit.bf` — `expect: 5`. `interface IB : IA`; call an `IA` method through an `IB`-typed value (slot from the base-interface block).

**Verify-corpus pin:** add a focused `.bf` mirroring program 3 to the verify corpus (T8) to pin IR shape; existing `Interfaces.bf` must still lower clean throughout.

**Dump-IR assertions (hard gates where data is produced, not four tasks later):**
- T2: assert `iface_bases[Square] == [IShape]` and `imethods[IShape] == [("Area", _)]` (and that a value struct listing an interface base has empty `iface_bases`).
- T3: dump-ir test that `Square$vtable` contains the `Area` impl symbol at `iface_slot_base[IShape]`; that an interface-only class gets a `Class$vtable` global emitted; that an inherited-impl class (program 8 fixture) resolves to the base symbol.

Each task below lists the *single* behavioral test that gates it; a task lands only when all prior gates plus its new test/assertion are green.

## 9. Task breakdown (ordered, agent-assignable)

**T1 — Register interfaces as `StructKind::Interface` (+ base-routing guard).**
*Scope:* lower.rs — add `Interface` to `StructKind` (36–40); `struct_kind` arm (42–48); `ty_of` arm (227); audit/fix every `match`/`matches!` on `StructKind`; register interfaces in `register_type_struct` (932–964, empty `StructDef`, push per-id `Vec` slots); add the five new `StructTable` fields (`#[derive(Default)]`); **guard the base-recording loop at 1479 with `matches!(kinds[bid], StructKind::Ref)`**.
*Deps:* none.
*Acceptance:* verify **152/152** (Interfaces.bf named explicitly as a regression: interface types now lower as `Ref`, an interface receiver still falls to the existing undef fallback without ill-typed IR), parser 152/152, run-corpus ~160, `interface_constraint.bf → 100`. **Not** "no behavior change" — the type-flip lands here, made safe by the base guard.

**T2 — Capture interface bases; populate `imethods`/`idefaults`/`explicit_impls`.**
*Scope:* lower.rs — `fill_iface_members` (record every **instance, non-generic** interface method into `imethods`, filtering out static/generic; `idefaults` per slot; abstract methods recorded despite the body-less skip; **defaults NOT added to `methods[iface]`**); `collect_iface_bases` (route class interface bases into transitively-flattened `iface_bases`; skip value structs and interfaces); read `explicit_iface` in the `Member::Method` arm into `explicit_impls`.
*Deps:* T1.
*Acceptance:* all three gates green; the T2 dump-ir assertions (`iface_bases[Square]`, `imethods[IShape]`, empty `iface_bases` for a value struct). Still no dispatch.

**T3 — Compose itables into class vtables (`apply_itables`, `iface_slot_base`, bounds-safe padding, diagnostics).**
*Scope:* lower.rs — `apply_itables` called in `build` immediately after `apply_vtables` at 221 (so after `apply_inheritance`); compose transitive `imethods`; `N = max over ALL ids of vimpls.len()`; assign `iface_slot_base` globally; per-class impl resolution (explicit → `pick_overload` incl. inherited → default → null + diagnostic); ABI param/return assertion; grow `vimpls` to cover used slots with empty-string (null) gaps; `debug_assert` no overlap. **No newbf-llvm change; no `$abort`.**
*Deps:* T2.
*Acceptance:* verify 152/152; run-corpus green; the T3 dump-ir assertions (impl symbol at `iface_slot_base`, interface-only class gets a vtable global, inherited-impl resolves to base symbol). No call-site change yet — behavior unchanged.

**T4 — Interface-typed receivers reach `struct_base`; upcast confirmed free.**
*Scope:* lower.rs — confirm `struct_base`'s `Ref` arm (4958/4967) returns `(body, iface_id)` for an interface (no gating on class-ness; no code change expected beyond a comment); confirm `coerce` (6128) already makes `Ref(class)→Ref(iface)` a no-op (**delete** the draft's proposed gated arm idea); confirm `(IFaceA)expr` reinterprets unchanged.
*Deps:* T3.
*Acceptance:* gates green; an interface-typed local/param resolves to `Ref(iface_id)` (verify corpus clean). With no dispatch branch yet, an interface receiver still reaches the methods-keyed block, finds nothing (abstract methods absent from `methods`, defaults deliberately absent), and returns undef — i.e. **no new wrong-direct-call is possible** because defaults were excluded from `methods[iface]` in T2.

**T5 — Itable dispatch at the call site + first run-corpus programs.**
*Scope:* lower.rs — the interface-dispatch **separate branch before 5854** (sourcing `sig` from `imethods`, raw `elem_addr(body_ptr, Ptr, 0)` header GEP); add programs 1–8 (`iface_dispatch_basic`, `_param`, `_polymorphic`, `iface_multi`, `iface_field_return`, `iface_vtable_coexist`, `iface_virtual_is_impl`, `iface_inherited_impl`).
*Deps:* T4.
*Acceptance:* all eight new programs pass under the JIT run-corpus harness with their expected values; verify 152/152; `interface_constraint.bf → 100`. This is the **minimal-but-correct first slice** (abstract methods, multi-interface, fields/returns, vtable coexistence incl. same-symbol-two-slots, inherited impl).

**T6 — Default interface methods.**
*Scope:* lower.rs — emit default-bodied interface methods as free functions `{IFace.prefix}{Method}` with `this : Ref(iface_id)` via `lower_method`; ensure a sibling unqualified call inside a default dispatches through `this`'s interface vtable; wire `idefaults` into `apply_itables`; reconcile the emitted symbol with the slot symbol (no double-emit, slot resolves non-null); add programs 9–11 (`iface_default_method`, `iface_default_calls_sibling`, `iface_default_override`).
*Deps:* T5.
*Acceptance:* all three pass (100 / 30 / 7); all gates green.

**T7 — `is`/`as`/inheritance against interfaces.**
*Scope:* lower.rs — `type_test` reads header via raw `elem_addr` (not `field_addr`); interface-`tid` target set = `iface_bases[c].contains(tid) && !vimpls[c].is_empty()`; keep class-`tid` path; confirm transitive flattening in `iface_bases`; add programs 12–13 (`iface_is_as` incl. interface-typed source, `iface_inherit`).
*Deps:* T6.
*Acceptance:* both pass (1 / 5); all gates green.

**T8 — Journal + verify-corpus pin + doc cross-link.**
*Scope:* `docs/journals/2026-05-31.md` (new numbered §: design + outcome); add the polymorphic fixture to the verify corpus (increment the count); cross-link this design doc.
*Deps:* T7.
*Acceptance:* journal entry present; verify corpus count incremented and green; commit pairs with the entry (conventional style + Co-Authored-By trailer).

T1–T4 are plumbing (T1 carries the type-flip + base guard; T4 is a confirm-and-delete-dead-code task with dump-ir/IR-shape checks). T5–T7 each add behavior plus their own pinning programs.

## 10. Open questions / decisions deferred

- **Boxing value structs to interfaces (`StructA sa; IFaceB ib = sa;`).** Out of scope for v1: a value struct has no `$header`/vtable. Interfaces.bf uses only the generic-constraint form for value structs, so the corpus has no dynamic value-struct-upcast site. Decision: such a struct never enters `iface_bases`; the upcast falls through to current behavior.
- **Generic interfaces / generic interface methods.** Out of scope. They stay on the generic-constraint static path; `imethods` skips generic interface methods; `IFaceD<int16> ifd` stays `Ptr` (verify-clean fallback, not behavioral dispatch). Monomorphizing interface types is a follow-on once §13's machinery extends to interfaces.
- **Default methods via the generic-constraint path.** v1 reaches defaults only through itable slots (not `methods[iface]`), to avoid wrong-body direct calls. Letting `val.DefaultM()` resolve to a default under `T : IFace` is a follow-on (needs ordering care so an override still wins).
- **`concrete IFaceA GetConcreteIA()` (Interfaces.bf:31).** The `concrete` interface-return modifier is a Beef optimization; treated as a plain interface return in v1.
- **`GetType()`/reflection on interface values.** Depends on reflection (REFLECTION.md); orthogonal. v1 leaves `GetType` on its current fallback.
- **`delete` through an interface-typed value.** No `$dtor` in `imethods` in v1; falls through to current behavior. A guard/diagnostic is a follow-on.
- **Cross-file simple-name interface collisions** (`IFaceA` in two files). Pre-existing `by_name` first-wins ambiguity; v1 keeps new fixtures' names unique. Qualified naming is a separate change.
- **Unimplemented-required-slot policy.** v1 writes a null placeholder (clean segfault if called) plus a sema diagnostic at composition. A `$abort` trap is a possible later debuggability improvement but is **not** needed (and `emit_vtables` already nulls unresolved entries).
- **Vtable-length scheme.** v1 uses a global per-interface base with per-class bounds-safe growth (no global-uniform padding). Per-interface reserved blocks that skip unimplemented interfaces, or full Beef-style per-class offsets (needs a runtime type-data table), are deferred to a size/perf follow-on.
