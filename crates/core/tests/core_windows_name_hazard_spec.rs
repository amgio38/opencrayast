//! A component Windows cannot name must answer `outside_workspace`, not `io_error` (BND-11).
//!
//! This is the fuzz case the Windows job found: `resolve_read(">>")` failed inside `open` with
//! `ERROR_INVALID_NAME`, which `std::io` reports as an I/O error, while a merely absent path
//! reports not-found. Same answer, two codes - and the difference tells an attacker which names
//! are illegal, which is the probe oracle the resolver exists to prevent (BND-18, T-20). The
//! fuzzer stops at its first failure, so a class found this way is a class whose neighbours
//! are still unreached; this file states the whole class rather than the one case.
//!
//! **The rule is consulted only on Windows** (`cfg!(windows)` at the call site), because on
//! Unix `con.go` and `aux.rs` are ordinary file names that real projects have. So what this
//! portable file asserts differs by platform, and says which is which:
//!
//! * on Windows: every hazard component resolves to `outside_workspace`;
//! * on Unix: every one of them resolves *normally* - to `not_found` for a name that is
//!   absent, which is the proof the gate is off and the file names have not become a
//!   cross-platform refusal.
//!
//! The pure rule itself is unit-tested against its full table in `core::boundary`, where it is
//! the *function* that is exercised rather than whichever platform is running.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_core::ErrorCode;
use opencrayast_core::boundary::{Boundary, BoundaryConfig};
use opencrayast_core::limits::Limits;

/// Every class from the ticket, spelled as full workspace-relative paths.
const HAZARDS: &[&str] = &[
    // Rule 1 - a character Windows rejects outright. `>>` is the case the fuzzer reported.
    ">>",
    "a<b",
    "a?b",
    "a*b",
    "\"x\"",
    "a|b",
    "dir/a>b.rs",
    // Rule 2 - a trailing dot or space, which Windows strips. `foo.` is the file `foo`.
    "foo.",
    "foo ",
    "foo..",
    ".hidden.",
    "dir/name. ",
    // Rule 3 - reserved device names, in the spellings Windows accepts: any case, with an
    // "extension", with a trailing dot or space, and the superscript digit forms.
    "CON",
    "con",
    "Con.txt",
    "nul.",
    "con ",
    "COM1",
    "com9.x",
    "LPT3",
    "lpt1 ",
    "COM\u{b9}",
    "COM\u{b2}.txt",
];

/// Names that must keep resolving on **both** platforms. If any of these ever answers
/// `outside_workspace`, a rule has grown past its class - `console` is not `CON`, `com10` is
/// not `COM1`, and an interior dot is not a trailing one.
const ORDINARY: &[&str] = &[
    "src/main.rs",
    "a.txt",
    "a.b",
    ".gitignore",
    "console.txt",
    "conf.rs",
    "com0.rs",
    "com10.rs",
    "communicate.md",
    "null.txt",
    "lpt.md",
];

#[test]
fn a_name_windows_cannot_name_answers_outside_workspace_and_never_io_error() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("a.rs"), "fn a() {}\n").unwrap();
    let boundary = Boundary::new(BoundaryConfig {
        root: ws.path().to_path_buf(),
        limits: Limits::default(),
        read_roots: Vec::new(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();

    for path in HAZARDS {
        for result in [boundary.resolve_read(path), boundary.resolve_write(path)] {
            let code = result.as_ref().err().map(|e| e.code);
            if cfg!(windows) {
                assert_eq!(
                    code,
                    Some(ErrorCode::OutsideWorkspace),
                    "{path:?} cannot be named on Windows, so it must be refused as outside \
                     before any filesystem call; an io_error is the probe oracle (got {result:?})"
                );
            } else {
                // The gate is off here, so the name is taken at face value and simply does not
                // exist. This is not a weaker assertion - it is the one that proves the rule
                // did not leak into the Unix path, which a real project depends on.
                assert_eq!(
                    code,
                    Some(ErrorCode::NotFound),
                    "{path:?} is an ordinary (if absent) Unix file name and must not be refused \
                     lexically on this platform (got {result:?})"
                );
            }
        }
    }
}

#[test]
fn ordinary_names_resolve_on_every_platform() {
    let ws = tempfile::tempdir().unwrap();
    let boundary = Boundary::new(BoundaryConfig {
        root: ws.path().to_path_buf(),
        limits: Limits::default(),
        read_roots: Vec::new(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();

    for path in ORDINARY {
        assert_eq!(
            boundary.resolve_read(path).as_ref().err().map(|e| e.code),
            Some(ErrorCode::NotFound),
            "{path:?} is an ordinary file name on every platform and must be judged on \
             existence, not refused by a name rule"
        );
    }

    // The control that makes the test above meaningful: a name that is present really does
    // resolve, so a green run is not just "everything 404s".
    std::fs::write(ws.path().join("a.txt"), "hello\n").unwrap();
    let resolved = boundary.resolve_read("a.txt").unwrap();
    assert!(
        resolved.abs.ends_with("a.txt"),
        "{}",
        resolved.abs.display()
    );
}
