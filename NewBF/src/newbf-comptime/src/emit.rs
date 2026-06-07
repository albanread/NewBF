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

/// Anti-cycle backstop only (comptime-breadth §3.4 / CB-T5 hardens it): the
/// dedup `seen` set makes identical emissions idempotent, so a well-behaved
/// generator stabilizes in 1–3 rounds. This caps the *number* of rounds for a
/// generator that returns normally but emits divergent text each round, turning
/// a would-be infinite loop into an error rather than a hang. (An emitter with
/// an *internal* infinite loop still hangs — bounded execution is deferred.)
const MAX_EMIT_ROUNDS: u32 = 16;

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
/// codegen-ready module (comptime-breadth §3.1, §5.3).
///
/// `base` is the user's parsed source (the same `&[SourceFile]` the driver and
/// run-corpus harness build and hand to `analyze` / `lower_program`).
/// `run_emission` re-analyzes + re-lowers internally — lowering is a pure
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
/// and re-lower. Stops at fixpoint (nothing new) or trips [`MAX_EMIT_ROUNDS`].
///
/// **The strip (R6, load-bearing).** Before returning, every emitter generator
/// (a `module.comptime` symbol that transitively references `__newbf_ct_emit`)
/// and the `__newbf_ct_emit` extern itself are removed — the app/run JIT and the
/// AOT link never register the shim, so a survivor would fail `lookup`/link. The
/// emitted members (ordinary reparsed source) stay.
pub fn run_emission(base: &[SourceFile<'_>]) -> Result<(IrModule, EmitOutcome), String> {
    // The owned synthesized `extension` units, kept alive for the whole loop so
    // each round's borrowed `SourceFile` set can reference them (comptime-breadth
    // §3.4 ownership model). `String` (source) + `CompUnit` (its parse tree).
    let mut generated: Vec<(String, CompUnit)> = Vec::new();
    // Normalized (owner, text) pairs already spliced — the idempotency guard.
    let mut seen: HashSet<EmitKey> = HashSet::new();
    let mut rounds: u32 = 0;

    loop {
        // (a) Build the round's source set = base + generated, then lower it.
        //     The generated FileIds sit well above the prelude/user bands.
        let mut files: Vec<SourceFile<'_>> = base
            .iter()
            .map(|f| SourceFile {
                file: f.file,
                src: f.src,
                unit: f.unit,
            })
            .collect();
        for (i, (src, unit)) in generated.iter().enumerate() {
            files.push(SourceFile {
                file: FileId(GENERATED_FILE_BASE + i as u32),
                src: src.as_str(),
                unit,
            });
        }
        let program = analyze(&files);
        let module = lower_program(&files, &program);

        // (b) Fast path / fixpoint exit: no generators recorded → done.
        if module.emit_jobs.is_empty() {
            let final_module = strip_emitter_and_shim(module);
            return Ok((final_module, EmitOutcome { rounds, diagnostics: Vec::new() }));
        }

        // Anti-cycle backstop: a generator that returns but emits divergent text
        // each round never reaches the fixpoint; cap the rounds (CB-T5 also adds
        // a byte cap + surfaces this as a structured diagnostic).
        if rounds >= MAX_EMIT_ROUNDS {
            return Err(format!(
                "comptime: emission did not reach a fixpoint within {MAX_EMIT_ROUNDS} rounds \
                 (a generator likely emits divergent text each round)"
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
        let emissions = run_generators(&module)?;

        // (e) Resolve + normalize + dedup; splice each NEW emission as an
        //     `extension`. Process in a deterministic order so the next round's
        //     StructId assignment (and thus any further emission) is reproducible.
        let mut new_units: Vec<(String, String)> = Vec::new();
        for (owner_id, text) in emissions {
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
    // Bind the host shim by address BEFORE the lookup (the absolute definition
    // wins over the process-search generator → no duplicate-definition error).
    jit.add_absolute_symbol("__newbf_ct_emit", __newbf_ct_emit as *const () as usize)
        .map_err(|e| format!("comptime: binding __newbf_ct_emit failed: {e}"))?;
    let addr = jit.lookup("$ct_emit_run").ok_or_else(|| {
        "comptime: emission wrapper `$ct_emit_run` did not resolve in the sandbox JIT".to_string()
    })?;
    // SAFETY: `$ct_emit_run` is a nullary `void` fn just built above; `addr` is
    // its entry point in JIT'd memory, mapped while `jit` is alive (it is, until
    // this fn returns after the call). The generators run in-process and push
    // into EMIT_SINK via the bound shim.
    let run: extern "C" fn() = unsafe { std::mem::transmute(addr) };
    run();

    Ok(drain_emit_sink())
}

/// Strip the emitter generators + the `__newbf_ct_emit` extern from the final
/// module (comptime-breadth §5.4 / R6 — load-bearing for the app/run JIT and AOT
/// link, neither of which registers the shim).
///
/// Droppable = any function that **transitively references `__newbf_ct_emit`**
/// (i.e. the generators and anything that calls into them, computed by a reverse
/// reachability sweep) **and** is a `module.comptime` symbol — plus the
/// `__newbf_ct_emit` extern declaration itself. The emitted members (ordinary
/// reparsed source, *not* comptime, *not* referencing the shim) are kept. This
/// agrees with `fold_comptime`'s `reachable_from_ordinary` view: both treat
/// `module.comptime` members reaching `__newbf_ct_emit` as droppable, so neither
/// keeps a function the other needs.
fn strip_emitter_and_shim(mut module: IrModule) -> IrModule {
    let comptime: HashSet<&str> = module.comptime.iter().map(String::as_str).collect();

    // Functions that reference `__newbf_ct_emit` directly.
    let mut references_shim: HashSet<String> = HashSet::new();
    for f in &module.funcs {
        if f.insts.iter().any(|i| {
            matches!(&i.kind, InstKind::Call { callee, .. } if callee.name == "__newbf_ct_emit")
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

    // Drop: the `__newbf_ct_emit` extern itself, plus every comptime function
    // that (transitively) references the shim — i.e. the emitter generators.
    module.funcs.retain(|f| {
        if f.name == "__newbf_ct_emit" {
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
        __newbf_ct_emit, drain_emit_sink, normalize, run_emission, strip_emitter_and_shim,
        EmitOutcome,
    };
    use newbf_ir::{FunctionBuilder, IrType, Module as IrModule, Param, Value};
    use newbf_lexer::FileId;
    use newbf_llvm::OrcJit;
    use newbf_parser::parse_file;
    use newbf_sema::SourceFile;

    /// Parse one `.bf` source and drive emission over it, returning the final
    /// module (the same shape the driver/harness use).
    fn emit_over(src: &str) -> (IrModule, EmitOutcome) {
        let (unit, pdiags) = parse_file(src, FileId(0));
        assert!(pdiags.is_empty(), "parse diagnostics: {pdiags:?}");
        let files = [SourceFile {
            file: FileId(0),
            src,
            unit: &unit,
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
}
