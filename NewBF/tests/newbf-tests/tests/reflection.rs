//! Reflection golden (RF-T7): pin the compiler's reflection metadata report.
//!
//! `format_reflection` renders `Module.type_meta` — every reflectable type, its
//! strip policy, and its (policy-gated) field/method rows — as a deterministic,
//! name-keyed text report. This snapshot test asserts that report matches a
//! checked-in golden file, so any change to *what the compiler reflects* (a
//! type's policy, its declared fields, its declared methods) surfaces as a
//! reviewable diff rather than silently drifting.
//!
//! The report is name-keyed and prints no `type_id` (which churns as corlib
//! grows), so it is stable across corlib churn and only moves when the reflected
//! surface of the program-under-test changes. To keep the golden focused on the
//! program (not all of corlib), the test filters the report to the fixture's own
//! types before comparing.

use newbf_ir::format_reflection;
use newbf_lexer::FileId;
use newbf_parser::parse_file;
use newbf_sema::{SourceFile, analyze, lower_program};

/// The fixture program: one class per policy variant, so the golden exercises
/// the full strip matrix — `[Reflect]` (TYPE|FIELDS|METHODS), `[Reflect(.Fields)]`,
/// `[Reflect(.Methods)]`, and an unmarked class (default TYPE only).
const FIXTURE: &str = r#"
    [Reflect] class Animal {
        public int32 mAge;
        public int32 Speak() { return 1; }
    }
    [Reflect(.Fields)] class Point {
        public int32 mX;
        public int32 mY;
    }
    [Reflect(.Methods)] class Widget {
        public int32 Area()  { return 1; }
        public int32 Width() { return 2; }
    }
    class Plain {
        public int32 mHidden;
        public int32 Hidden() { return 0; }
    }
    class Program { public static int32 Main() { return 0; } }
"#;

/// Lower the fixture and render its reflection report, filtered to the fixture's
/// own types (drop corlib rows so the golden is stable across corlib growth).
fn fixture_report() -> String {
    let (unit, pd) = parse_file(FIXTURE, FileId(0));
    assert!(pd.is_empty(), "parse diagnostics: {pd:?}");
    let files = [SourceFile {
        file: FileId(0),
        src: FIXTURE,
        unit: &unit,
    }];
    let program = analyze(&files);
    let mut module = lower_program(&files, &program);

    // Keep only the fixture's own types (a corlib type is anything not named
    // here), so the golden tracks the program's reflected surface, not corlib's.
    let own = ["Animal", "Point", "Widget", "Plain", "Program"];
    module.type_meta.retain(|t| own.contains(&t.name.as_str()));
    format_reflection(&module)
}

#[test]
fn reflection_report_matches_golden() {
    let got = fixture_report();
    let want = include_str!("golden/reflection_report.golden");
    // Normalize CRLF so the golden compares identically regardless of the
    // checkout's line endings (this repo is on Windows).
    let norm = |s: &str| s.replace("\r\n", "\n");
    assert_eq!(
        norm(&got),
        norm(want),
        "reflection report drifted from the golden.\n\
         If this change is intended, update tests/golden/reflection_report.golden \
         to the new report below:\n\n{got}"
    );
}
