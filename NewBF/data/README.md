# data/

Vendored build inputs.

- `windows_api.db` — a read-only snapshot of the Win32 API metadata from
  the shared `E:\windows_api` repository (SQLite). Added at the winapi
  sprint (SPRINTS.md Sprint 27). `newbf-winapi`'s `build.rs` reads it and
  emits a postcard+zstd blob into `OUT_DIR`, mirroring NewOpenDylan's
  `nod-winapi`. The `E:\windows_api` repository is consumed read-only; we
  re-snapshot the `.db` here so builds are reproducible and do not reach
  outside the workspace.

Generated artifacts (`*.bin`, `*.zst`) are git-ignored; the `.db` snapshot
is committed.
