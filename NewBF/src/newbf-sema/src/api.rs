//! FFI API-discovery pass — find native import declarations in the def graph.
//!
//! Beef's native FFI surfaces as `extern` methods (typically attributed
//! `[Import("…")]`). This pass walks the authoritative def graph, collects
//! every `extern` method as an import candidate, and (via an injected
//! resolver) resolves each against the Win32 ABI oracle (`newbf-winapi`),
//! recording the demanded `{dll, symbol}` set + arity cross-check.
//!
//! The demanded set is what later phases consume: IR lowering materializes
//! **one import thunk per demanded API** (JIT binds via `GetProcAddress`, AOT
//! via the IAT), and the AOT linker pulls each demanded DLL's import lib.
//!
//! The resolver is **injected** (the driver wires `newbf_winapi`) so
//! `newbf-sema` needn't depend on the FFI-metadata crate — the same
//! decoupling the comptime callback uses. Resolution treats the oracle as the
//! source of truth for the DLL; an `[Import("dll")]` attribute, once its
//! string arg is captured, will cross-check rather than drive it.

use newbf_lexer::Span;

use newbf_parser::Modifier;

use crate::Program;
use crate::model::MemberDef;

/// A discovered FFI import candidate: an `extern` method.
#[derive(Clone, Debug)]
pub struct ApiImport {
    /// Method name — the symbol resolved against the oracle.
    pub symbol: String,
    /// Owning type's name (for diagnostics / the report).
    pub owner: String,
    /// Declared parameter count (for the arity cross-check).
    pub param_count: usize,
    pub span: Span,
}

/// Collect every `extern` method in the program — the FFI import candidates.
/// Pure: no oracle, so `newbf-sema` stays backend-agnostic.
pub fn discover_extern_methods(program: &Program) -> Vec<ApiImport> {
    let g = &program.graph;
    let it = &program.interner;
    let mut out = Vec::new();
    for m in &g.members {
        if let MemberDef::Method(md) = m
            && md.modifiers.contains(&Modifier::Extern)
        {
            out.push(ApiImport {
                symbol: it.resolve(md.name).to_string(),
                owner: it.resolve(g.ty(md.owner).name).to_string(),
                param_count: md.params.len(),
                span: md.span,
            });
        }
    }
    out
}

/// One import resolved against the injected oracle.
#[derive(Clone, Debug)]
pub struct ResolvedApi {
    pub import: ApiImport,
    /// DLL the oracle says exports the symbol (`None` if unresolved).
    pub dll: Option<String>,
    /// The oracle's parameter count (`None` if unresolved).
    pub oracle_param_count: Option<usize>,
}

impl ResolvedApi {
    pub fn is_resolved(&self) -> bool {
        self.dll.is_some()
    }

    /// `Some(true/false)` when resolved — does the declared arity match the
    /// oracle's? `None` when unresolved.
    pub fn arity_matches(&self) -> Option<bool> {
        self.oracle_param_count
            .map(|n| n == self.import.param_count)
    }
}

/// Resolve discovered imports against an injected oracle. `resolve(name)`
/// yields `(dll, param_count)` for a known Win32 export; the driver wires
/// `newbf_winapi::find_function_any_dll`. Keeping the resolver a parameter is
/// what lets `newbf-sema` avoid a dependency on the FFI-metadata crate.
pub fn resolve_apis(
    imports: Vec<ApiImport>,
    resolve: impl Fn(&str) -> Option<(String, usize)>,
) -> Vec<ResolvedApi> {
    imports
        .into_iter()
        .map(|import| {
            let (dll, oracle_param_count) = match resolve(&import.symbol) {
                Some((dll, n)) => (Some(dll), Some(n)),
                None => (None, None),
            };
            ResolvedApi {
                import,
                dll,
                oracle_param_count,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SourceFile, analyze};
    use newbf_lexer::FileId;
    use newbf_parser::parse_file;

    fn analyze_src(src: &str) -> Program {
        let (unit, pdiags) = parse_file(src, FileId(0));
        assert!(pdiags.is_empty(), "parse diagnostics: {pdiags:?}");
        analyze(&[SourceFile {
            file: FileId(0),
            src,
            unit: &unit,
        }])
    }

    #[test]
    fn discovers_extern_methods_only() {
        let src = "class Win32 { \
                   public static extern int32 Beep(uint32 freq, uint32 dur); \
                   public int32 NotExtern() => 0; }";
        let imports = discover_extern_methods(&analyze_src(src));
        assert_eq!(imports.len(), 1, "{imports:?}");
        assert_eq!(imports[0].symbol, "Beep");
        assert_eq!(imports[0].owner, "Win32");
        assert_eq!(imports[0].param_count, 2);
    }

    #[test]
    fn resolves_against_injected_oracle() {
        let src = "class Win32 { public static extern int32 Beep(uint32 freq, uint32 dur); }";
        let imports = discover_extern_methods(&analyze_src(src));
        // Stub oracle: Beep lives in kernel32.dll, 2 params.
        let resolved = resolve_apis(imports, |name| {
            (name == "Beep").then(|| ("kernel32.dll".to_string(), 2))
        });
        assert_eq!(resolved.len(), 1);
        assert!(resolved[0].is_resolved());
        assert_eq!(resolved[0].dll.as_deref(), Some("kernel32.dll"));
        assert_eq!(resolved[0].arity_matches(), Some(true));
    }

    #[test]
    fn arity_mismatch_and_unresolved_are_flagged() {
        let src = "class X { \
                   public static extern void Mystery(int32 a); \
                   public static extern void Beep(uint32 a); }";
        let imports = discover_extern_methods(&analyze_src(src));
        // Resolve only Beep, and report a different arity to trip the check.
        let resolved = resolve_apis(imports, |name| {
            (name == "Beep").then(|| ("kernel32.dll".to_string(), 2))
        });
        let beep = resolved.iter().find(|r| r.import.symbol == "Beep").unwrap();
        assert_eq!(beep.arity_matches(), Some(false)); // declared 1, oracle 2
        let myst = resolved
            .iter()
            .find(|r| r.import.symbol == "Mystery")
            .unwrap();
        assert!(!myst.is_resolved());
        assert_eq!(myst.arity_matches(), None);
    }
}
