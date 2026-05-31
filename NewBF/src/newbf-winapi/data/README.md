# Committed Win32 ABI snapshot

`winapi_data.bin.zst` is a zstd-compressed [postcard](https://docs.rs/postcard)
serialization of the Win32 function ABI index — function names, parameter/return
types, and calling conventions — that `newbf-winapi` embeds via `include_bytes!`.
Committing it makes the crate build on a clean clone with **no external
dependency** (no SQLite DB required).

- **Source:** projected from Microsoft's `Windows.Win32.winmd` metadata (the
  [`win32metadata`](https://github.com/microsoft/win32metadata) project,
  MIT-licensed) via a local SQLite mirror.
- **Contents:** API *signatures* (facts — not implementation): ~15,000 functions
  across ~346 DLLs.
- **Regenerate:** `build.rs` rewrites this file (only when its contents change)
  whenever a Win32 ABI SQLite DB is available — set
  `NEWBF_WINDOWS_API_DB=<path>` (or use the default path) and rebuild. With no DB
  present, the build uses this committed snapshot as-is.
