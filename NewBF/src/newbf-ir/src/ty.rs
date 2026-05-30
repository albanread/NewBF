//! The IR type system: the concrete, post-monomorphization types that
//! flow through the typed SSA IR. Deliberately close to LLVM's model
//! (opaque pointers, sized integers/floats) so lowering is mechanical —
//! aggregate/struct/array types arrive with the layout sprint.

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

    /// Bit width of a scalar type (`Void`/`Ptr` report 0 / pointer width).
    pub fn bit_width(self) -> u16 {
        match self {
            IrType::Void => 0,
            IrType::Bool => 1,
            IrType::Int { bits, .. } | IrType::Float { bits } => bits,
            IrType::Ptr => 64,
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
        }
    }
}
