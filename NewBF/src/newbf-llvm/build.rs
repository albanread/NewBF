//! Build script for `newbf-llvm`.
//!
//! Its one job (MS-T3b): tell `aot.rs::link_executable` where the
//! `newbf-runtime` **staticlib** lands so an AOT executable can be linked
//! against the real memory guard (`newbf_alloc`/`newbf_free` + the bootstrap
//! lifecycle: `newbf_install_crash_handler` / `newbf_set_guard_mode` /
//! `newbf_register_alloc_sites`).
//!
//! `newbf-runtime` is `crate-type = ["rlib", "staticlib"]`, so cargo emits its
//! staticlib (`newbf_runtime.lib` on MSVC, `libnewbf_runtime.a` elsewhere) into
//! the **profile target dir** — e.g. `target/debug/` or `target/release/`. That
//! dir is the grand-parent-of-grand-parent of this build script's `OUT_DIR`
//! (`target/<profile>/build/newbf-llvm-<hash>/out`). The path is deterministic
//! regardless of crate build order — the `.lib` itself is produced because
//! `newbf-llvm` depends on `newbf-runtime`, so it exists by the time
//! `link_executable` runs at test / driver time.
//!
//! We export the dir as a compile-time env (`NEWBF_RUNTIME_LIB_DIR`) read by
//! `link_executable` via `env!`. Exporting the *dir* (not the full file path)
//! keeps the OS-specific staticlib name in one place (`aot.rs`).

use std::path::PathBuf;

fn main() {
    // OUT_DIR = <target>/<profile>/build/newbf-llvm-<hash>/out
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR set by cargo"));
    // ../../.. → <target>/<profile> (the profile dir where staticlibs land).
    let profile_dir = out_dir
        .parent() // newbf-llvm-<hash>
        .and_then(|p| p.parent()) // build
        .and_then(|p| p.parent()) // <profile>
        .expect("OUT_DIR has the expected target/<profile>/build/<crate>/out shape")
        .to_path_buf();

    println!(
        "cargo:rustc-env=NEWBF_RUNTIME_LIB_DIR={}",
        profile_dir.display()
    );
    // Re-run only if the script itself changes; the path is profile-stable.
    println!("cargo:rerun-if-changed=build.rs");
}
