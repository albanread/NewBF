//! `newbf-ide` — the NewBF IDE (Rust iGui, two-thread).
//!
//! A Rust application built on the portfolio's shared iGui front-end
//! (Direct2D / DirectWrite MDI), with the two-thread architecture proven
//! in NewFactor (`E:\NewFactor\src\bin\newfactor_ui.rs`):
//!
//! ```text
//! newbf-ide.exe  (one Windows process)
//! ├── GUI thread        Direct2D MDI, Win32 message pump (igui)
//! │     ↕ IGuiEvent MPSC channel
//! ├── IDE worker        receives events, drives the Session
//! │     ↕ Command / EvalResult channels
//! └── language worker   owns the NewBF compiler + JIT + manual-memory runtime
//! ```
//!
//! A supervisor wraps the worker in `catch_unwind` + an SEH crash handler
//! (three-level recovery), and a GUI-thread interrupt hook aborts a
//! long-running eval. Panes: the DocCrate doc browser, the `selkie`
//! editor, the console, the inspector, the leak/allocation view, and the
//! live phase-report viewer.
//!
//! **Compiler-first.** The iGui wiring activates only once the compiler
//! can JIT and run non-trivial Beef (SPRINTS.md, the IDE sprint). Until
//! then this binary is a stub.

fn main() {
    eprintln!(
        "newbf-ide: the iGui two-thread IDE is not yet wired \
         (compiler-first; see SPRINTS.md). Use `newbf-driver` for now."
    );
}
