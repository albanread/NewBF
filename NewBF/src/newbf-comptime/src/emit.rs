//! Comptime member **emission** — the fixpoint-loop seam (comptime-breadth §3, §5.3).
//!
//! Beef's hard comptime feature is *emission that feeds back into resolution*: a
//! `[Comptime, EmitGenerator]` method emits Beef **source text** appended to a
//! type body, which is re-parsed and re-resolved, which can trigger more comptime
//! — a fixpoint worklist. NewBF has no VM; the emitter is native JIT'd code that
//! calls a host **runtime shim** ([`__newbf_ct_emit`]) which `newbf-comptime`
//! defines and binds into the comptime JIT as an ORC **absolute symbol**
//! (`OrcJit::add_absolute_symbol`, landed by MS-T0). The shim pushes emitted text
//! into a thread-local [`EMIT_SINK`] the loop drains after each JIT call.
//!
//! **This module is the CB-T2 skeleton:** the seam is *proven* (see the unit test
//! `shim_populates_sink_via_absolute_symbol`), but [`run_emission`] is a **no-op
//! fast path** — `module.emit_jobs` is empty for every current corpus program
//! (CB-T3 is what populates it), so emission changes nothing yet. The real
//! fixpoint loop body (JIT each generator's nullary wrapper, drain the sink,
//! resolve owner-id → qualified name, normalize + dedup, splice as
//! `extension Owner { … }`, re-analyze + re-lower, then strip the emitter/shim
//! before returning) is **CB-T4**.

use std::cell::RefCell;

use newbf_ir::Module as IrModule;

thread_local! {
    /// The host-side sink the emit shim drains into. Each entry is
    /// `(owner_type_id, emitted_text)`: the per-round owner id sema injected as a
    /// literal into the generator's `__newbf_ct_emit` call, and the UTF-8 source
    /// text the generator produced. Thread-local because `OrcJit` runs the emitter
    /// on the calling thread, in-process; CB-T4's loop snapshots + clears this
    /// around each JIT call. **CB-T2 only writes to it from the shim** (and the
    /// unit test reads it back).
    static EMIT_SINK: RefCell<Vec<(i32, String)>> = const { RefCell::new(Vec::new()) };
}

/// The compile-time emit runtime shim — the **single** new host symbol the
/// comptime JIT needs beyond CRT/kernel32 (comptime-breadth §4.2). JIT'd emit
/// generators call this (lowered from `Compiler.EmitTypeBody(text)` by CB-T3)
/// with `(owner_id, text.Ptr, text.Len)`; it copies the bytes out as a `String`
/// and pushes `(owner_id, text)` into [`EMIT_SINK`].
///
/// Bound into the comptime JIT via `OrcJit::add_absolute_symbol("__newbf_ct_emit",
/// __newbf_ct_emit as usize)` **before** the generator is looked up/run, so the
/// generator's call resolves to this host fn — and because an absolute definition
/// in the JITDylib wins over the on-demand process-search generator, there is no
/// duplicate-definition error (proven by the unit test below).
///
/// # Safety
/// `ptr`/`len` come from JIT'd code: `ptr` must point to at least `len` valid
/// bytes (or `len <= 0`, treated as empty). Negative `len` is clamped to `0`. The
/// borrow of `EMIT_SINK` is never held across the FFI return — the text is copied
/// out first — so a re-entrant emit (an emitter calling another) cannot panic on
/// an already-borrowed cell.
///
/// `#[unsafe(no_mangle)]` so the symbol name is exactly `__newbf_ct_emit`; it is
/// bound by address through `add_absolute_symbol`, not resolved as a PE export.
#[unsafe(no_mangle)]
pub extern "C" fn __newbf_ct_emit(owner_type_id: i32, ptr: *const u8, len: i32) {
    let text = if ptr.is_null() || len <= 0 {
        String::new()
    } else {
        // SAFETY: by the shim contract `ptr` points to `len` valid bytes for the
        // duration of this call. Copy out immediately (lossy UTF-8) so no raw
        // pointer / borrow outlives the call.
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
        String::from_utf8_lossy(bytes).into_owned()
    };
    EMIT_SINK.with(|b| b.borrow_mut().push((owner_type_id, text)));
}

/// Snapshot-and-clear the emit sink. CB-T4's loop calls this after each JIT'd
/// generator runs, to collect that round's emissions. Exposed crate-internally so
/// the seam unit test can assert the shim populated it.
fn drain_emit_sink() -> Vec<(i32, String)> {
    EMIT_SINK.with(|b| std::mem::take(&mut *b.borrow_mut()))
}

/// The result of an emission run: how many fixpoint rounds executed and any
/// emission/analyze diagnostics to merge into the driver's diagnostic stream
/// (comptime-breadth §5.3). **CB-T2 always returns `{ rounds: 0, diagnostics:
/// [] }`** (the no-op fast path runs zero rounds); CB-T4/CB-T5 populate it.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct EmitOutcome {
    /// Fixpoint rounds executed (0 for the no-op fast path).
    pub rounds: u32,
    /// Emission/analyze diagnostics, surfaced by the driver like parse/sema ones.
    pub diagnostics: Vec<String>,
}

/// Drive comptime member emission to a fixpoint and return the final,
/// codegen-ready module (comptime-breadth §3.1, §5.3).
///
/// **CB-T2 — no-op fast path.** When `module.emit_jobs` is empty (every current
/// corpus program, since CB-T3 is what records emit jobs) this returns the module
/// **verbatim** with a zero-round [`EmitOutcome`]. The pipeline threads the
/// already-lowered module through this seam after `lower_program` and before
/// `fold_comptime`/codegen; with an empty `emit_jobs` it is a pure pass-through,
/// so all behavior is preserved.
///
/// **CB-T4 — the real loop (stub below).** For a non-empty `emit_jobs` the loop
/// will: JIT each generator's nullary `$ct_emit_run` wrapper in a sandbox clone
/// (with [`__newbf_ct_emit`] bound via `add_absolute_symbol` **before** the
/// lookup), [`drain_emit_sink`], resolve each owner id back to a qualified name
/// via the per-round `name → StructId` map, normalize + dedup the text, splice it
/// as `extension Owner { … }`, re-analyze + re-lower until fixpoint, then **strip**
/// the emitter/shim functions before returning (so the final module JIT/AOT-links
/// with no unresolved `__newbf_ct_emit`). That body re-parses + re-analyzes +
/// re-lowers, which needs `newbf-sema` and the borrowed `SourceFile` set, so the
/// signature will move to source-in (`&[SourceFile<'_>]`) when CB-T4 lands.
///
/// **It never panics**: the non-empty branch returns the module unchanged today
/// (a clear `// CB-T4` stub), rather than `todo!()`, so even if some future change
/// records an emit job before CB-T4 the compiler stays behavior-preserving rather
/// than crashing. CB-T2's invariant (no corpus program populates `emit_jobs`) is
/// what keeps this branch unreached in practice.
pub fn run_emission(module: IrModule) -> Result<(IrModule, EmitOutcome), String> {
    if module.emit_jobs.is_empty() {
        // No-op fast path: pass the module through untouched, zero rounds.
        return Ok((module, EmitOutcome::default()));
    }

    // CB-T4 implements the real fixpoint loop here (JIT generators, drain
    // EMIT_SINK via `drain_emit_sink`, splice `extension Owner { … }`,
    // re-analyze/re-lower, strip emitter/shim before returning). Until then,
    // return the module unchanged so this path is behavior-preserving and never
    // panics. `drain_emit_sink` is referenced here to keep the seam wired.
    let _ = drain_emit_sink; // CB-T4: drained per round around each JIT call.
    Ok((module, EmitOutcome::default()))
}

#[cfg(test)]
mod tests {
    use super::{__newbf_ct_emit, drain_emit_sink, run_emission, EmitOutcome};
    use newbf_ir::{FunctionBuilder, IrType, Module as IrModule, Param, Value};
    use newbf_llvm::OrcJit;

    /// The no-op fast path returns the module verbatim with a zero-round outcome.
    #[test]
    fn empty_emit_jobs_is_a_no_op() {
        let mut m = IrModule::new("inert");
        let mut f = FunctionBuilder::new("noop", vec![], IrType::I32);
        f.ret(Some(Value::int(7, IrType::I32)));
        m.add_function(f.finish());
        assert!(m.emit_jobs.is_empty());

        let before = m.clone();
        let (out, outcome) = run_emission(m).expect("no-op succeeds");
        assert_eq!(out, before, "module passed through verbatim");
        assert_eq!(outcome, EmitOutcome::default());
        assert_eq!(outcome.rounds, 0);
    }

    /// **The load-bearing seam proof (CB-T2 acceptance gate).** JIT a nullary fn
    /// that calls the host shim `__newbf_ct_emit(owner_id, ptr, len)` with a
    /// string literal (lowered to a private `[N x i8]` constant whose value is a
    /// `char8*`), bind the shim by address via `OrcJit::add_absolute_symbol` (the
    /// MS-T0 mechanism), run it, and assert `EMIT_SINK` now holds `(owner_id,
    /// "that string")` — with **no duplicate-definition error** (the absolute shim
    /// wins over the on-demand process-search generator). This is exactly what
    /// CB-T4's emitter relies on.
    ///
    /// The literal pointer is built with `Value::str` (a real string constant in
    /// the JIT'd module — the same construct app code uses), and its byte length
    /// is passed as the `len`, mirroring how CB-T3 lowers `text.Ptr`/`text.Len`.
    #[test]
    fn shim_populates_sink_via_absolute_symbol() {
        // Clear any residue so the assertion is exact for this thread.
        let _ = drain_emit_sink();

        const OWNER_ID: i32 = 4242;
        let text = "public int SumXY() { return x + y; }";

        // Declare the extern shim with the C ABI sema lowers to:
        //   void __newbf_ct_emit(i32 owner, ptr text, i32 len)
        let mut m = IrModule::new("ct_emit_seam");
        m.declare_extern(
            "__newbf_ct_emit",
            vec![
                Param { name: None, ty: IrType::I32 },
                Param { name: None, ty: IrType::Ptr },
                Param { name: None, ty: IrType::I32 },
            ],
            IrType::Void,
        );

        // void emit_one():
        //   __newbf_ct_emit(OWNER_ID, "…literal…", len);
        let mut f = FunctionBuilder::new("emit_one", vec![], IrType::Void);
        f.call(
            "__newbf_ct_emit",
            vec![
                Value::int(OWNER_ID as i128, IrType::I32),
                Value::str(text),
                Value::int(text.len() as i128, IrType::I32),
            ],
            IrType::Void,
        );
        f.ret(None);
        m.add_function(f.finish());

        // Build the comptime sandbox JIT, then bind the host shim by address
        // BEFORE looking up the entry that calls it — the absolute definition
        // wins over the process-search generator, so no duplicate-def error.
        let jit = OrcJit::from_ir(&m).expect("comptime sandbox jit builds");
        jit.add_absolute_symbol("__newbf_ct_emit", __newbf_ct_emit as *const () as usize)
            .expect("absolute shim binds with no duplicate-definition error");

        let addr = jit.lookup("emit_one").expect("emit_one resolves");
        let run: extern "C" fn() = unsafe { std::mem::transmute(addr) };
        run(); // JIT'd code calls the host shim → EMIT_SINK gets one entry.

        let drained = drain_emit_sink();
        assert_eq!(
            drained,
            vec![(OWNER_ID, text.to_string())],
            "the host shim received (owner_id, text) from JIT'd code"
        );
    }
}
