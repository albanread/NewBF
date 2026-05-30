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
}

fn main() {
    // Arm the compiler process first thing: a fault (ours, or a JIT'd /
    // comptime fault running in-process) prints a signal-safe crash dump
    // instead of dying silently (MANIFESTO core decision 16).
    newbf_runtime::install_crash_handler();

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

    let program = newbf_sema::analyze(&files);
    let module = newbf_sema::lower_program(&files, &program);
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

    let program = newbf_sema::analyze(&files);
    let module = newbf_sema::lower_program(&files, &program);
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

    let program = newbf_sema::analyze(&files);
    let mut module = newbf_sema::lower_program(&files, &program);

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
