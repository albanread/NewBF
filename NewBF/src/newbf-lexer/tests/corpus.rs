//! Corpus test: lex every file in the curated corlib slice and assert the
//! lossless invariant (token text concatenation == source) plus the
//! absence of `Unknown` tokens in clean Beef. This is the Sprint 02
//! acceptance gate (SPRINTS.md). The corpus lives at
//! `E:\NewBF\beef-tests\corlib-slice` (copied read-only from `E:\beef`).

use std::path::PathBuf;

use newbf_lexer::{FileId, TokenKind, lex};

fn corlib_slice() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../beef-tests/corlib-slice")
}

#[test]
fn lexes_corlib_slice_losslessly() {
    let dir = corlib_slice();
    let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));

    let mut files = 0usize;
    let mut total_tokens = 0usize;
    let mut unknowns: Vec<String> = Vec::new();

    for entry in entries {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("bf") {
            continue;
        }
        let src = std::fs::read_to_string(&path).unwrap();
        let toks = lex(&src, FileId(0));

        // (1) lossless round-trip: spans tile the whole file.
        let mut rebuilt = String::with_capacity(src.len());
        for t in &toks {
            if t.kind == TokenKind::Eof {
                continue;
            }
            rebuilt.push_str(&src[t.span.lo as usize..t.span.hi as usize]);
        }
        assert_eq!(rebuilt, src, "round-trip mismatch in {}", path.display());

        // (2) clean Beef should lex with no Unknown tokens.
        let name = path.file_name().unwrap().to_string_lossy();
        for t in &toks {
            if t.kind == TokenKind::Unknown {
                unknowns.push(format!(
                    "{name}: {:?}",
                    &src[t.span.lo as usize..t.span.hi as usize]
                ));
            }
        }

        files += 1;
        total_tokens += toks.len();
    }

    eprintln!("lexed {files} files, {total_tokens} tokens");
    assert!(
        files >= 80,
        "expected ~89 corlib-slice files, found {files}"
    );
    assert!(unknowns.is_empty(), "Unknown tokens in clean Beef:\n{}", {
        unknowns.sort();
        unknowns.dedup();
        unknowns.join("\n")
    });
}
