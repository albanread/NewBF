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
//! **CB-T4 — the real fixpoint loop.** [`run_emission`] now takes the *source*
//! (a borrowed [`SourceFile`] set) so it can re-parse / re-analyze / re-lower
//! every round (lowering is already a pure `source → Module` function). Each
//! round it:
//!
//!   1. analyzes + lowers the current source set (base + generated `extension`s);
//!   2. if the lowered module records no `emit_jobs`, **strips nothing extra and
//!      returns** (the no-op fast path — every generator-free corpus program);
//!   3. else JITs a single nullary `$ct_emit_run` wrapper (calling every
//!      generator) in a **sandbox clone** of the module, with [`__newbf_ct_emit`]
//!      bound by address via `add_absolute_symbol` *before* the lookup;
//!   4. drains [`EMIT_SINK`] → `(owner_id, text)` pairs, resolves each owner id
//!      back to the owner's qualified name via the per-round `StructId → qual`
//!      map, normalizes + dedups the text against a `seen` set (idempotency =
//!      termination), and appends each NEW `(owner, text)` as an
//!      `extension <owner> { <text> }` source unit;
//!   5. loops if anything new was emitted, else stops (fixpoint).
//!
//! Before returning, it **strips** the emitter generators and the
//! `__newbf_ct_emit` extern from the final module (comptime-breadth §5.4 / R6):
//! the app/run JIT and the AOT link do **not** register the shim, so a surviving
//! `__newbf_ct_emit` extern would fail `lookup`/link with "Symbols not found".
//! The emitted members (now ordinary reparsed source) stay.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use newbf_ir::{InstKind, Module as IrModule};
use newbf_llvm::OrcJit;
use newbf_parser::{parse_file, CompUnit};
use newbf_sema::{analyze, lower_program, SourceFile};

use newbf_lexer::FileId;

/// Default anti-cycle backstop (comptime-breadth §3.4): the dedup `seen` set
/// makes identical emissions idempotent, so a well-behaved generator stabilizes
/// in 1–3 rounds. This caps the *number* of rounds for a generator that returns
/// normally but emits divergent text each round, turning a would-be infinite
/// loop into a **diagnostic** rather than a hang. (An emitter with an *internal*
/// infinite loop still hangs — bounded execution is deferred.) Overridable via
/// [`EmitConfig::max_rounds`].
pub const DEFAULT_MAX_EMIT_ROUNDS: u32 = 16;

/// Default total-emitted-bytes cap across all rounds (comptime-breadth §3.4): a
/// generous 1 MiB. The dedup set catches a generator that re-emits *identical*
/// text, but a generator that emits **unique growing** text each round defeats
/// dedup (every emission is new) and would only be bounded by the round cap;
/// the byte cap additionally bounds a generator that emits a lot of *new* text
/// within the round budget. Overridable via [`EmitConfig::max_bytes`].
pub const DEFAULT_MAX_EMIT_BYTES: usize = 1 << 20;

/// Tunable termination caps for the emission fixpoint loop (comptime-breadth
/// §3.4). Both guards are **anti-cycle backstops**, not correctness guarantees:
/// a legitimate generator that genuinely needs more rounds / bytes must raise
/// the relevant cap explicitly. Defaults keep the corpus unaffected (the corpus
/// has no divergent emitters, so neither cap is ever approached).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EmitConfig {
    /// Hard cap on fixpoint rounds before a non-convergence diagnostic is
    /// emitted and the loop stops (default [`DEFAULT_MAX_EMIT_ROUNDS`]).
    pub max_rounds: u32,
    /// Hard cap on the total bytes of emitted text accumulated across all
    /// rounds before a runaway-growth diagnostic is emitted and the loop stops
    /// (default [`DEFAULT_MAX_EMIT_BYTES`]).
    pub max_bytes: usize,
}

impl Default for EmitConfig {
    fn default() -> Self {
        Self {
            max_rounds: DEFAULT_MAX_EMIT_ROUNDS,
            max_bytes: DEFAULT_MAX_EMIT_BYTES,
        }
    }
}

/// The base FileId for synthesized `extension` units, well clear of the prelude
/// band (`10_000+`, see `lower_program`) and user files (`0..n`).
const GENERATED_FILE_BASE: u32 = 900_000;

thread_local! {
    /// The host-side sink the emit shim drains into. Each entry is
    /// `(owner_type_id, emitted_text)`: the per-round owner id sema injected as a
    /// literal into the generator's `__newbf_ct_emit` call, and the UTF-8 source
    /// text the generator produced. Thread-local because `OrcJit` runs the emitter
    /// on the calling thread, in-process; the loop snapshots + clears this around
    /// each JIT call.
    static EMIT_SINK: RefCell<Vec<(i32, String)>> = const { RefCell::new(Vec::new()) };

    /// The host-side sink the CR-T0 diagnostic marker drains into. Each entry is
    /// the diagnostic message a generator's `__newbf_ct_emit_error` call carried
    /// (a `Compiler.EmitTypeBody` arg that was neither a string literal nor a
    /// `String`). Surfaced into `EmitOutcome.diagnostics` after each round. Empty
    /// on every well-formed generator, so the common path pays nothing.
    static EMIT_ERROR_SINK: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
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
///
/// The pointer-deref lint is allowed deliberately: this is a C-ABI shim called
/// only from JIT'd code through the absolute-symbol binding, with the documented
/// `(ptr, len)` contract above (mirroring `newbf-runtime`'s `route_free`).
#[allow(clippy::not_unsafe_ptr_arg_deref)]
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

/// Snapshot-and-clear the emit sink. The loop calls this after each JIT'd
/// generator wrapper runs, to collect that round's emissions.
fn drain_emit_sink() -> Vec<(i32, String)> {
    EMIT_SINK.with(|b| std::mem::take(&mut *b.borrow_mut()))
}

/// CR-T0: the compile-time **diagnostic** marker shim — a sibling of
/// `__newbf_ct_emit`. Sema's relaxed `try_lower_emit_type_body` emits a
/// `__newbf_ct_emit_error(owner_id, msg.Ptr, msg.Len)` call for a
/// `Compiler.EmitTypeBody` argument that is neither a string literal nor a
/// `String` (R4 — a loud diagnostic, never a silent decline into the empty
/// `EmitTypeBody(String)` stub). It copies the message bytes out and pushes them
/// into [`EMIT_ERROR_SINK`], so an actually-run malformed generator surfaces a
/// diagnostic into `EmitOutcome.diagnostics` rather than miscompiling silently.
///
/// `#[unsafe(no_mangle)]` so the symbol name is exactly `__newbf_ct_emit_error`;
/// bound by address via `add_absolute_symbol` (same as `__newbf_ct_emit`).
///
/// # Safety
/// Same `(ptr, len)` contract as `__newbf_ct_emit`: `ptr` points to `len` valid
/// bytes for the call, or `len <= 0` (treated as empty). The bytes are copied out
/// before the borrow of `EMIT_ERROR_SINK` is taken, so no raw pointer outlives the
/// call.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[unsafe(no_mangle)]
pub extern "C" fn __newbf_ct_emit_error(_owner_type_id: i32, ptr: *const u8, len: i32) {
    let msg = if ptr.is_null() || len <= 0 {
        String::new()
    } else {
        // SAFETY: by the shim contract `ptr` points to `len` valid bytes for the
        // duration of this call. Copy out immediately (lossy UTF-8).
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
        String::from_utf8_lossy(bytes).into_owned()
    };
    EMIT_ERROR_SINK.with(|b| b.borrow_mut().push(msg));
}

/// Snapshot-and-clear the diagnostic-marker sink. Drained alongside the emit sink
/// after each JIT'd generator wrapper runs.
fn drain_emit_error_sink() -> Vec<String> {
    EMIT_ERROR_SINK.with(|b| std::mem::take(&mut *b.borrow_mut()))
}

/// The result of an emission run: how many fixpoint rounds executed and any
/// emission/analyze diagnostics to merge into the driver's diagnostic stream
/// (comptime-breadth §5.3). The no-op fast path runs zero rounds.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct EmitOutcome {
    /// Fixpoint rounds executed (0 for the no-op fast path).
    pub rounds: u32,
    /// Emission/analyze diagnostics, surfaced by the driver like parse/sema ones.
    pub diagnostics: Vec<String>,
}

/// The cross-round dedup key: an owner qualified name plus a normalized form of
/// the emitted text. Keying by *normalized* text makes cosmetically-different
/// re-emissions of the same member idempotent (A emits B, B re-emits B-identical
/// → no new unit), which is the termination guarantee (comptime-breadth §3.4).
type EmitKey = (String, String);

/// Normalize emitted text for the dedup key: trim, strip `//` line comments, and
/// collapse interior whitespace runs to a single space. Cosmetic differences
/// (indentation, trailing comments) thus map to the same key.
fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_ws = false;
    for raw_line in text.lines() {
        // Strip a trailing `//` line comment (v1 normalization — §10.6).
        let line = match raw_line.find("//") {
            Some(i) => &raw_line[..i],
            None => raw_line,
        };
        for ch in line.chars() {
            if ch.is_whitespace() {
                if !last_ws && !out.is_empty() {
                    out.push(' ');
                }
                last_ws = true;
            } else {
                out.push(ch);
                last_ws = false;
            }
        }
        // A newline is interior whitespace too — collapse to a single space.
        if !last_ws && !out.is_empty() {
            out.push(' ');
            last_ws = true;
        }
    }
    out.trim().to_string()
}

/// Drive comptime member emission to a fixpoint and return the final,
/// codegen-ready module (comptime-breadth §3.1, §5.3), using the default
/// termination caps ([`EmitConfig::default`]). This is the entry point the
/// driver and run-corpus harness call.
///
/// Equivalent to [`run_emission_with`]`(base, &EmitConfig::default())` — see it
/// for the full loop, guard, and strip semantics.
pub fn run_emission(base: &[SourceFile<'_>]) -> Result<(IrModule, EmitOutcome), String> {
    run_emission_with(base, &EmitConfig::default())
}

/// Drive comptime member emission to a fixpoint with explicit termination caps
/// ([`EmitConfig`]) and return the final, codegen-ready module
/// (comptime-breadth §3.1, §5.3).
///
/// `base` is the user's parsed source (the same `&[SourceFile]` the driver and
/// run-corpus harness build and hand to `analyze` / `lower_program`).
/// `run_emission_with` re-analyzes + re-lowers internally — lowering is a pure
/// `source → Module` function, so the loop just augments the source set with
/// `extension Owner { … }` units and re-lowers until no new member is emitted.
///
/// **Fast path.** When the freshly-lowered module records no `emit_jobs` (every
/// generator-free program — the entire current corpus), the module is returned
/// after **round 0** with no emitter/shim to strip: a pure pass-through, so all
/// existing behavior is preserved (the loop runs exactly the same `analyze` +
/// `lower_program` the driver used to run inline).
///
/// **The loop.** For a non-empty `emit_jobs`: JIT a single nullary
/// `$ct_emit_run` wrapper (calling every generator, in a deterministic order) in
/// a sandbox clone with [`__newbf_ct_emit`] bound via `add_absolute_symbol`,
/// drain [`EMIT_SINK`], resolve each owner id → qualified name via the per-round
/// `StructId → qual` map, normalize + dedup, splice as `extension Owner { … }`,
/// and re-lower.
///
/// **Termination + diagnostics (CB-T5, load-bearing).** The loop is
/// triple-guarded and a tripped guard is reported, **never** swallowed and
/// **never** a hang or crash:
///   * the `seen` normalized-text dedup makes identical re-emissions idempotent
///     (the fixpoint exit);
///   * a **round cap** ([`EmitConfig::max_rounds`]) bounds a generator that
///     returns but emits divergent text each round — on trip the loop **stops**
///     and pushes a structured non-convergence diagnostic into
///     [`EmitOutcome::diagnostics`], returning the module-so-far (not an `Err`);
///   * a **byte cap** ([`EmitConfig::max_bytes`]) bounds total emitted bytes
///     across rounds — a generator emitting *unique growing* text defeats dedup
///     (every emission is new) but trips this cap with the same
///     stop-with-diagnostic behavior.
///
/// Additionally, if **re-analyzing the spliced sources** produces analyze
/// diagnostics (the EMITTED code is malformed — e.g. a duplicate member), those
/// are surfaced into [`EmitOutcome::diagnostics`] and the loop **stops** (abort
/// on generated-code analyze diagnostics — not a silent miscompile, not an
/// infinite loop). A parse error in generated source still aborts via `Err`
/// (a parse failure is structural, not a recoverable program diagnostic).
///
/// **The strip (R6, load-bearing).** Before returning, every emitter generator
/// (a `module.comptime` symbol that transitively references `__newbf_ct_emit`)
/// and the `__newbf_ct_emit` extern itself are removed — the app/run JIT and the
/// AOT link never register the shim, so a survivor would fail `lookup`/link. The
/// emitted members (ordinary reparsed source) stay.
pub fn run_emission_with(
    base: &[SourceFile<'_>],
    config: &EmitConfig,
) -> Result<(IrModule, EmitOutcome), String> {
    run_emission_inner(base, config, run_generators)
}

/// The shared fixpoint loop, parameterized by the generator-runner so tests can
/// inject a deterministic emitter (e.g. one that returns normally but emits
/// **divergent / growing** text each round, to exercise the round/byte caps
/// without depending on a non-deterministic JIT'd generator). Production calls
/// it with [`run_generators`] (which JITs the real generators in a sandbox).
fn run_emission_inner(
    base: &[SourceFile<'_>],
    config: &EmitConfig,
    mut run_gens: impl FnMut(&IrModule) -> Result<Vec<(i32, String)>, String>,
) -> Result<(IrModule, EmitOutcome), String> {
    // The owned synthesized `extension` units, kept alive for the whole loop so
    // each round's borrowed `SourceFile` set can reference them (comptime-breadth
    // §3.4 ownership model). `String` (source) + `CompUnit` (its parse tree).
    let mut generated: Vec<(String, CompUnit)> = Vec::new();
    // Normalized (owner, text) pairs already spliced — the idempotency guard.
    let mut seen: HashSet<EmitKey> = HashSet::new();
    let mut rounds: u32 = 0;
    // Total bytes of emitted text accumulated across all rounds — the byte cap's
    // running total (a runaway emitter that emits unique growing text each round
    // defeats `seen` dedup but is bounded here).
    let mut total_emitted_bytes: usize = 0;

    loop {
        // (a) Build the round's source set = base + generated, then lower it.
        //     The generated FileIds sit well above the prelude/user bands.
        let mut files: Vec<SourceFile<'_>> = base
            .iter()
            .map(|f| SourceFile {
                file: f.file,
                src: f.src,
                unit: f.unit,
                name: f.name,
            })
            .collect();
        for (i, (src, unit)) in generated.iter().enumerate() {
            files.push(SourceFile {
                file: FileId(GENERATED_FILE_BASE + i as u32),
                src: src.as_str(),
                unit,
                // Comptime-generated source has no user file name.
                name: "<generated>",
            });
        }
        let program = analyze(&files);

        // (a') Abort on generated-code analyze diagnostics (comptime-breadth
        //      §3.4 step 1 / CB-T5). Only relevant once we have spliced
        //      `generated` units: a diagnostic now means the EMITTED source is
        //      malformed (e.g. a duplicate member). Surface every analyze
        //      diagnostic into the outcome and STOP — don't lower garbage IR
        //      (a silent miscompile) and don't loop forever. Base-program
        //      diagnostics (round 0, no generated units) are the driver's job to
        //      surface via its own `analyze`, so we don't double-report them.
        if !generated.is_empty() && !program.diagnostics.is_empty() {
            let mut diagnostics: Vec<String> = vec![format!(
                "comptime: generated source produced {} analyze diagnostic(s); \
                 aborting emission (the emitted code is malformed)",
                program.diagnostics.len()
            )];
            diagnostics.extend(
                program
                    .diagnostics
                    .iter()
                    .map(|d| format!("comptime: generated-code diagnostic: {}", d.message)),
            );
            // Lower so the strip + return produce a settled module (the emitted
            // members are still present as best-effort IR; the diagnostics tell
            // the driver to fail, so this module is never codegen'd in practice).
            let module = lower_program(&files, &program);
            let final_module = strip_emitter_and_shim(module);
            return Ok((final_module, EmitOutcome { rounds, diagnostics }));
        }

        let module = lower_program(&files, &program);

        // (b) Fast path / fixpoint exit: no generators recorded → done.
        if module.emit_jobs.is_empty() {
            let final_module = strip_emitter_and_shim(module);
            return Ok((final_module, EmitOutcome { rounds, diagnostics: Vec::new() }));
        }

        // (b') Round cap — anti-cycle backstop (comptime-breadth §3.4). A
        //      generator that returns but emits divergent text each round never
        //      reaches the fixpoint; cap the rounds, STOP, and surface a
        //      structured non-convergence diagnostic (NOT an `Err`, NOT a hang).
        //      The module-so-far is stripped + returned so the driver can still
        //      report cleanly.
        if rounds >= config.max_rounds {
            let diag = format!(
                "comptime: emission did not converge within {} round(s) — a generator \
                 likely emits divergent text each round (raise EmitConfig::max_rounds \
                 if the generator legitimately needs more)",
                config.max_rounds
            );
            let final_module = strip_emitter_and_shim(module);
            return Ok((
                final_module,
                EmitOutcome { rounds, diagnostics: vec![diag] },
            ));
        }
        rounds += 1;

        // (c) Per-round owner-id → qualified-name map. CB-T3 injects the owner's
        //     dense `StructId.0` as the literal in each `__newbf_ct_emit` call;
        //     `module.structs[id]` is that round's registration, so its simple
        //     name resolves the qualified name carried by the matching emit job.
        //     StructIds shift between rounds, hence the map is rebuilt each round.
        let id_to_qual = owner_id_to_qual(&module);

        // (d) Run every generator (deterministic order) via one nullary wrapper
        //     in a sandbox clone, then drain the sink.
        let _ = drain_emit_sink(); // discard any residue from a prior call/thread
        let emissions = run_gens(&module)?;

        // (e) Resolve + normalize + dedup; splice each NEW emission as an
        //     `extension`. Process in a deterministic order so the next round's
        //     StructId assignment (and thus any further emission) is reproducible.
        let mut new_units: Vec<(String, String)> = Vec::new();
        for (owner_id, text) in emissions {
            // Account every emitted byte toward the byte cap *before* dedup, so a
            // generator that emits unique growing text (defeating dedup) is
            // bounded by total output, not just by distinct-unit count.
            total_emitted_bytes = total_emitted_bytes.saturating_add(text.len());
            let Some(qual) = id_to_qual.get(&owner_id) else {
                return Err(format!(
                    "comptime: emitted text routed to unknown owner id {owner_id} \
                     (no matching type registered this round)"
                ));
            };
            let key: EmitKey = (qual.clone(), normalize(&text));
            if seen.insert(key) {
                new_units.push((qual.clone(), text));
            }
        }
        new_units.sort();

        // (e') Byte cap — anti-cycle backstop for *growth* (comptime-breadth
        //      §3.4). A generator emitting unique text each round defeats the
        //      `seen` dedup (every emission is new), so the round cap alone could
        //      let it emit a great deal before tripping; the byte cap bounds the
        //      total. On trip, STOP with a structured diagnostic (NOT an `Err`,
        //      NOT a hang). Checked after accounting this round's emissions.
        if total_emitted_bytes > config.max_bytes {
            let diag = format!(
                "comptime: emission exceeded the {}-byte total-output cap after {} round(s) \
                 ({} bytes emitted) — a generator likely emits growing text each round \
                 (raise EmitConfig::max_bytes if this is intended)",
                config.max_bytes, rounds, total_emitted_bytes
            );
            let final_module = strip_emitter_and_shim(module);
            return Ok((
                final_module,
                EmitOutcome { rounds, diagnostics: vec![diag] },
            ));
        }

        // (f) Fixpoint reached if nothing new was emitted this round.
        if new_units.is_empty() {
            let final_module = strip_emitter_and_shim(module);
            return Ok((final_module, EmitOutcome { rounds, diagnostics: Vec::new() }));
        }

        // Splice each new emission as an owned `extension Owner { … }` unit and
        // loop. A parse error in generated source aborts (a malformed emission is
        // an ordinary diagnostic, not a silent miscompile — comptime-breadth §3.4).
        for (qual, text) in new_units {
            let unit_src = format!("extension {qual} {{ {text} }}\n");
            let fid = FileId(GENERATED_FILE_BASE + generated.len() as u32);
            let (unit, pdiags) = parse_file(&unit_src, fid);
            if !pdiags.is_empty() {
                return Err(format!(
                    "comptime: emitted source for `{qual}` failed to parse: {pdiags:?}\n  \
                     emitted text: {text}"
                ));
            }
            generated.push((unit_src, unit));
        }
    }
}

/// Build the per-round `StructId.0 → owner qualified name` map used to resolve a
/// drained owner id back to its routing key. CB-T3 records each generator's
/// `EmitJob { owner_qual_name, .. }` and injects the owner's `StructId.0` as the
/// `__newbf_ct_emit` literal; `module.structs[id].name` is that id's *simple*
/// name. Match the job's qualified name (whose last `.`-segment is the simple
/// name) to the struct id with that simple name.
fn owner_id_to_qual(module: &IrModule) -> HashMap<i32, String> {
    // Simple name → StructId index (the dense `module.structs` order).
    let mut name_to_id: HashMap<&str, i32> = HashMap::new();
    for (i, s) in module.structs.iter().enumerate() {
        name_to_id.insert(s.name.as_str(), i as i32);
    }
    let mut map: HashMap<i32, String> = HashMap::new();
    for job in &module.emit_jobs {
        let simple = job
            .owner_qual_name
            .rsplit('.')
            .next()
            .unwrap_or(&job.owner_qual_name);
        if let Some(&id) = name_to_id.get(simple) {
            map.insert(id, job.owner_qual_name.clone());
        }
    }
    map
}

/// JIT a single nullary `$ct_emit_run` wrapper that calls every emit generator
/// (in a deterministic order), in a **sandbox clone** of `module` so running the
/// generators never disturbs the real module. [`__newbf_ct_emit`] is bound by
/// address via `add_absolute_symbol` **before** the lookup (so the absolute
/// definition wins over the process-search generator, no duplicate-def error).
/// Returns the drained `(owner_id, text)` emissions.
fn run_generators(module: &IrModule) -> Result<Vec<(i32, String)>, String> {
    use newbf_ir::{FunctionBuilder, IrType, Value};

    // Deterministic generator order (owner qual name, then symbol) so the
    // emitted-source order — and thus the next round's StructId assignment — is
    // reproducible (comptime-breadth §3.4 determinism).
    let mut jobs: Vec<&newbf_ir::EmitJob> = module.emit_jobs.iter().collect();
    jobs.sort_by(|a, b| {
        a.owner_qual_name
            .cmp(&b.owner_qual_name)
            .then_with(|| a.symbol.cmp(&b.symbol))
    });

    let mut sandbox = module.clone();
    // $ct_emit_run() { gen0(); gen1(); … }  — a void wrapper that calls each
    // generator with no user args (the §3.3 invocation recipe).
    let mut wb = FunctionBuilder::new("$ct_emit_run", vec![], IrType::Void);
    for job in &jobs {
        wb.call(&job.symbol, vec![] as Vec<Value>, IrType::Void);
    }
    wb.ret(None);
    sandbox.add_function(wb.finish());

    let jit = OrcJit::from_ir(&sandbox)
        .map_err(|e| format!("comptime: emission sandbox JIT build failed: {e}"))?;
    // Bind the host shims by address BEFORE the lookup (the absolute definition
    // wins over the process-search generator → no duplicate-definition error).
    jit.add_absolute_symbol("__newbf_ct_emit", __newbf_ct_emit as *const () as usize)
        .map_err(|e| format!("comptime: binding __newbf_ct_emit failed: {e}"))?;
    // CR-T0: bind the diagnostic marker too, so a generator that hits the
    // `Compiler.EmitTypeBody` bad-arg path (a `__newbf_ct_emit_error` call) resolves
    // and surfaces a diagnostic rather than failing to JIT-link. Sema declares the
    // extern in every generator-owning module (uncalled on the literal/`String`
    // paths), so the symbol must resolve even when the marker is never invoked.
    jit.add_absolute_symbol(
        "__newbf_ct_emit_error",
        __newbf_ct_emit_error as *const () as usize,
    )
    .map_err(|e| format!("comptime: binding __newbf_ct_emit_error failed: {e}"))?;
    let addr = jit.lookup("$ct_emit_run").ok_or_else(|| {
        "comptime: emission wrapper `$ct_emit_run` did not resolve in the sandbox JIT".to_string()
    })?;
    // SAFETY: `$ct_emit_run` is a nullary `void` fn just built above; `addr` is
    // its entry point in JIT'd memory, mapped while `jit` is alive (it is, until
    // this fn returns after the call). The generators run in-process and push
    // into EMIT_SINK via the bound shim.
    let run: extern "C" fn() = unsafe { std::mem::transmute(addr) };
    run();

    // CR-T0: a malformed `Compiler.EmitTypeBody` argument pushed a diagnostic into
    // the error sink. Surface it as an emission error (loud — never a silent
    // miscompile). The text sink is drained too so no residue leaks to the next
    // round/thread.
    let errors = drain_emit_error_sink();
    if !errors.is_empty() {
        let _ = drain_emit_sink();
        return Err(format!("comptime: {}", errors.join("; ")));
    }

    Ok(drain_emit_sink())
}

/// The host emit shims sema may emit inside a `[Comptime, EmitGenerator]` body:
/// the text shim (`__newbf_ct_emit`) and the CR-T0 diagnostic marker
/// (`__newbf_ct_emit_error`, emitted for a `Compiler.EmitTypeBody` arg that is
/// neither a string literal nor a `String`). Both are stripped from the final
/// module identically.
const EMIT_SHIM_SYMBOLS: [&str; 2] = ["__newbf_ct_emit", "__newbf_ct_emit_error"];

/// Strip the emitter generators + the emit-shim externs from the final module
/// (comptime-breadth §5.4 / R6 — load-bearing for the app/run JIT and AOT link,
/// neither of which registers the shims).
///
/// Droppable = any function that **transitively references an emit shim**
/// (`__newbf_ct_emit` or the CR-T0 `__newbf_ct_emit_error` marker — the
/// generators and anything that calls into them, computed by a reverse
/// reachability sweep) **and** is a `module.comptime` symbol — plus the shim
/// extern declarations themselves. The emitted members (ordinary reparsed source,
/// *not* comptime, *not* referencing a shim) are kept. This agrees with
/// `fold_comptime`'s `reachable_from_ordinary` view: both treat `module.comptime`
/// members reaching a shim as droppable, so neither keeps a function the other
/// needs.
fn strip_emitter_and_shim(mut module: IrModule) -> IrModule {
    let comptime: HashSet<&str> = module.comptime.iter().map(String::as_str).collect();

    // Functions that reference an emit shim directly.
    let mut references_shim: HashSet<String> = HashSet::new();
    for f in &module.funcs {
        if f.insts.iter().any(|i| {
            matches!(&i.kind, InstKind::Call { callee, .. }
                if EMIT_SHIM_SYMBOLS.contains(&callee.name.as_str()))
        }) {
            references_shim.insert(f.name.clone());
        }
    }
    // Transitively: any comptime function that calls one that references the shim
    // also reaches it. Iterate to a fixpoint over the (small) comptime call graph.
    loop {
        let mut grew = false;
        for f in &module.funcs {
            if references_shim.contains(&f.name) {
                continue;
            }
            let reaches = f.insts.iter().any(|i| {
                matches!(&i.kind, InstKind::Call { callee, .. }
                    if references_shim.contains(&callee.name))
            });
            if reaches {
                references_shim.insert(f.name.clone());
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }

    // Drop: the emit-shim externs themselves, plus every comptime function that
    // (transitively) references a shim — i.e. the emitter generators.
    module.funcs.retain(|f| {
        if EMIT_SHIM_SYMBOLS.contains(&f.name.as_str()) {
            return false;
        }
        !(comptime.contains(f.name.as_str()) && references_shim.contains(&f.name))
    });

    // The emit jobs are consumed — drop them so a downstream re-`run_emission`
    // (or any inspector) sees a settled module with no pending generators.
    module.emit_jobs.clear();
    module
}

#[cfg(test)]
mod tests {
    use super::{
        __newbf_ct_emit, drain_emit_sink, normalize, owner_id_to_qual, run_emission,
        run_emission_inner, strip_emitter_and_shim, EmitConfig, EmitOutcome,
    };
    use newbf_ir::{FunctionBuilder, IrType, Module as IrModule, Param, Value};
    use newbf_lexer::FileId;
    use newbf_llvm::OrcJit;
    use newbf_parser::parse_file;
    use newbf_sema::SourceFile;

    /// Read a NUL-terminated C string at `p` (a `char8*` returned from JIT'd
    /// reflection code) into an owned `String`. Used by the CR-T1 companion test
    /// to compare a reflected field name read out of the sandbox-shaped module.
    ///
    /// # Safety
    /// `p` must point at a NUL-terminated byte run in still-mapped memory (it does:
    /// the field-name `char8*` is a `.rodata` cstr the JIT'd module owns while
    /// `jit` is alive). A null `p` reads as the empty string.
    unsafe fn cstr_to_string(p: *const u8) -> String {
        if p.is_null() {
            return String::new();
        }
        let mut len = 0usize;
        // SAFETY: by the contract `p` points at a NUL-terminated run; walk to the
        // terminator, then copy the bytes out (lossy UTF-8) before returning.
        while unsafe { *p.add(len) } != 0 {
            len += 1;
        }
        let bytes = unsafe { std::slice::from_raw_parts(p, len) };
        String::from_utf8_lossy(bytes).into_owned()
    }

    /// Parse one `.bf` source and drive emission over it, returning the final
    /// module (the same shape the driver/harness use).
    fn emit_over(src: &str) -> (IrModule, EmitOutcome) {
        let (unit, pdiags) = parse_file(src, FileId(0));
        assert!(pdiags.is_empty(), "parse diagnostics: {pdiags:?}");
        let files = [SourceFile {
            file: FileId(0),
            src,
            unit: &unit,
            name: "",
        }];
        run_emission(&files).expect("emission succeeds")
    }

    /// A program with no `[EmitGenerator]` hits the no-op fast path: zero rounds,
    /// a normal module, and `Program.Main` runs (the corpus invariant).
    #[test]
    fn no_generator_is_a_no_op_fast_path() {
        let src = r#"
            class Program {
                public static int32 Main() { return 7; }
            }
        "#;
        let (module, outcome) = emit_over(src);
        assert_eq!(outcome.rounds, 0, "fast path runs zero rounds");
        assert!(outcome.diagnostics.is_empty());
        let jit = OrcJit::from_ir(&module).expect("jit builds");
        let addr = jit.lookup("Program.Main").expect("Program.Main resolves");
        let main: extern "C" fn() -> i32 = unsafe { std::mem::transmute(addr) };
        assert_eq!(main(), 7);
    }

    /// normalize() collapses whitespace and strips line comments so cosmetic
    /// differences dedup to the same key.
    #[test]
    fn normalize_collapses_whitespace_and_comments() {
        assert_eq!(
            normalize("  public  int  F()\n  {  return 1;  } // hi\n"),
            normalize("public int F() { return 1; }")
        );
        assert_eq!(normalize("a // comment\nb"), "a b");
    }

    /// **The CB-T4 marquee, end-to-end on real source.** A `[Comptime,
    /// EmitGenerator]` generator emits a method that reads pre-existing fields;
    /// after emission the method exists, resolves, and `Main` returns 42 — which
    /// is computable *only if* the emitted member read the original fields.
    #[test]
    fn emits_member_reading_preexisting_fields() {
        let src = r#"
            class Vec2 {
                public int32 mX;
                public int32 mY;
                public this(int32 x, int32 y) { this.mX = x; this.mY = y; }

                [Comptime, EmitGenerator]
                public static void Generate() {
                    Compiler.EmitTypeBody("public int32 Sum() { return this.mX + this.mY; }");
                }
            }
            class Program {
                public static int32 Main() {
                    Vec2 v = new Vec2(30, 12);
                    int32 r = v.Sum();
                    delete v;
                    return r;
                }
            }
        "#;
        let (module, outcome) = emit_over(src);
        assert!(outcome.rounds >= 1, "at least one emission round ran");

        // The generated method is present; the generator + shim are stripped.
        assert!(
            module.funcs.iter().any(|f| f.name.ends_with("Vec2.Sum")
                || f.name == "Vec2.Sum"
                || f.name.contains("Sum")),
            "the emitted Sum method must be present in the final module"
        );
        assert!(
            !module.funcs.iter().any(|f| f.name.contains("Generate")),
            "the [EmitGenerator] must be stripped"
        );
        assert!(
            !module.funcs.iter().any(|f| f.name == "__newbf_ct_emit"),
            "the __newbf_ct_emit shim must be stripped"
        );

        // Final module JIT-links (no unresolved __newbf_ct_emit) and Main → 42.
        let jit = OrcJit::from_ir(&module).expect("final module JIT-links clean");
        let addr = jit.lookup("Program.Main").expect("Program.Main resolves");
        let main: extern "C" fn() -> i32 = unsafe { std::mem::transmute(addr) };
        assert_eq!(main(), 42, "emitted member reads pre-existing fields → 42");
    }

    /// **CR-T1 — the sandbox-reflection HARD gate (the integration half).** Drive
    /// `run_emission` over a `[Comptime, EmitGenerator]` generator that *reflects*
    /// inside the `$ct_emit_run` sandbox:
    ///   1. reads `typeof(Pair).GetFieldCount()` — a reflection-metadata read in
    ///      the sandbox JIT (the `%struct.Type` global the sandbox clone's
    ///      `emit_metadata` built, CR §3.1); AND
    ///   2. **binds a `FieldInfo` local** for `GetField(0).GetName()` (the R5
    ///      value-struct-by-value path — `GetField` returns `FieldInfo` BY VALUE,
    ///      so it cannot be chained; a local is bound, then `.GetName()` is read);
    ///   3. uses CR-T0's runtime-`String` `Compiler.EmitTypeBody(...)` path to emit
    ///      a member whose returned constant is DERIVED from both reflection reads
    ///      (the field count + whether the first field's reflected name is "mA").
    ///
    /// `Main → 42` is computable only if the sandbox saw `GetFieldCount() == 2`
    /// AND the bound-`FieldInfo`-local's `GetName()` returned "mA" (`2 + 40`). The
    /// assertions also pin the strip: the corlib `Type`/`FieldInfo` reflection
    /// methods SURVIVE (they are non-comptime corlib code), while the generator +
    /// `__newbf_ct_emit` are GONE. Per CR §3.1 the metadata is already in the
    /// sandbox clone, so this works out-of-the-box — this test is the pin that
    /// proves it (and would fail loudly if a future change gated `emit_metadata`).
    #[test]
    fn sandbox_generator_reflects_field_count_and_bound_fieldinfo_name() {
        let src = r#"
            [Reflect(.Fields)]
            class Pair {
                public int32 mA;
                public int32 mB;

                [Comptime, EmitGenerator]
                public static void Generate() {
                    // (1) Reflection-metadata read in the sandbox: the field count.
                    int32 n = typeof(Pair).GetFieldCount();        // 2
                    // (2) The R5 struct-by-value path: GetField returns a FieldInfo
                    //     BY VALUE, so bind a LOCAL before reading .GetName() —
                    //     never chain off the value-struct rvalue.
                    FieldInfo gf = typeof(Pair).GetField(0);
                    int32 bonus = Internal.StrEq(gf.GetName(), "mA") ? 40 : 0;
                    // (3) Emit a member whose constant is derived from BOTH reads,
                    //     via CR-T0's runtime-String EmitTypeBody path (NOT a literal).
                    //     Widen to `int` so `Append(int)` (decimal render) is the
                    //     unambiguous overload, not `Append(char8)` (a char code).
                    int total = n + bonus;                          // 2 + 40 = 42
                    String s = new String("public int32 Probe() { return ");
                    s.Append(total);                                // "42"
                    s.Append("; }");
                    Compiler.EmitTypeBody(s);
                    delete s;                                       // exactly once
                }
            }
            class Program {
                public static int32 Main() {
                    Pair p = new Pair();
                    int32 r = p.Probe();                            // the emitted member
                    delete p;
                    return r;
                }
            }
        "#;
        let (module, outcome) = emit_over(src);
        assert!(
            outcome.diagnostics.is_empty(),
            "the reflecting generator converges cleanly: {:?}",
            outcome.diagnostics
        );
        assert!(outcome.rounds >= 1, "at least one emission round ran");

        // The strip kept the corlib reflection API the generator pulled in (it is
        // ordinary corlib code, NOT module.comptime) and dropped the generator +
        // shim (comptime, reaches __newbf_ct_emit).
        assert!(
            module.funcs.iter().any(|f| f.name.contains("Type.GetField"))
                && module.funcs.iter().any(|f| f.name.contains("Type.GetFieldCount")),
            "corlib `Type.GetField`/`GetFieldCount` survive the strip"
        );
        assert!(
            module.funcs.iter().any(|f| f.name.contains("FieldInfo.GetName")),
            "corlib `FieldInfo.GetName` survives the strip"
        );
        assert!(
            !module.funcs.iter().any(|f| f.name.contains("Generate")),
            "the [EmitGenerator] must be stripped"
        );
        assert!(
            !module.funcs.iter().any(|f| f.name == "__newbf_ct_emit"),
            "the __newbf_ct_emit shim must be stripped"
        );
        assert!(
            module.emit_jobs.is_empty(),
            "emit jobs are consumed by the strip"
        );

        // The final module JIT-links clean (no unresolved __newbf_ct_emit) and the
        // emitted member returns 42 — provable only if the sandbox reflected the
        // field count AND the bound-FieldInfo-local's name.
        let jit = OrcJit::from_ir(&module).expect("final module JIT-links clean");
        let addr = jit.lookup("Program.Main").expect("Program.Main resolves");
        let main: extern "C" fn() -> i32 = unsafe { std::mem::transmute(addr) };
        assert_eq!(
            main(),
            42,
            "emitted Probe() returns GetFieldCount(2) + name-matches-\"mA\"(40) = 42"
        );
    }

    /// **CR-T1 — the companion `from_ir` sandbox-shaped unit test (R5 pin).** Where
    /// the integration test above drives the *whole* `run_emission` pipeline, this
    /// isolates the one thing the app-JIT `reflect_field_*.bf` tests don't: a
    /// **value-struct `FieldInfo` return executed inside a `$ct_emit_run`-shaped
    /// wrapper**, in a hand-built `from_ir` module that mirrors the sandbox clone.
    ///
    /// The module carries exactly what `emit_module` builds for every module
    /// (including the sandbox clone): the `%struct.Type` / `%struct.FieldInfo`
    /// aggregates + a per-type `Type` global + the in-module `__newbf_type_by_id`
    /// accessor (all from `emit_metadata`, driven by the `TypeMeta` we register).
    /// On top of it:
    ///   * `get_field0(i32 id) -> FieldInfo` looks up the `Type*` via
    ///     `__newbf_type_by_id(id)`, reads `mFields`, and returns `mFields[0]` BY
    ///     VALUE — the struct-by-value reflection return (mirrors `Type.GetField`);
    ///   * `$ct_emit_run()` (the sandbox wrapper name) calls `get_field0`, **binds
    ///     the returned `FieldInfo` into a local alloca** (the R5 bound-local
    ///     pattern), and stores its `mName` into a host-visible global so the test
    ///     can read it back.
    ///
    /// Asserts: (i) `__newbf_type_by_id` + the `Point.$type` global resolve in the
    /// JIT'd sandbox-shaped module; (ii) running `$ct_emit_run` produces the
    /// reflected first-field name "mX" — i.e. the value-struct `FieldInfo` return
    /// is present AND callable inside the wrapper, not just in the app JIT.
    #[test]
    fn from_ir_sandbox_shaped_value_struct_fieldinfo_return_in_ct_emit_run() {
        use newbf_ir::{FieldDef, FieldMeta, ReflectPolicy, StructDef, TypeMeta, VtableDef};

        let mut m = IrModule::new("sandbox_shaped");

        // The corlib value-struct ABIs, byte-identical to what `emit_metadata`
        // emits (`%struct.Type` { 5×i32, 3×ptr }, `%struct.FieldInfo` { ptr, i32,
        // i32 }). We index `mFields` (Type field 6) through a `FieldInfo*`.
        let type_id_struct = m.add_struct(StructDef {
            name: "Type".into(),
            fields: vec![
                FieldDef { name: "mSize".into(), ty: IrType::I32 },
                FieldDef { name: "mTypeId".into(), ty: IrType::I32 },
                FieldDef { name: "mFlags".into(), ty: IrType::I32 },
                FieldDef { name: "mFieldCount".into(), ty: IrType::I32 },
                FieldDef { name: "mMethodCount".into(), ty: IrType::I32 },
                FieldDef { name: "mName".into(), ty: IrType::Ptr },
                FieldDef { name: "mFields".into(), ty: IrType::Ptr },
                FieldDef { name: "mMethods".into(), ty: IrType::Ptr },
            ],
        });
        let fieldinfo_struct = m.add_struct(StructDef {
            name: "FieldInfo".into(),
            fields: vec![
                FieldDef { name: "mName".into(), ty: IrType::Ptr },
                FieldDef { name: "mOffset".into(), ty: IrType::I32 },
                FieldDef { name: "mTypeId".into(), ty: IrType::I32 },
            ],
        });

        // A `[Reflect(.Fields)]`-shaped class `Point { $header, mX, mY }` with a
        // dense type-id of 0, so `emit_metadata` emits a non-null FieldInfo array
        // (first entry's name = "mX") and a `Point.$type` global at table index 0.
        let point = m.add_struct(StructDef {
            name: "Point".into(),
            fields: vec![
                FieldDef { name: "$header".into(), ty: IrType::Ptr },
                FieldDef { name: "mX".into(), ty: IrType::I32 },
                FieldDef { name: "mY".into(), ty: IrType::I32 },
            ],
        });
        // ClassVData global so `type_global_name`'s prefix (→ `Point.$type`) is
        // recoverable by `emit_metadata`.
        m.add_vtable(VtableDef { name: "Point.$cvdata".into(), entries: vec![], type_id: 0 });
        m.add_type_meta(TypeMeta::new(
            0,
            point,
            "Point".into(),
            ReflectPolicy(ReflectPolicy::TYPE.0 | ReflectPolicy::FIELDS.0),
            true,
            vec![
                FieldMeta { name: "mX".into(), ty: IrType::I32, field_index: 1 },
                FieldMeta { name: "mY".into(), ty: IrType::I32, field_index: 2 },
            ],
            vec![],
        ));

        // The in-module reflection accessor is DEFINED by `emit_metadata`; declare
        // it as an extern so our `get_field0` can call it by name (the metadata
        // pass fills in the body — exactly the sandbox clone's situation).
        m.declare_extern(
            "__newbf_type_by_id",
            vec![Param { name: None, ty: IrType::I32 }],
            IrType::Ptr,
        );

        // A host-visible global the wrapper writes the reflected `mName` into, so
        // the test reads the result back out of the JIT'd module.
        m.add_global(newbf_ir::GlobalDef { name: "g_name".into(), ty: IrType::Ptr });

        // `FieldInfo get_field0(i32 id)`: the value-struct reflection return.
        // t = __newbf_type_by_id(id); fields = t.mFields; return fields[0]; (by value)
        {
            let mut f = FunctionBuilder::new(
                "get_field0",
                vec![Param { name: Some("id".into()), ty: IrType::I32 }],
                IrType::Struct(fieldinfo_struct),
            );
            let t = f.call("__newbf_type_by_id", vec![f.param(0)], IrType::Ptr);
            // mFields is Type field index 6 (a `FieldInfo*`).
            let fields_pp = f.field_addr(t, type_id_struct, 6);
            let fields = f.load(fields_pp, IrType::Ptr);
            // fields[0] — element 0 of the `[k x %FieldInfo]` array, by value.
            let e0 = f.elem_addr(fields, IrType::Struct(fieldinfo_struct), Value::int(0, IrType::I64));
            let fi = f.load(e0, IrType::Struct(fieldinfo_struct));
            f.ret(Some(fi));
            m.add_function(f.finish());
        }

        // `void $ct_emit_run()`: the sandbox wrapper. Calls get_field0, BINDS the
        // returned FieldInfo into a local alloca (R5: never chain off the rvalue),
        // reads `.mName` from the bound local, and stores it into `g_name`.
        {
            let mut w = FunctionBuilder::new("$ct_emit_run", vec![], IrType::Void);
            let fi = w.call("get_field0", vec![Value::int(0, IrType::I32)], IrType::Struct(fieldinfo_struct));
            // Bind the value-struct return into a local (the bound-FieldInfo-local
            // pattern the design mandates before reading a member).
            let slot = w.alloca(IrType::Struct(fieldinfo_struct));
            w.store(slot.clone(), fi);
            // local.mName  (FieldInfo field 0, a char8*).
            let name_pp = w.field_addr(slot, fieldinfo_struct, 0);
            let name = w.load(name_pp, IrType::Ptr);
            let gp = w.global_addr("g_name");
            w.store(gp, name);
            w.ret(None);
            m.add_function(w.finish());
        }

        // JIT the sandbox-shaped module exactly as `run_generators` does.
        let jit = OrcJit::from_ir(&m).expect("sandbox-shaped module JIT-links clean");

        // (i) The reflection accessor + the per-type Type global resolve in the
        //     sandbox-shaped JIT (CR §3.1 — the metadata IS in the clone).
        assert!(
            jit.lookup("__newbf_type_by_id").is_some(),
            "the in-module __newbf_type_by_id accessor resolves in the sandbox-shaped module"
        );
        assert!(
            jit.lookup("Point.$type").is_some(),
            "the per-type `Point.$type` %struct.Type global resolves in the sandbox-shaped module"
        );

        // (ii) Run the `$ct_emit_run` wrapper: the value-struct FieldInfo return is
        //      present AND callable inside the wrapper. It writes the reflected
        //      first-field name into `g_name`; read it back and confirm "mX".
        let run_addr = jit.lookup("$ct_emit_run").expect("$ct_emit_run resolves");
        let run: extern "C" fn() = unsafe { std::mem::transmute(run_addr) };
        run();

        let gname_addr = jit.lookup("g_name").expect("g_name global resolves");
        // SAFETY: `g_name` is a `ptr` global; after `run()` it holds the `char8*`
        // reflected name (a `.rodata` cstr the JIT'd module owns while `jit` lives).
        let name_ptr: *const u8 = unsafe { *(gname_addr as *const *const u8) };
        let got = unsafe { cstr_to_string(name_ptr) };
        assert_eq!(
            got, "mX",
            "the value-struct FieldInfo return read inside $ct_emit_run yields the \
             reflected first-field name — struct-by-value reflection is present AND \
             callable in the sandbox-shaped module (R5)"
        );
    }

    /// strip_emitter_and_shim removes a comptime fn that calls `__newbf_ct_emit`
    /// and the extern, but keeps an ordinary emitted member.
    #[test]
    fn strip_drops_generator_and_shim_keeps_members() {
        let mut m = IrModule::new("strip");
        m.declare_extern(
            "__newbf_ct_emit",
            vec![
                Param { name: None, ty: IrType::I32 },
                Param { name: None, ty: IrType::Ptr },
                Param { name: None, ty: IrType::I32 },
            ],
            IrType::Void,
        );
        // The generator (comptime, calls the shim).
        let mut g = FunctionBuilder::new("Owner.Generate", vec![], IrType::Void);
        g.call(
            "__newbf_ct_emit",
            vec![
                Value::int(0, IrType::I32),
                Value::str("x"),
                Value::int(1, IrType::I32),
            ],
            IrType::Void,
        );
        g.ret(None);
        m.add_function(g.finish());
        m.comptime.push("Owner.Generate".to_string());
        // An ordinary emitted member (not comptime, no shim ref).
        let mut s = FunctionBuilder::new("Owner.Sum", vec![], IrType::I32);
        s.ret(Some(Value::int(42, IrType::I32)));
        m.add_function(s.finish());

        let out = strip_emitter_and_shim(m);
        assert!(!out.funcs.iter().any(|f| f.name == "__newbf_ct_emit"));
        assert!(!out.funcs.iter().any(|f| f.name == "Owner.Generate"));
        assert!(
            out.funcs.iter().any(|f| f.name == "Owner.Sum"),
            "the emitted member must survive the strip"
        );
    }

    /// **The load-bearing seam proof (CB-T2 acceptance gate, retained).** JIT a
    /// nullary fn that calls the host shim `__newbf_ct_emit(owner_id, ptr, len)`,
    /// bind the shim by address via `OrcJit::add_absolute_symbol`, run it, and
    /// assert `EMIT_SINK` now holds `(owner_id, text)` — with no
    /// duplicate-definition error (the absolute shim wins over the process
    /// generator). This is exactly what `run_generators` relies on.
    #[test]
    fn shim_populates_sink_via_absolute_symbol() {
        let _ = drain_emit_sink();

        const OWNER_ID: i32 = 4242;
        let text = "public int SumXY() { return x + y; }";

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

        let jit = OrcJit::from_ir(&m).expect("comptime sandbox jit builds");
        jit.add_absolute_symbol("__newbf_ct_emit", __newbf_ct_emit as *const () as usize)
            .expect("absolute shim binds with no duplicate-definition error");

        let addr = jit.lookup("emit_one").expect("emit_one resolves");
        let run: extern "C" fn() = unsafe { std::mem::transmute(addr) };
        run();

        let drained = drain_emit_sink();
        assert_eq!(
            drained,
            vec![(OWNER_ID, text.to_string())],
            "the host shim received (owner_id, text) from JIT'd code"
        );
    }

    // ── CB-T5: fixpoint guards + diagnostics ──────────────────────────────────

    /// A base program with a real `[Comptime, EmitGenerator]` so the lowered
    /// module records a non-empty `emit_jobs` (the loop only invokes the
    /// generator-runner when generators exist). The integration tests inject a
    /// *synthetic* generator-runner via `run_emission_inner` to drive the
    /// round/byte caps deterministically (a JIT'd generator cannot emit
    /// per-round-divergent text without host state — comptime-breadth §3.4).
    const DIVERGENT_BASE_SRC: &str = r#"
        class Bag {
            public int32 mN;
            [Comptime, EmitGenerator]
            public static void Generate() {
                Compiler.EmitTypeBody("public int32 Tag0() { return this.mN; }");
            }
        }
        class Program { public static int32 Main() { return 0; } }
    "#;

    /// Build the borrowed `SourceFile` set for a one-file base program, calling
    /// `run_emission_inner` with a caller-supplied generator-runner + caps.
    fn run_inner(
        src: &str,
        config: EmitConfig,
        run_gens: impl FnMut(&IrModule) -> Result<Vec<(i32, String)>, String>,
    ) -> (IrModule, EmitOutcome) {
        let (unit, pdiags) = parse_file(src, FileId(0));
        assert!(pdiags.is_empty(), "parse diagnostics: {pdiags:?}");
        let files = [SourceFile {
            file: FileId(0),
            src,
            unit: &unit,
            name: "",
        }];
        run_emission_inner(&files, &config, run_gens).expect("inner emission returns Ok")
    }

    /// Resolve a valid owner id for the round (the id `Bag` is registered under),
    /// so the synthetic emitter routes its text to a real type.
    fn an_owner_id(module: &IrModule) -> i32 {
        *owner_id_to_qual(module)
            .keys()
            .next()
            .expect("the lowered module has at least one emit-job owner")
    }

    /// **The round-cap diagnostic (CB-T5 acceptance — returning-but-divergent).**
    /// A generator that returns normally but emits **unique text each round**
    /// never reaches the `seen`-dedup fixpoint. With a small round cap the loop
    /// must STOP and surface a structured non-convergence diagnostic — **no
    /// crash, no hang, no `Err`**.
    #[test]
    fn divergent_emitter_trips_round_cap_with_diagnostic() {
        let mut n = 0u32;
        let (_module, outcome) = run_inner(
            DIVERGENT_BASE_SRC,
            EmitConfig {
                max_rounds: 4,
                // Generous byte cap so the ROUND cap is the thing under test.
                max_bytes: 1 << 30,
            },
            |module| {
                let owner = an_owner_id(module);
                // A unique (small) method each round → always "new" → never dedups.
                let text = format!("public int32 R{n}() {{ return {n}; }}");
                n += 1;
                Ok(vec![(owner, text)])
            },
        );
        assert_eq!(
            outcome.rounds, 4,
            "the loop stops exactly at the round cap (no hang)"
        );
        assert_eq!(
            outcome.diagnostics.len(),
            1,
            "exactly one round-cap diagnostic, got: {:?}",
            outcome.diagnostics
        );
        assert!(
            outcome.diagnostics[0].contains("did not converge"),
            "the diagnostic names non-convergence: {:?}",
            outcome.diagnostics
        );
    }

    /// **The byte-cap diagnostic (CB-T5 acceptance — runaway growth).** A
    /// generator that emits a large chunk of *unique* text each round defeats the
    /// `seen` dedup (every emission is new) but is bounded by the total-output
    /// byte cap: the loop STOPs with a structured runaway-growth diagnostic
    /// **before** the round cap, with no hang/crash.
    #[test]
    fn growing_emitter_trips_byte_cap_with_diagnostic() {
        let mut n = 0u32;
        let (_module, outcome) = run_inner(
            DIVERGENT_BASE_SRC,
            EmitConfig {
                // High round cap so the BYTE cap trips first.
                max_rounds: 1000,
                max_bytes: 4096,
            },
            |module| {
                let owner = an_owner_id(module);
                // ~2 KiB of unique text per round → the 4 KiB cap trips by round 3.
                let filler = "x".repeat(2000);
                let text = format!("public int32 Big{n}() {{ /* {filler} */ return {n}; }}");
                n += 1;
                Ok(vec![(owner, text)])
            },
        );
        assert!(
            outcome.rounds < 1000,
            "the byte cap stops the loop well before the round cap (rounds={})",
            outcome.rounds
        );
        assert_eq!(
            outcome.diagnostics.len(),
            1,
            "exactly one byte-cap diagnostic, got: {:?}",
            outcome.diagnostics
        );
        assert!(
            outcome.diagnostics[0].contains("byte"),
            "the diagnostic names the byte cap: {:?}",
            outcome.diagnostics
        );
    }

    /// **Idempotency / fixpoint (CB-T5 dedup).** A generator that re-emits the
    /// **same** (normalized) text every round converges via the `seen` dedup: it
    /// is spliced once, the next round emits the identical (now-deduped) text, and
    /// the loop reaches a clean fixpoint with NO diagnostics — even under a tiny
    /// round cap that a divergent emitter would trip.
    #[test]
    fn idempotent_emitter_converges_no_diagnostic() {
        let (_module, outcome) = run_inner(
            DIVERGENT_BASE_SRC,
            EmitConfig {
                max_rounds: 3,
                max_bytes: 1 << 20,
            },
            |module| {
                let owner = an_owner_id(module);
                // Identical text every round (only cosmetic whitespace varies, which
                // `normalize` collapses) → deduped after the first splice → fixpoint.
                Ok(vec![(
                    owner,
                    "public int32 Stable()  {  return 1;  }".to_string(),
                )])
            },
        );
        assert!(
            outcome.diagnostics.is_empty(),
            "an idempotent emitter converges with no diagnostic, got: {:?}",
            outcome.diagnostics
        );
        // 2 rounds: round 1 splices the member, round 2 emits the identical text
        // (deduped → nothing new) → fixpoint.
        assert!(
            outcome.rounds <= 2,
            "idempotent emission reaches the fixpoint in ≤2 rounds (got {})",
            outcome.rounds
        );
    }

    /// **Abort on generated-code analyze diagnostics (CB-T5 acceptance).** A
    /// generator emits a member that, once spliced as `extension Bag { … }`,
    /// produces an **analyze diagnostic** (here a duplicate field — what this
    /// compiler's `analyze` catches; see the report's deviation note). The loop
    /// must surface the analyze diagnostic into the outcome and STOP — not lower
    /// garbage IR (a silent miscompile) and not loop forever.
    #[test]
    fn generated_code_analyze_diagnostic_aborts_with_diagnostic() {
        let (_module, outcome) = run_inner(
            DIVERGENT_BASE_SRC,
            EmitConfig::default(),
            |module| {
                let owner = an_owner_id(module);
                // Two identical fields in ONE emitted extension unit → analyze's
                // `check_duplicate_members` fires `duplicate member`.
                Ok(vec![(
                    owner,
                    "public int32 dupF; public int32 dupF;".to_string(),
                )])
            },
        );
        assert!(
            !outcome.diagnostics.is_empty(),
            "a malformed (analyze-erroring) emission must surface a diagnostic"
        );
        assert!(
            outcome
                .diagnostics
                .iter()
                .any(|d| d.contains("generated source produced")),
            "an abort header names generated-source analyze diagnostics: {:?}",
            outcome.diagnostics
        );
        assert!(
            outcome
                .diagnostics
                .iter()
                .any(|d| d.contains("duplicate member")),
            "the underlying analyze diagnostic is surfaced verbatim: {:?}",
            outcome.diagnostics
        );
    }

    /// The default caps leave a well-behaved (no-generator) program completely
    /// unaffected — the fast path runs zero rounds with no diagnostics, and the
    /// `EmitConfig` defaults match the documented values (corpus unaffected).
    #[test]
    fn default_config_matches_documented_caps() {
        let cfg = EmitConfig::default();
        assert_eq!(cfg.max_rounds, super::DEFAULT_MAX_EMIT_ROUNDS);
        assert_eq!(cfg.max_bytes, super::DEFAULT_MAX_EMIT_BYTES);
        assert_eq!(cfg.max_rounds, 16);
        assert_eq!(cfg.max_bytes, 1 << 20);
    }
}
