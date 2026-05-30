//! Functions, basic blocks, and an ergonomic builder.

use newbf_lexer::Span;

use crate::inst::*;
use crate::ty::IrType;

/// A function parameter. `name` is for the report only; operands reference
/// parameters positionally as [`Value::Param`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Param {
    pub name: Option<String>,
    pub ty: IrType,
}

/// A basic block: a label, a straight-line list of instructions (by id),
/// and exactly one terminator.
#[derive(Clone, PartialEq, Debug)]
pub struct Block {
    pub label: String,
    pub insts: Vec<InstId>,
    pub term: Terminator,
}

/// A function: signature, parameters, basic blocks, and a flat instruction
/// arena the blocks index into. `is_extern` marks a body-less declaration
/// (FFI import / not-yet-lowered).
#[derive(Clone, PartialEq, Debug)]
pub struct Function {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: IrType,
    pub blocks: Vec<Block>,
    pub insts: Vec<InstData>,
    pub is_extern: bool,
}

impl Function {
    pub fn inst(&self, id: InstId) -> &InstData {
        &self.insts[id.0 as usize]
    }
    pub fn block(&self, id: BlockId) -> &Block {
        &self.blocks[id.0 as usize]
    }
}

/// Builds one [`Function`]. Starts with an `entry` block selected; emit
/// methods append to the current block and return the result [`Value`];
/// terminator methods close the current block.
pub struct FunctionBuilder {
    name: String,
    params: Vec<Param>,
    ret: IrType,
    blocks: Vec<Block>,
    insts: Vec<InstData>,
    current: BlockId,
}

impl FunctionBuilder {
    pub fn new(name: impl Into<String>, params: Vec<Param>, ret: IrType) -> Self {
        let entry = Block {
            label: "entry".to_string(),
            insts: Vec::new(),
            term: Terminator::Unreachable,
        };
        Self {
            name: name.into(),
            params,
            ret,
            blocks: vec![entry],
            insts: Vec::new(),
            current: BlockId(0),
        }
    }

    pub fn entry(&self) -> BlockId {
        BlockId(0)
    }

    pub fn param(&self, i: u32) -> Value {
        Value::Param(i)
    }

    pub fn create_block(&mut self, label: impl Into<String>) -> BlockId {
        let id = BlockId(self.blocks.len() as u32);
        self.blocks.push(Block {
            label: label.into(),
            insts: Vec::new(),
            term: Terminator::Unreachable,
        });
        id
    }

    pub fn switch_to(&mut self, block: BlockId) {
        self.current = block;
    }

    pub fn current_block(&self) -> BlockId {
        self.current
    }

    // ── raw emit ────────────────────────────────────────────────────────

    fn emit(&mut self, kind: InstKind, ty: IrType, span: Option<Span>) -> Value {
        let id = InstId(self.insts.len() as u32);
        self.insts.push(InstData { kind, ty, span });
        self.blocks[self.current.0 as usize].insts.push(id);
        Value::Inst(id)
    }

    // ── instruction helpers ───────────────────────────────────────────────

    pub fn bin(&mut self, op: BinOp, lhs: Value, rhs: Value, ty: IrType) -> Value {
        self.emit(InstKind::Bin { op, lhs, rhs }, ty, None)
    }

    pub fn cmp(&mut self, pred: CmpPred, lhs: Value, rhs: Value) -> Value {
        self.emit(InstKind::Cmp { pred, lhs, rhs }, IrType::Bool, None)
    }

    pub fn cast(&mut self, kind: CastKind, val: Value, to: IrType) -> Value {
        self.emit(InstKind::Cast { kind, val }, to, None)
    }

    /// Allocates a stack slot for `elem`; result is a `ptr`.
    pub fn alloca(&mut self, elem: IrType) -> Value {
        self.emit(InstKind::Alloca { elem }, IrType::Ptr, None)
    }

    pub fn load(&mut self, ptr: Value, ty: IrType) -> Value {
        self.emit(InstKind::Load { ptr }, ty, None)
    }

    pub fn store(&mut self, ptr: Value, val: Value) {
        self.emit(InstKind::Store { ptr, val }, IrType::Void, None);
    }

    pub fn call(&mut self, name: impl Into<String>, args: Vec<Value>, ret: IrType) -> Value {
        self.emit(
            InstKind::Call {
                callee: Callee { name: name.into() },
                args,
            },
            ret,
            None,
        )
    }

    pub fn phi(&mut self, incomings: Vec<(BlockId, Value)>, ty: IrType) -> Value {
        self.emit(InstKind::Phi { incomings }, ty, None)
    }

    pub fn select(&mut self, cond: Value, a: Value, b: Value, ty: IrType) -> Value {
        self.emit(InstKind::Select { cond, a, b }, ty, None)
    }

    /// Attach a source span to the most recently emitted instruction (for
    /// debug info / symbolicated stack dumps).
    pub fn set_span(&mut self, value: &Value, span: Span) {
        if let Value::Inst(id) = value {
            self.insts[id.0 as usize].span = Some(span);
        }
    }

    // ── terminators (close the current block) ─────────────────────────────

    pub fn ret(&mut self, val: Option<Value>) {
        self.blocks[self.current.0 as usize].term = Terminator::Ret(val);
    }

    pub fn br(&mut self, target: BlockId) {
        self.blocks[self.current.0 as usize].term = Terminator::Br(target);
    }

    pub fn cond_br(&mut self, cond: Value, then: BlockId, els: BlockId) {
        self.blocks[self.current.0 as usize].term = Terminator::CondBr { cond, then, els };
    }

    pub fn finish(self) -> Function {
        Function {
            name: self.name,
            params: self.params,
            ret: self.ret,
            blocks: self.blocks,
            insts: self.insts,
            is_extern: false,
        }
    }
}
