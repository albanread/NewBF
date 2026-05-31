//! Values, constants, operations, instructions, and terminators.

use newbf_lexer::Span;

use crate::ty::{IrType, StructId};

/// Index of an instruction within a [`crate::Function`]'s instruction arena.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct InstId(pub u32);

/// Index of a basic block within a [`crate::Function`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BlockId(pub u32);

/// An operand: an instruction result, a function parameter, or an inline
/// constant. (SSA: every instruction result is referenced as `Inst`.)
#[derive(Clone, PartialEq, Debug)]
pub enum Value {
    Inst(InstId),
    Param(u32),
    Const(Const),
}

impl Value {
    pub fn int(v: i128, ty: IrType) -> Value {
        Value::Const(Const::Int(v, ty))
    }
    pub fn float(v: f64, ty: IrType) -> Value {
        Value::Const(Const::Float(v, ty))
    }
    pub fn bool(v: bool) -> Value {
        Value::Const(Const::Bool(v))
    }
    pub fn str(s: impl Into<String>) -> Value {
        Value::Const(Const::Str(s.into()))
    }
}

/// An inline constant.
#[derive(Clone, PartialEq, Debug)]
pub enum Const {
    Int(i128, IrType),
    Float(f64, IrType),
    Bool(bool),
    /// A null pointer.
    Null,
    /// An undefined value of the given type (uninitialized `?`).
    Undef(IrType),
    /// A string literal. Lowers to a private, NUL-terminated `[N x i8]`
    /// constant global; the value is a `ptr` to its first byte (a C `char*`).
    Str(String),
}

/// Binary arithmetic/bitwise operators. Signed vs. unsigned division and
/// shifts are distinct ops (selected from operand signedness during
/// lowering); float ops are the `F*` variants.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    SDiv,
    UDiv,
    SRem,
    URem,
    And,
    Or,
    Xor,
    Shl,
    LShr,
    AShr,
    FAdd,
    FSub,
    FMul,
    FDiv,
    FRem,
}

impl BinOp {
    pub fn mnemonic(self) -> &'static str {
        match self {
            BinOp::Add => "add",
            BinOp::Sub => "sub",
            BinOp::Mul => "mul",
            BinOp::SDiv => "sdiv",
            BinOp::UDiv => "udiv",
            BinOp::SRem => "srem",
            BinOp::URem => "urem",
            BinOp::And => "and",
            BinOp::Or => "or",
            BinOp::Xor => "xor",
            BinOp::Shl => "shl",
            BinOp::LShr => "lshr",
            BinOp::AShr => "ashr",
            BinOp::FAdd => "fadd",
            BinOp::FSub => "fsub",
            BinOp::FMul => "fmul",
            BinOp::FDiv => "fdiv",
            BinOp::FRem => "frem",
        }
    }
}

/// Comparison predicates (integer `i*`/`s*`/`u*`, float `fo*`). Result is
/// always [`IrType::Bool`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CmpPred {
    Eq,
    Ne,
    Slt,
    Sle,
    Sgt,
    Sge,
    Ult,
    Ule,
    Ugt,
    Uge,
    FOeq,
    FOne,
    FOlt,
    FOle,
    FOgt,
    FOge,
}

impl CmpPred {
    pub fn mnemonic(self) -> &'static str {
        match self {
            CmpPred::Eq => "eq",
            CmpPred::Ne => "ne",
            CmpPred::Slt => "slt",
            CmpPred::Sle => "sle",
            CmpPred::Sgt => "sgt",
            CmpPred::Sge => "sge",
            CmpPred::Ult => "ult",
            CmpPred::Ule => "ule",
            CmpPred::Ugt => "ugt",
            CmpPred::Uge => "uge",
            CmpPred::FOeq => "foeq",
            CmpPred::FOne => "fone",
            CmpPred::FOlt => "folt",
            CmpPred::FOle => "fole",
            CmpPred::FOgt => "fogt",
            CmpPred::FOge => "foge",
        }
    }

    pub fn is_float(self) -> bool {
        matches!(
            self,
            CmpPred::FOeq
                | CmpPred::FOne
                | CmpPred::FOlt
                | CmpPred::FOle
                | CmpPred::FOgt
                | CmpPred::FOge
        )
    }
}

/// A value-converting cast.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CastKind {
    Trunc,
    ZExt,
    SExt,
    FpTrunc,
    FpExt,
    FpToSi,
    FpToUi,
    SiToFp,
    UiToFp,
    Bitcast,
    IntToPtr,
    PtrToInt,
}

impl CastKind {
    pub fn mnemonic(self) -> &'static str {
        match self {
            CastKind::Trunc => "trunc",
            CastKind::ZExt => "zext",
            CastKind::SExt => "sext",
            CastKind::FpTrunc => "fptrunc",
            CastKind::FpExt => "fpext",
            CastKind::FpToSi => "fptosi",
            CastKind::FpToUi => "fptoui",
            CastKind::SiToFp => "sitofp",
            CastKind::UiToFp => "uitofp",
            CastKind::Bitcast => "bitcast",
            CastKind::IntToPtr => "inttoptr",
            CastKind::PtrToInt => "ptrtoint",
        }
    }
}

/// What a [`Call`](InstKind::Call) targets. Module-local calls and external
/// (FFI / not-yet-lowered) calls are both by name at this phase.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Callee {
    pub name: String,
}

/// The operation an instruction performs. The instruction's *result type*
/// lives on [`InstData::ty`].
#[derive(Clone, PartialEq, Debug)]
pub enum InstKind {
    Bin {
        op: BinOp,
        lhs: Value,
        rhs: Value,
    },
    Cmp {
        pred: CmpPred,
        lhs: Value,
        rhs: Value,
    },
    Cast {
        kind: CastKind,
        val: Value,
    },
    /// Stack slot for an addressable local; result is a `ptr` to `elem`.
    Alloca {
        elem: IrType,
    },
    Load {
        ptr: Value,
    },
    /// Result type is `Void`.
    Store {
        ptr: Value,
        val: Value,
    },
    /// Address of field `field` within the `Struct(struct_id)` that `base`
    /// points to. Result is a `ptr` to the field — an LLVM struct GEP
    /// (`getelementptr %sN, ptr base, 0, field`).
    FieldAddr {
        base: Value,
        struct_id: StructId,
        field: u32,
    },
    Call {
        callee: Callee,
        args: Vec<Value>,
    },
    /// SSA merge: `[ (predecessor, value), … ]`.
    Phi {
        incomings: Vec<(BlockId, Value)>,
    },
    /// `cond ? a : b` without branching.
    Select {
        cond: Value,
        a: Value,
        b: Value,
    },
    /// A trap intrinsic. `debug: true` is a resumable breakpoint
    /// (`int3` / `@llvm.debugtrap`) — a Vectored/SEH handler can catch it,
    /// dump the stack, and continue. `debug: false` is a fatal illegal
    /// instruction (`ud2` / `@llvm.trap`) for `Runtime.FatalError`, failed
    /// asserts, and unreachable code. Result type is `Void`.
    Trap {
        debug: bool,
    },
}

/// One instruction: its operation, its result type, and an optional source
/// span (carried for debug info / symbolicated stack dumps).
#[derive(Clone, PartialEq, Debug)]
pub struct InstData {
    pub kind: InstKind,
    pub ty: IrType,
    pub span: Option<Span>,
}

impl InstData {
    /// An instruction yields no SSA value when its result type is `Void`
    /// (`store`, void `call`) — the printer skips numbering these.
    pub fn yields_value(&self) -> bool {
        self.ty != IrType::Void
    }
}

/// How a basic block ends. Every block has exactly one terminator.
#[derive(Clone, PartialEq, Debug)]
pub enum Terminator {
    Ret(Option<Value>),
    Br(BlockId),
    CondBr {
        cond: Value,
        then: BlockId,
        els: BlockId,
    },
    Unreachable,
}
