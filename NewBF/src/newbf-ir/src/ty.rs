//! The IR type system: the concrete, post-monomorphization types that
//! flow through the typed SSA IR. Deliberately close to LLVM's model
//! (opaque pointers, sized integers/floats) so lowering is mechanical —
//! aggregate/struct/array types arrive with the layout sprint.

/// Index into a [`crate::Module`]'s struct table (`module.structs`). Kept a
/// plain `u32` so [`IrType`] stays `Copy`; the field layout lives in the
/// module's [`crate::StructDef`], not on the type.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct StructId(pub u32);

/// A concrete IR type. Every [`crate::Value`] has one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum IrType {
    /// No value (the result type of `store`, `ret void`, void calls).
    Void,
    /// `i1` — the result of comparisons and the type of `bool`.
    Bool,
    /// A sized integer. `signed` records Beef's signedness for op selection
    /// (LLVM integers are sign-agnostic; we pick `sdiv`/`udiv` etc. from it).
    Int { bits: u16, signed: bool },
    /// `f32` / `f64`.
    Float { bits: u16 },
    /// An opaque pointer (LLVM-22 opaque-pointer model). The pointee type
    /// lives on the instruction that uses it (`alloca` elem, `load` result),
    /// never on the pointer itself.
    Ptr,
    /// An aggregate (value struct / heap object body) type, indexing the
    /// module's struct table. Used as an `alloca`'s element type and the base
    /// type of a `fieldaddr`; the field list lives in [`crate::StructDef`].
    Struct(StructId),
    /// A typed reference: a pointer to a heap object whose body layout is the
    /// struct `id` (a class instance). Lowers to a plain `ptr`, but carries the
    /// nominal class so member access can `fieldaddr` through the body.
    Ref(StructId),
}

impl IrType {
    /// Common width helpers (Beef: `int`/`uint` are pointer-width = 64).
    pub const I1: IrType = IrType::Bool;
    pub const I8: IrType = IrType::Int {
        bits: 8,
        signed: true,
    };
    pub const U8: IrType = IrType::Int {
        bits: 8,
        signed: false,
    };
    pub const I16: IrType = IrType::Int {
        bits: 16,
        signed: true,
    };
    pub const I32: IrType = IrType::Int {
        bits: 32,
        signed: true,
    };
    pub const U32: IrType = IrType::Int {
        bits: 32,
        signed: false,
    };
    pub const I64: IrType = IrType::Int {
        bits: 64,
        signed: true,
    };
    pub const U64: IrType = IrType::Int {
        bits: 64,
        signed: false,
    };
    pub const F32: IrType = IrType::Float { bits: 32 };
    pub const F64: IrType = IrType::Float { bits: 64 };

    pub fn is_int(self) -> bool {
        matches!(self, IrType::Int { .. } | IrType::Bool)
    }

    pub fn is_float(self) -> bool {
        matches!(self, IrType::Float { .. })
    }

    pub fn is_signed(self) -> bool {
        matches!(self, IrType::Int { signed: true, .. })
    }

    /// Pointer-like: an opaque `Ptr` or a typed `Ref`. Both are LLVM `ptr`, so
    /// coercion treats them interchangeably.
    pub fn is_pointer(self) -> bool {
        matches!(self, IrType::Ptr | IrType::Ref(_))
    }

    /// Bit width of a scalar type (`Void`/`Ptr` report 0 / pointer width).
    pub fn bit_width(self) -> u16 {
        match self {
            IrType::Void => 0,
            IrType::Bool => 1,
            IrType::Int { bits, .. } | IrType::Float { bits } => bits,
            IrType::Ptr | IrType::Ref(_) => 64,
            // Aggregates have no single scalar width; scalar coercion never
            // reaches here for a struct.
            IrType::Struct(_) => 0,
        }
    }

    /// The LLVM-style type mnemonic used in the `dump-ir` report.
    pub fn mnemonic(self) -> String {
        match self {
            IrType::Void => "void".to_string(),
            IrType::Bool => "i1".to_string(),
            IrType::Int { bits, .. } => format!("i{bits}"),
            IrType::Float { bits } => format!("f{bits}"),
            IrType::Ptr => "ptr".to_string(),
            IrType::Struct(id) => format!("%s{}", id.0),
            IrType::Ref(id) => format!("&s{}", id.0),
        }
    }
}
