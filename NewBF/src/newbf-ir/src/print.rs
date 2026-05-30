//! `dump-ir` — the schema-stable textual report for the typed SSA IR.
//!
//! This is a human-reviewable report (LLVM-*flavoured* but not LLVM textual
//! IR — real LLVM emission goes through the API in the lowering sprint).
//! SSA values are numbered `%0..` (parameters first, then value-yielding
//! instruction results in emission order); `store`/void calls yield nothing.

use std::collections::HashMap;
use std::fmt::Write;

use crate::func::Function;
use crate::inst::*;
use crate::module::Module;
use crate::ty::IrType;

/// Render a whole module as the `dump-ir` report.
pub fn format_ir(m: &Module) -> String {
    let mut out = String::new();
    let defs = m.funcs.iter().filter(|f| !f.is_extern).count();
    let externs = m.funcs.len() - defs;
    let _ = writeln!(
        out,
        "ir module {:?}: {defs} functions, {externs} externs",
        m.name
    );
    for f in &m.funcs {
        out.push('\n');
        format_function(&mut out, f);
    }
    out
}

fn format_function(out: &mut String, f: &Function) {
    let params: Vec<String> = f
        .params
        .iter()
        .enumerate()
        .map(|(i, p)| format!("{} %{i}", p.ty.mnemonic()))
        .collect();
    let head = format!("@{}({}) -> {}", f.name, params.join(", "), f.ret.mnemonic());

    if f.is_extern {
        let _ = writeln!(out, "declare {head}");
        return;
    }

    // Number value-yielding results: params occupy 0..nparams.
    let mut num: HashMap<u32, usize> = HashMap::new();
    let mut next = f.params.len();
    for b in &f.blocks {
        for &id in &b.insts {
            if f.inst(id).yields_value() {
                num.insert(id.0, next);
                next += 1;
            }
        }
    }
    let p = Printer { f, num: &num };

    let _ = writeln!(out, "func {head} {{");
    for b in &f.blocks {
        let _ = writeln!(out, "{}:", b.label);
        for &id in &b.insts {
            let _ = writeln!(out, "    {}", p.inst(id));
        }
        let _ = writeln!(out, "    {}", p.terminator(&b.term));
    }
    let _ = writeln!(out, "}}");
}

struct Printer<'a> {
    f: &'a Function,
    num: &'a HashMap<u32, usize>,
}

impl Printer<'_> {
    fn value(&self, v: &Value) -> String {
        match v {
            Value::Param(i) => format!("%{i}"),
            Value::Inst(id) => match self.num.get(&id.0) {
                Some(n) => format!("%{n}"),
                None => "%<void>".to_string(),
            },
            Value::Const(c) => Self::constant(c),
        }
    }

    fn constant(c: &Const) -> String {
        match c {
            Const::Int(v, _) => v.to_string(),
            Const::Float(v, _) => format!("{v:?}"),
            Const::Bool(b) => b.to_string(),
            Const::Null => "null".to_string(),
            Const::Undef(_) => "undef".to_string(),
            Const::Str(s) => format!("{s:?}"),
        }
    }

    fn block_label(&self, id: BlockId) -> &str {
        &self.f.block(id).label
    }

    fn inst(&self, id: InstId) -> String {
        let data = self.f.inst(id);
        let body = self.inst_body(&data.kind, data.ty);
        if data.yields_value() {
            let n = self.num.get(&id.0).copied().unwrap_or(usize::MAX);
            format!("%{n} = {body}")
        } else {
            body
        }
    }

    fn inst_body(&self, kind: &InstKind, ty: IrType) -> String {
        match kind {
            InstKind::Bin { op, lhs, rhs } => format!(
                "{} {} {}, {}",
                op.mnemonic(),
                ty.mnemonic(),
                self.value(lhs),
                self.value(rhs)
            ),
            InstKind::Cmp { pred, lhs, rhs } => format!(
                "{} {} {}, {}",
                if pred.is_float() { "fcmp" } else { "icmp" },
                pred.mnemonic(),
                self.value(lhs),
                self.value(rhs)
            ),
            InstKind::Cast { kind, val } => {
                format!(
                    "{} {} to {}",
                    kind.mnemonic(),
                    self.value(val),
                    ty.mnemonic()
                )
            }
            InstKind::Alloca { elem } => format!("alloca {}", elem.mnemonic()),
            InstKind::Load { ptr } => format!("load {}, {}", ty.mnemonic(), self.value(ptr)),
            InstKind::Store { ptr, val } => {
                format!("store {}, {}", self.value(val), self.value(ptr))
            }
            InstKind::Call { callee, args } => {
                let a: Vec<String> = args.iter().map(|v| self.value(v)).collect();
                format!("call {} @{}({})", ty.mnemonic(), callee.name, a.join(", "))
            }
            InstKind::Phi { incomings } => {
                let arms: Vec<String> = incomings
                    .iter()
                    .map(|(b, v)| format!("[ {}, {} ]", self.value(v), self.block_label(*b)))
                    .collect();
                format!("phi {} {}", ty.mnemonic(), arms.join(", "))
            }
            InstKind::Select { cond, a, b } => format!(
                "select {}, {}, {}",
                self.value(cond),
                self.value(a),
                self.value(b)
            ),
            InstKind::Trap { debug: true } => "debugtrap".to_string(),
            InstKind::Trap { debug: false } => "trap".to_string(),
        }
    }

    fn terminator(&self, t: &Terminator) -> String {
        match t {
            Terminator::Ret(None) => "ret void".to_string(),
            Terminator::Ret(Some(v)) => format!("ret {}", self.value(v)),
            Terminator::Br(b) => format!("br {}", self.block_label(*b)),
            Terminator::CondBr { cond, then, els } => format!(
                "condbr {}, {}, {}",
                self.value(cond),
                self.block_label(*then),
                self.block_label(*els)
            ),
            Terminator::Unreachable => "unreachable".to_string(),
        }
    }
}
