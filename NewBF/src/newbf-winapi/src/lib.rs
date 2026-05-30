//! `newbf-winapi` — vendored Win32 **ABI metadata** for the FFI surface.
//!
//! ## What this is
//! The Win32 API *surface* — every function's exact signature, DLL, calling
//! convention, A/W family, and `SetLastError` semantics — projected from
//! Microsoft's official `Windows.Win32.winmd` (ingested into the shared
//! `E:\windows_api\windows_api.db`). `build.rs` reads that SQLite DB and
//! embeds a zstd-compressed postcard blob; this module decompresses + indexes
//! it once on first access ([`LazyLock`]) and answers lookups from in-memory
//! `HashMap`s.
//!
//! The model is **language-agnostic** (mirrors NewOpenDylan's `nod-winapi`):
//! it records ABI *facts*; the Beef-type crosswalk (`DWORD→uint32`,
//! `LPCWSTR→char16*`, …) happens at consumption time in sema, not here.
//!
//! ## How it's consumed (the plan)
//! A **sema discovery pass** walks the program, finds Win32 API calls,
//! resolves each against [`find_function`] (validating arity/types/charset),
//! and records the demanded `{dll, fn}` set + resolved signatures — an
//! authoritative sema output. IR lowering then materializes **one import
//! thunk per demanded API** (the JIT binds it via `GetProcAddress`; AOT via
//! the IAT) and rewrites call sites to near-call their thunk. The AOT linker
//! pulls each demanded DLL's import lib via [`import_lib_for_dll`].
//!
//! ## NOT here (yet)
//! No `LoadLibrary`/`GetProcAddress`, no actual calls — this is the ABI
//! oracle. Constants are deferred (the DB lacks enum member values).

use std::collections::HashMap;
use std::sync::LazyLock;

include!("data_schema.rs");
// `include!` brings `ConstantInfo`, `Direction`, `FunctionInfo`, `ParamInfo`,
// `TypeRef`, `WinApiIndex` into the crate root — no re-export needed.

/// The embedded zstd-compressed postcard blob (path filled by `build.rs`).
static WINAPI_BLOB: &[u8] = include_bytes!(env!("WINAPI_DATA_BIN"));

/// Aggregate counts — diagnostics + the blob-size test.
#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub function_count: usize,
    pub constant_count: usize,
    pub dll_count: usize,
    pub blob_bytes: usize,
}

struct ResolvedIndex {
    functions: Vec<FunctionInfo>,
    constants: Vec<ConstantInfo>,
    dll_names: Vec<String>,
    /// (lower-cased dll, name) → index into `functions`.
    by_dll_and_name: HashMap<(String, String), usize>,
    /// name → all matching indices (across DLLs).
    by_name: HashMap<String, Vec<usize>>,
    /// lower-cased dll → indices.
    by_dll: HashMap<String, Vec<usize>>,
    consts_by_name: HashMap<String, usize>,
}

static INDEX: LazyLock<ResolvedIndex> = LazyLock::new(|| {
    let decompressed =
        zstd::stream::decode_all(WINAPI_BLOB).expect("embedded winapi blob is valid zstd");
    let raw: WinApiIndex =
        postcard::from_bytes(&decompressed).expect("embedded winapi blob is valid postcard");
    let WinApiIndex {
        functions,
        constants,
        dll_names,
    } = raw;

    let mut by_dll_and_name = HashMap::with_capacity(functions.len());
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    let mut by_dll: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, f) in functions.iter().enumerate() {
        let dll_key = f.dll.to_ascii_lowercase();
        by_dll_and_name
            .entry((dll_key.clone(), f.name.clone()))
            .or_insert(i);
        by_name.entry(f.name.clone()).or_default().push(i);
        by_dll.entry(dll_key).or_default().push(i);
    }
    let mut consts_by_name = HashMap::with_capacity(constants.len());
    for (i, c) in constants.iter().enumerate() {
        consts_by_name.entry(c.name.clone()).or_insert(i);
    }

    ResolvedIndex {
        functions,
        constants,
        dll_names,
        by_dll_and_name,
        by_name,
        by_dll,
        consts_by_name,
    }
});

/// Look up a function by DLL + name (case-insensitive on the DLL).
pub fn find_function(dll: &str, name: &str) -> Option<&'static FunctionInfo> {
    let key = (dll.to_ascii_lowercase(), name.to_string());
    INDEX
        .by_dll_and_name
        .get(&key)
        .map(|&i| &INDEX.functions[i])
}

/// Look up by name across all DLLs (first match in DB order). Use
/// [`find_function`] once the `library:` clause has fixed the DLL.
pub fn find_function_any_dll(name: &str) -> Option<&'static FunctionInfo> {
    INDEX
        .by_name
        .get(name)
        .and_then(|v| v.first())
        .map(|&i| &INDEX.functions[i])
}

/// Look up a named integer constant (deferred — currently always `None`).
pub fn find_constant(name: &str) -> Option<&'static ConstantInfo> {
    INDEX.consts_by_name.get(name).map(|&i| &INDEX.constants[i])
}

/// Iterate functions for a DLL (case-insensitive).
pub fn iter_dll(dll: &str) -> impl Iterator<Item = &'static FunctionInfo> {
    let key = dll.to_ascii_lowercase();
    INDEX
        .by_dll
        .get(&key)
        .map(|v| v.as_slice())
        .unwrap_or(&[])
        .iter()
        .map(|&i| &INDEX.functions[i])
}

/// All distinct DLL names in the index.
pub fn dll_names() -> &'static [String] {
    &INDEX.dll_names
}

/// All functions, in DB order.
pub fn functions() -> &'static [FunctionInfo] {
    &INDEX.functions
}

/// Aggregate counts.
pub fn stats() -> Stats {
    Stats {
        function_count: INDEX.functions.len(),
        constant_count: INDEX.constants.len(),
        dll_count: INDEX.dll_names.len(),
        blob_bytes: WINAPI_BLOB.len(),
    }
}

/// Map a Windows DLL name to its MSVC import-library name, mechanically:
/// lowercase and swap the trailing `.dll` for `.lib`. `None` if `dll`
/// doesn't end in `.dll`. The AOT linker consumes this for each demanded DLL.
pub fn import_lib_for_dll(dll: &str) -> Option<String> {
    let lower = dll.to_ascii_lowercase();
    let stem = lower.strip_suffix(".dll")?;
    (!stem.is_empty()).then(|| format!("{stem}.lib"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_is_populated() {
        let s = stats();
        // The full Win32 surface is many thousands of functions across many
        // DLLs; assert a generous floor so a broken projection is caught.
        assert!(
            s.function_count > 5000,
            "only {} functions projected",
            s.function_count
        );
        assert!(s.dll_count > 10, "only {} dlls", s.dll_count);
        assert!(s.blob_bytes > 0);
    }

    #[test]
    fn finds_a_well_known_function() {
        // MessageBoxW lives in user32.dll; if the projection dropped it the
        // FFI surface is broken.
        let f = find_function("user32.dll", "MessageBoxW")
            .or_else(|| find_function_any_dll("MessageBoxW"))
            .expect("MessageBoxW present");
        assert_eq!(f.dll, "user32.dll");
        assert_eq!(f.aw_family, Some(b'W'));
        assert!(!f.params.is_empty());
    }

    #[test]
    fn import_lib_mapping() {
        assert_eq!(
            import_lib_for_dll("KERNEL32.DLL").as_deref(),
            Some("kernel32.lib")
        );
        assert_eq!(
            import_lib_for_dll("user32.dll").as_deref(),
            Some("user32.lib")
        );
        assert_eq!(import_lib_for_dll("not_a_dll"), None);
    }
}
