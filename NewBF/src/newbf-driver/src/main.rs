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
    /// Compile a .bf file (JIT or AOT). Lands in phase 3 (Sprint 08).
    Compile {
        /// Path to a `.bf` source file.
        input: String,
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
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        None => {
            // Bare invocation prints the same banner as `--version`, so
            // the Sprint 01 demo works either way.
            println!("newbf-driver {VERSION_BANNER}");
        }
        Some(Command::Compile { input }) => {
            eprintln!("compile {input}: not yet implemented (SPRINTS.md phase 3)");
        }
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
    }
}
