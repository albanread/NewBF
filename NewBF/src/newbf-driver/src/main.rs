//! `newbf-driver` — the NewBF command-line driver.
//!
//! Orchestrates the compiler pipeline and exposes a `dump-<phase>`
//! subcommand for every phase, per the phase-report convention
//! (MANIFESTO core decision 12). At Sprint 01 only `--version` is live;
//! the phase subcommands are stubs that land sprint by sprint.

use clap::{Parser, Subcommand};

/// Version banner. The LLVM backend is pinned but inactive until the
/// LLVM sprint, so the banner advertises it as `pending`.
const VERSION_BANNER: &str = concat!(env!("CARGO_PKG_VERSION"), " (LLVM 22.1, pending)");

#[derive(Parser)]
#[command(
    name = "newbf-driver",
    version = VERSION_BANNER,
    about = "NewBF — a Rust + LLVM compiler for the Beef language",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Compile a .bf file or directory to a native `.exe` (AOT): parse →
    /// analyze → lower → emit object → link. Win32 imports the program calls
    /// are discovered via sema and their import libs linked automatically.
    Compile {
        /// Path to a `.bf` source file or a directory.
        input: String,
        /// Output `.exe` path (default: the input with a `.exe` extension).
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Start the REPL (the JIT). Lands in phase 3.
    Repl,
    /// Dump the token stream for a .bf file (the lexer phase report).
    DumpTokens {
        /// Path to a `.bf` source file.
        input: String,
    },
    /// Dump the parsed AST of a .bf statement fragment (the parser phase
    /// report). Whole-file parsing (with declarations) lands in Sprint 04.
    DumpParse {
        /// Path to a `.bf` source file (a statement fragment).
        input: String,
    },
    /// Dump the parsed AST of a whole .bf file (the declaration-parser
    /// phase report — using directives, namespaces, types, members).
    DumpAst {
        /// Path to a `.bf` source file.
        input: String,
    },
    /// Dump the definition graph for a .bf file or a directory of them (the
    /// sema phase report — namespaces, types with full shapes, members,
    /// usings). A directory is walked recursively and analyzed as one
    /// program (open namespaces merge across files).
    DumpDefs {
        /// Path to a `.bf` source file or a directory.
        input: String,
    },
    /// Dump the typed SSA IR for a .bf file or directory (the IR phase
    /// report). Sprint 06b lowers the primitive kernel; richer constructs
    /// are skipped without panicking.
    DumpIr {
        /// Path to a `.bf` source file or a directory.
        input: String,
    },
    /// Dump the LLVM IR for a .bf file or directory (the backend phase
    /// report). Lowers the typed SSA IR to LLVM via inkwell; the same
    /// module feeds the ORC JIT and AOT object emission.
    DumpLlvm {
        /// Path to a `.bf` source file or a directory.
        input: String,
    },
    /// Symbolicate a crash dump's raw hex IPs against a linker `.map` —
    /// frames-with-names for our own code, no dbghelp/PDB needed.
    Symbolicate {
        /// `.map` emitted by the linker; `compile` produces `<exe>.map`.
        #[arg(short, long)]
        map: String,
        /// Crash-dump file to rewrite (default: stdin).
        input: Option<String>,
        /// Write the rewritten dump here (default: stdout).
        #[arg(short, long)]
        output: Option<String>,
        /// Runtime exe base in hex for the ASLR slide (default: the `.map`'s
        /// preferred base).
        #[arg(long)]
        runtime_base: Option<String>,
    },
}

fn main() {
    // Arm the compiler process first thing: a fault (ours, or a JIT'd /
    // comptime fault running in-process) prints a signal-safe crash dump
    // instead of dying silently (MANIFESTO core decision 16).
    newbf_runtime::install_crash_handler();

    // MS-T3: enable the debug memory guard for the AOT-less JIT/`run` paths.
    // The MODE atomic lives in *this* host process's `newbf-runtime`; JIT'd
    // Beef code's `newbf_alloc`/`newbf_free` (resolved via the MS-T0 absolute-
    // symbol seam) then route through the quarantining stomp allocator so a
    // UAF faults and a double-free aborts (memory-safety.md §A5). Stomp on a
    // debug driver, Thunk passthrough in release (the strip is by the host's
    // own profile here, distinct from AOT's per-target-program profile — A5).
    #[cfg(debug_assertions)]
    newbf_runtime::set_guard_mode(newbf_runtime::GuardMode::Stomp);

    let cli = Cli::parse();
    match cli.command {
        None => {
            // Bare invocation prints the same banner as `--version`, so
            // the Sprint 01 demo works either way.
            println!("newbf-driver {VERSION_BANNER}");
        }
        Some(Command::Compile { input, output }) => compile(&input, output.as_deref()),
        Some(Command::Repl) => {
            eprintln!("repl: not yet implemented (SPRINTS.md phase 3)");
        }
        Some(Command::DumpTokens { input }) => match std::fs::read_to_string(&input) {
            Ok(src) => {
                let tokens = newbf_lexer::lex(&src, newbf_lexer::FileId(0));
                print!("{}", newbf_lexer::format_tokens(&src, &tokens));
            }
            Err(e) => {
                eprintln!("dump-tokens: cannot read {input}: {e}");
                std::process::exit(1);
            }
        },
        Some(Command::DumpParse { input }) => match std::fs::read_to_string(&input) {
            Ok(src) => {
                let (stmts, diags) = newbf_parser::parse_fragment(&src, newbf_lexer::FileId(0));
                print!("{}", newbf_parser::format_parse(&src, &stmts));
                for d in &diags {
                    eprintln!("{}..{}: {}", d.span.lo, d.span.hi, d.message);
                }
                if !diags.is_empty() {
                    std::process::exit(1);
                }
            }
            Err(e) => {
                eprintln!("dump-parse: cannot read {input}: {e}");
                std::process::exit(1);
            }
        },
        Some(Command::DumpAst { input }) => match std::fs::read_to_string(&input) {
            Ok(src) => {
                let (unit, diags) = newbf_parser::parse_file(&src, newbf_lexer::FileId(0));
                print!("{}", newbf_parser::format_ast(&src, &unit));
                for d in &diags {
                    eprintln!("{}..{}: {}", d.span.lo, d.span.hi, d.message);
                }
                if !diags.is_empty() {
                    std::process::exit(1);
                }
            }
            Err(e) => {
                eprintln!("dump-ast: cannot read {input}: {e}");
                std::process::exit(1);
            }
        },
        Some(Command::DumpDefs { input }) => dump_defs(&input),
        Some(Command::DumpIr { input }) => dump_ir(&input),
        Some(Command::DumpLlvm { input }) => dump_llvm(&input),
        Some(Command::Symbolicate {
            map,
            input,
            output,
            runtime_base,
        }) => symbolicate_cmd(
            &map,
            input.as_deref(),
            output.as_deref(),
            runtime_base.as_deref(),
        ),
    }
}

/// Collect `.bf` files: just `path` if it's a file, else every `.bf` under
/// it (recursively). Returns paths sorted for deterministic FileIds.
fn collect_bf(path: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    if path.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            let mut children: Vec<_> = entries.flatten().map(|e| e.path()).collect();
            children.sort();
            for c in children {
                collect_bf(&c, out);
            }
        }
    } else if path.extension().and_then(|x| x.to_str()) == Some("bf") {
        out.push(path.to_path_buf());
    }
}

fn dump_defs(input: &str) {
    let root = std::path::Path::new(input);
    let mut paths = Vec::new();
    if root.is_file() {
        paths.push(root.to_path_buf());
    } else {
        collect_bf(root, &mut paths);
    }
    if paths.is_empty() {
        eprintln!("dump-defs: no .bf files at {input}");
        std::process::exit(1);
    }

    // Read + parse every file; keep sources and units alive for the borrow.
    let mut srcs: Vec<String> = Vec::with_capacity(paths.len());
    for path in &paths {
        match std::fs::read_to_string(path) {
            Ok(s) => srcs.push(s),
            Err(e) => {
                eprintln!("dump-defs: cannot read {}: {e}", path.display());
                std::process::exit(1);
            }
        }
    }
    let mut units = Vec::with_capacity(paths.len());
    let mut parse_diags = 0usize;
    for (i, src) in srcs.iter().enumerate() {
        let (unit, diags) = newbf_parser::parse_file(src, newbf_lexer::FileId(i as u32));
        parse_diags += diags.len();
        units.push(unit);
    }
    let files: Vec<newbf_sema::SourceFile<'_>> = srcs
        .iter()
        .zip(units.iter())
        .enumerate()
        .map(|(i, (src, unit))| newbf_sema::SourceFile {
            file: newbf_lexer::FileId(i as u32),
            src,
            unit,
        })
        .collect();

    let program = newbf_sema::analyze(&files);
    print!("{}", newbf_sema::format_defs(&program));

    if parse_diags > 0 {
        eprintln!(
            "(note: {parse_diags} parse diagnostics across {} files)",
            paths.len()
        );
    }
    for d in &program.diagnostics {
        eprintln!("{}..{}: {}", d.span.lo, d.span.hi, d.message);
    }
    if !program.diagnostics.is_empty() {
        std::process::exit(1);
    }
}

fn dump_ir(input: &str) {
    let root = std::path::Path::new(input);
    let mut paths = Vec::new();
    if root.is_file() {
        paths.push(root.to_path_buf());
    } else {
        collect_bf(root, &mut paths);
    }
    if paths.is_empty() {
        eprintln!("dump-ir: no .bf files at {input}");
        std::process::exit(1);
    }

    let mut srcs: Vec<String> = Vec::with_capacity(paths.len());
    for path in &paths {
        match std::fs::read_to_string(path) {
            Ok(s) => srcs.push(s),
            Err(e) => {
                eprintln!("dump-ir: cannot read {}: {e}", path.display());
                std::process::exit(1);
            }
        }
    }
    let mut units = Vec::with_capacity(paths.len());
    let mut parse_diags = 0usize;
    for (i, src) in srcs.iter().enumerate() {
        let (unit, diags) = newbf_parser::parse_file(src, newbf_lexer::FileId(i as u32));
        parse_diags += diags.len();
        units.push(unit);
    }
    let files: Vec<newbf_sema::SourceFile<'_>> = srcs
        .iter()
        .zip(units.iter())
        .enumerate()
        .map(|(i, (src, unit))| newbf_sema::SourceFile {
            file: newbf_lexer::FileId(i as u32),
            src,
            unit,
        })
        .collect();

    // Drive comptime member emission to a fixpoint (CB-T4). `run_emission`
    // analyzes + lowers internally each round, splices emitted `extension`s, and
    // strips the emitter/shim before returning (a no-op fast path when the
    // program records no emit generators). Runs before value folding: emission
    // changes the program's *shape*, folding its values.
    let (mut module, emit) = match newbf_comptime::run_emission(&files) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("dump-ir: comptime emission failed: {e}");
            std::process::exit(1);
        }
    };
    // Merge emission diagnostics (a divergent/erroring emitter — the round/byte
    // caps or a generated-code analyze diagnostic) into the diagnostic stream,
    // surfaced like parse/sema diagnostics: report and fail rather than codegen a
    // module from a non-converged or malformed emission (CB-T5).
    if !emit.diagnostics.is_empty() {
        for d in &emit.diagnostics {
            eprintln!("dump-ir: {d}");
        }
        std::process::exit(1);
    }
    // Fold comptime call sites so the IR report reflects the real compiled
    // output (a no-op for programs without `[Comptime]`).
    if let Err(e) = newbf_comptime::fold_comptime(&mut module) {
        eprintln!("dump-ir: comptime evaluation failed: {e}");
        std::process::exit(1);
    }
    print!("{}", newbf_ir::format_ir(&module));

    if parse_diags > 0 {
        eprintln!(
            "(note: {parse_diags} parse diagnostics across {} files)",
            paths.len()
        );
    }
}

fn dump_llvm(input: &str) {
    let root = std::path::Path::new(input);
    let mut paths = Vec::new();
    if root.is_file() {
        paths.push(root.to_path_buf());
    } else {
        collect_bf(root, &mut paths);
    }
    if paths.is_empty() {
        eprintln!("dump-llvm: no .bf files at {input}");
        std::process::exit(1);
    }

    let mut srcs: Vec<String> = Vec::with_capacity(paths.len());
    for path in &paths {
        match std::fs::read_to_string(path) {
            Ok(s) => srcs.push(s),
            Err(e) => {
                eprintln!("dump-llvm: cannot read {}: {e}", path.display());
                std::process::exit(1);
            }
        }
    }
    let mut units = Vec::with_capacity(paths.len());
    let mut parse_diags = 0usize;
    for (i, src) in srcs.iter().enumerate() {
        let (unit, diags) = newbf_parser::parse_file(src, newbf_lexer::FileId(i as u32));
        parse_diags += diags.len();
        units.push(unit);
    }
    let files: Vec<newbf_sema::SourceFile<'_>> = srcs
        .iter()
        .zip(units.iter())
        .enumerate()
        .map(|(i, (src, unit))| newbf_sema::SourceFile {
            file: newbf_lexer::FileId(i as u32),
            src,
            unit,
        })
        .collect();

    // Drive comptime member emission to a fixpoint (CB-T4): analyze + lower +
    // splice emitted `extension`s + strip the emitter/shim, internally (a no-op
    // fast path when no emit generators are recorded). Runs before value folding.
    let (mut module, emit) = match newbf_comptime::run_emission(&files) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("dump-llvm: comptime emission failed: {e}");
            std::process::exit(1);
        }
    };
    // Merge emission diagnostics into the diagnostic stream (CB-T5): a tripped
    // round/byte cap or a generated-code analyze diagnostic is reported and fails
    // the build rather than being silently codegen'd.
    if !emit.diagnostics.is_empty() {
        for d in &emit.diagnostics {
            eprintln!("dump-llvm: {d}");
        }
        std::process::exit(1);
    }
    // Fold comptime call sites so the LLVM report reflects the real compiled
    // output (a no-op for programs without `[Comptime]`).
    if let Err(e) = newbf_comptime::fold_comptime(&mut module) {
        eprintln!("dump-llvm: comptime evaluation failed: {e}");
        std::process::exit(1);
    }
    print!("{}", newbf_llvm::lower_to_string(&module));

    if parse_diags > 0 {
        eprintln!(
            "(note: {parse_diags} parse diagnostics across {} files)",
            paths.len()
        );
    }
}

fn compile(input: &str, output: Option<&str>) {
    let root = std::path::Path::new(input);
    let mut paths = Vec::new();
    if root.is_file() {
        paths.push(root.to_path_buf());
    } else {
        collect_bf(root, &mut paths);
    }
    if paths.is_empty() {
        eprintln!("compile: no .bf files at {input}");
        std::process::exit(1);
    }

    let mut srcs: Vec<String> = Vec::with_capacity(paths.len());
    for path in &paths {
        match std::fs::read_to_string(path) {
            Ok(s) => srcs.push(s),
            Err(e) => {
                eprintln!("compile: cannot read {}: {e}", path.display());
                std::process::exit(1);
            }
        }
    }
    let mut units = Vec::with_capacity(paths.len());
    let mut parse_diags = 0usize;
    for (i, src) in srcs.iter().enumerate() {
        let (unit, diags) = newbf_parser::parse_file(src, newbf_lexer::FileId(i as u32));
        parse_diags += diags.len();
        units.push(unit);
    }
    let files: Vec<newbf_sema::SourceFile<'_>> = srcs
        .iter()
        .zip(units.iter())
        .enumerate()
        .map(|(i, (src, unit))| newbf_sema::SourceFile {
            file: newbf_lexer::FileId(i as u32),
            src,
            unit,
        })
        .collect();

    // `analyze` here feeds Win32 import discovery below (which keys off the
    // user's source, unaffected by emission). Emission re-analyzes + re-lowers
    // internally.
    let program = newbf_sema::analyze(&files);

    // Drive comptime member emission to a fixpoint before codegen (CB-T4):
    // analyze + lower + splice emitted `extension`s + strip the emitter/shim,
    // internally (a no-op fast path when no emit generators are recorded).
    // Emission changes the program's *shape* (new members); it must run before
    // value folding, which collapses values.
    let (mut module, emit) = match newbf_comptime::run_emission(&files) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("compile: comptime emission failed: {e}");
            std::process::exit(1);
        }
    };
    // Merge emission diagnostics into the diagnostic stream (CB-T5): a tripped
    // round/byte cap or a generated-code analyze diagnostic is reported and fails
    // the compile rather than producing a binary from a non-converged or
    // malformed emission.
    if !emit.diagnostics.is_empty() {
        for d in &emit.diagnostics {
            eprintln!("compile: {d}");
        }
        std::process::exit(1);
    }

    // Evaluate `[Comptime]` functions and fold their call sites into literals
    // (then drop them) before codegen — they are compile-time-only.
    if let Err(e) = newbf_comptime::fold_comptime(&mut module) {
        eprintln!("compile: comptime evaluation failed: {e}");
        std::process::exit(1);
    }

    // Discover the Win32 APIs the program imports, resolve them against the
    // oracle, and collect the import libs the linker needs for the IAT.
    let imports = newbf_sema::discover_extern_methods(&program);
    let resolved = newbf_sema::resolve_apis(imports, |name| {
        newbf_winapi::find_function_any_dll(name).map(|f| (f.dll.clone(), f.params.len()))
    });
    let mut import_libs: Vec<String> = resolved
        .iter()
        .filter_map(|r| r.dll.as_deref().and_then(newbf_winapi::import_lib_for_dll))
        .collect();
    import_libs.sort();
    import_libs.dedup();

    // Emit the C `main` entry stub forwarding to the Beef entry point.
    add_main_stub(&mut module);

    let exe = match output {
        Some(o) => std::path::PathBuf::from(o),
        None => std::path::Path::new(input).with_extension("exe"),
    };
    let obj = exe.with_extension("obj");

    if let Err(e) = newbf_llvm::emit_object(&module, &obj) {
        eprintln!("compile: emitting object failed: {e}");
        std::process::exit(1);
    }
    let lib_refs: Vec<&str> = import_libs.iter().map(String::as_str).collect();
    if let Err(e) = newbf_llvm::link_executable(&[obj.as_path()], &exe, &lib_refs) {
        eprintln!("compile: link failed: {e}");
        std::process::exit(1);
    }
    let _ = std::fs::remove_file(&obj);

    if parse_diags > 0 {
        eprintln!(
            "(note: {parse_diags} parse diagnostics across {} files)",
            paths.len()
        );
    }
    if !import_libs.is_empty() {
        eprintln!(
            "(linked {} Win32 import lib(s): {})",
            import_libs.len(),
            import_libs.join(", ")
        );
    }
    println!("compiled: {}", exe.display());
}

/// Emit a C `i32 main()` entry stub forwarding to the Beef entry point (a
/// lowered `*.Main` function) so the linked exe has the CRT-expected entry.
/// If the entry returns `int32`, its value becomes the exit code; otherwise
/// the stub returns 0. (Entry args / wider return types: a later sprint.)
fn add_main_stub(module: &mut newbf_ir::Module) {
    use newbf_ir::{FunctionBuilder, IrType, Value};

    let entry = module
        .funcs
        .iter()
        .find(|f| !f.is_extern && f.name.ends_with(".Main"))
        .map(|f| (f.name.clone(), f.ret));

    let mut f = FunctionBuilder::new("main", vec![], IrType::I32);
    let code = match entry {
        Some((name, ret)) => {
            let r = f.call(name, vec![], ret);
            if ret == IrType::I32 {
                r
            } else {
                Value::int(0, IrType::I32)
            }
        }
        None => Value::int(0, IrType::I32),
    };
    f.ret(Some(code));
    module.add_function(f.finish());
}

fn symbolicate_cmd(
    map: &str,
    input: Option<&str>,
    output: Option<&str>,
    runtime_base: Option<&str>,
) {
    use std::io::{Read, Write};

    let map_text = match std::fs::read_to_string(map) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("symbolicate: read {map}: {e}");
            std::process::exit(1);
        }
    };
    let rt_base = match runtime_base {
        None => None,
        Some(s) => match u64::from_str_radix(s.trim_start_matches("0x"), 16) {
            Ok(v) => Some(v),
            Err(e) => {
                eprintln!("symbolicate: --runtime-base `{s}` is not hex: {e}");
                std::process::exit(1);
            }
        },
    };
    let crash_text = match input {
        Some(p) => match std::fs::read_to_string(p) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("symbolicate: read {p}: {e}");
                std::process::exit(1);
            }
        },
        None => {
            let mut s = String::new();
            if let Err(e) = std::io::stdin().read_to_string(&mut s) {
                eprintln!("symbolicate: stdin: {e}");
                std::process::exit(1);
            }
            s
        }
    };
    match newbf_llvm::symbolicate(&crash_text, &map_text, rt_base) {
        Ok(out) => match output {
            Some(p) => {
                if let Err(e) = std::fs::write(p, out) {
                    eprintln!("symbolicate: write {p}: {e}");
                    std::process::exit(1);
                }
            }
            None => {
                let _ = std::io::stdout().write_all(out.as_bytes());
            }
        },
        Err(e) => {
            eprintln!("symbolicate: {e}");
            std::process::exit(1);
        }
    }
}
