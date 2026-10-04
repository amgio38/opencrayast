//! Hostile-input fuzz-style tests for the boundary, the workspace id and the renderers.
//!
//! Every case is a pure function of a printed seed, and each one runs under a wall-clock
//! ceiling, so a failure here is a reproduction rather than an anecdote.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]

// Only the fuzz helper: `common/mod.rs` also holds Unix-only socket helpers, and this suite
// has to build (and run its portable cases) on Windows too.
#[allow(
    dead_code,
    reason = "the helper serves several test binaries; each uses a subset"
)]
#[path = "common/fuzz.rs"]
mod fuzz;

use fuzz::{Case, Ran};
use opencrayast_core::ErrorCode;
use opencrayast_core::boundary::{Boundary, BoundaryConfig};
use opencrayast_core::limits::Limits;
use opencrayast_core::render::{ESCAPE_CATALOGUE, escape_inline, fenced_block};
use opencrayast_core::workspace::workspace_id;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

/// How many cases each target runs. The ticket's floor is 3000; the wall-clock budget is what
/// actually limits it, and each case is a syscall-light string operation.
const CASES: usize = 3000;

/// One seed for the whole file: a failure names the seed and the case index, and both together
/// rebuild the input.
const SEED: u64 = 0x00F0_0DE5_2026_1002;

// -- The executed-fraction floor, one constant per target -----------------------------
//
// Each target below declares its own floor next to itself, and every one of them is 1.0: each
// target's body asserts on whatever the call decided and returns `Ran::checked()` unconditionally,
// so there is nothing for it to decline. 1.0 is therefore not a target - it is the measurement.
//
// The floors used to be one shared `MIN_EXECUTED_FRACTION = 0.95` for the whole crate, which had
// no force against this tree: all six targets execute 3000/3000, so 0.95 admitted a 3.3% decline
// (2900/3000), a 10% decline and a 30% decline without complaint. The old `edit.overlap`
// generator really did produce 2900/3000 with the suite green. A floor that lets a third of a suite
// go unexecuted is exactly the "green but nothing was tested" failure this harness exists to stop.
//
// A target that must genuinely decline some cases declares a LOWER value here, in this file, where
// a reader of that target will see it - never by editing something shared, which would silently
// weaken every other target at the same time.

/// `core.resolve_read`: 3000/3000. `resolve_read` always answers, so there is no decline path.
const CORE_RESOLVE_READ_MIN_EXECUTED: f64 = 1.0;

/// `core.traversal`: 3000/3000. A traversal spelling is refused or resolved; both are asserted.
const CORE_TRAVERSAL_MIN_EXECUTED: f64 = 1.0;

/// `core.resolve_write`: 3000/3000.
const CORE_RESOLVE_WRITE_MIN_EXECUTED: f64 = 1.0;

/// `core.workspace_id`: 3000/3000. Every id is either refused or well formed; both are asserted.
const CORE_WORKSPACE_ID_MIN_EXECUTED: f64 = 1.0;

/// `core.render`: 3000/3000.
const CORE_RENDER_MIN_EXECUTED: f64 = 1.0;

/// `core.limits`: 3000/3000. Limit validation is total: every input is accepted or refused, and
/// the sweep asserts on whichever happened.
const CORE_LIMITS_MIN_EXECUTED: f64 = 1.0;

/// Seeds for the path mutator: the shapes a caller would actually type.
const PATHS: &[&str] = &[
    "",
    ".",
    "..",
    "../..",
    "a.rs",
    "src/a.rs",
    "src/lib.rs",
    "./a.rs",
    "a/b/c.rs",
    "/etc/passwd",
    "/tmp/x",
    "C:\\Windows\\System32",
    "\\\\?\\C:\\x",
    "\\\\server\\share",
    "CON",
    "nul.txt",
    ".git/config",
    ".env",
    "~",
    "\u{0}",
    "a\u{0}b",
    "\u{202e}a",
    "a\u{202e}",
    "\u{feff}a.rs",
    "e\u{301}.rs",
    "\u{e0001}a",
    "\u{ff0e}/etc",
    "a\u{3002}\u{3002}/b",
    "a/../../b",
    "a/./b",
];

/// What a resolution is allowed to answer. The boundary's own taxonomy: never a panic, never a
/// success that points outside.
fn assert_sane_read(
    result: Result<opencrayast_core::boundary::ResolvedPath, opencrayast_core::ToolError>,
    workspace: &PathBuf,
) {
    match result {
        Ok(p) => {
            // `p.abs.starts_with(workspace) || p.abs.is_symlink()` used to stand here.
            // The second arm was an escape hatch: it waived the property for EVERY symlink,
            // including one that points outside the workspace — and "never a success that
            // points outside" is the whole claim of this function. A link is not an exception
            // to containment, it is the most common way to break it: the boundary refuses a
            // link whose target leaves the root, so no successful resolution here may be one.
            assert!(
                p.abs.starts_with(workspace),
                "a resolved path left the workspace: {:?} (rel {:?})",
                p.abs,
                p.rel
            );
            assert!(
                !p.abs.is_symlink(),
                "a resolved path is itself a link: {:?} (rel {:?}) - containment through a \
                 link is not containment",
                p.abs,
                p.rel
            );
            assert!(
                !p.rel.contains('\0') && !p.rel.contains('\\'),
                "a display path carries a separator it should not: {:?}",
                p.rel
            );
        }
        Err(e) => assert!(
            matches!(
                e.code,
                ErrorCode::OutsideWorkspace
                    | ErrorCode::NotFound
                    | ErrorCode::InvalidArgs
                    | ErrorCode::ProtectedPath
                    | ErrorCode::UnsupportedTarget
                    | ErrorCode::LimitExceeded
            ),
            "a refusal with an unexpected code: {e:?}"
        ),
    }
}

#[test]
fn resolve_read_never_panics_and_never_answers_from_outside() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(ws.path().join("src")).unwrap();
    std::fs::write(ws.path().join("src/a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(ws.path().join("plain.txt"), "x").unwrap();
    let workspace = ws.path().canonicalize().unwrap();
    let boundary = Arc::new(
        Boundary::new(BoundaryConfig {
            root: ws.path().to_path_buf(),
            limits: Limits::default(),
            read_roots: Vec::new(),
            state_dir: None,
            extra_protected: Vec::new(),
        })
        .unwrap(),
    );
    fuzz::run_cases(
        "core.resolve_read",
        SEED,
        PATHS,
        CASES,
        move |case: &Case| {
            assert_sane_read(boundary.resolve_read(&case.input), &workspace);
            // `resolve_read` always answers; the case is done when the answer is sane.
            Ran::checked()
        },
    )
    .assert_executed_fraction(CORE_RESOLVE_READ_MIN_EXECUTED);
}

/// A `..` that escapes above the starting point is refused *lexically*, before the filesystem
/// is consulted, whatever separator spells it.
///
/// The backslash half is the load-bearing one: on Unix `src\\..\\..\\x` is a single legal file
/// name, so nothing downstream would catch it, and the only thing standing between an agent and
/// a traversal a Windows client would execute is this check.
#[test]
fn a_traversal_spelling_is_refused_before_the_filesystem_is_consulted() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(ws.path().join("src")).unwrap();
    std::fs::write(ws.path().join("src/a.rs"), "fn a() {}\n").unwrap();
    // A file above the workspace root, which is what a successful escape would reach.
    std::fs::write(ws.path().parent().unwrap().join("outside.txt"), "secret").unwrap();
    let workspace = ws.path().canonicalize().unwrap();
    let boundary = Arc::new(
        Boundary::new(BoundaryConfig {
            root: ws.path().to_path_buf(),
            limits: Limits::default(),
            read_roots: Vec::new(),
            state_dir: None,
            extra_protected: Vec::new(),
        })
        .unwrap(),
    );

    const ESCAPES: &[&str] = &[
        "../outside.txt",
        "src/../../outside.txt",
        "..\\outside.txt",
        "src\\..\\..\\outside.txt",
        "a\\..\\..\\..\\..\\etc\\passwd",
        "./../outside.txt",
        "//../outside.txt",
    ];
    let mutated_boundary = Arc::clone(&boundary);
    fuzz::run_cases(
        "core.traversal",
        SEED,
        ESCAPES,
        CASES,
        move |case: &Case| {
            // Whatever the mutation produced, an answer has to be a path that is genuinely
            // under the root. It can never be a successful read above the root.
            for result in [
                mutated_boundary.resolve_read(&case.input),
                mutated_boundary.resolve_write(&case.input),
            ] {
                let Ok(p) = result else {
                    continue;
                };
                assert!(
                    p.abs.starts_with(&workspace),
                    "{:?} resolved outside the workspace: {:?}",
                    case.input,
                    p.abs
                );
            }
            // A refusal is still an answer: the loop `continue`s past it rather than abandoning
            // the case, so both spellings were resolved and checked.
            Ran::checked()
        },
    )
    .assert_executed_fraction(CORE_TRAVERSAL_MIN_EXECUTED);

    // And the seven spellings themselves are refused *as escapes*, which is the part the
    // mutation removes: without the lexical check these come back as `not_found`, because on
    // Unix `src\\..\\..\\outside.txt` is just an unusual file name that does not exist.
    //
    // HONEST GRADING OF WHAT THE TWO LOOPS ABOVE ACTUALLY PROVE, because I got this wrong when
    // I first reported it and the next reader would otherwise assume more than is there:
    //
    // * This loop — the hard-coded `ESCAPES` list — is the ONLY thing that catches the
    //   backslash-traversal mutation. It is a fixed list of seven spellings, so it says
    //   nothing about the 3,000 mutated cases in the loop above it.
    // * The 3,000-case loop does NOT catch that mutation. It is a "never answers from
    //   outside" property test: whatever the mutation produces, the answer must be inside
    //   the root or an honest refusal. Removing the escape check does not make a mutated
    //   spelling resolve OUTSIDE — it makes it resolve as an ordinary (absent) file name
    //   inside, which that loop is happy with. So the property test and the escape test are
    //   answering different questions, and only one of them has teeth here.
    // * Removing the `/`-separated `..` refusal leaves everything GREEN, and that is not
    //   because the check is untested: it is because `ESCAPES` and this loop are redundant
    //   for that case. `lexically_normalise` refuses `..` for `/` and for `\\` in the same
    //   match arm, so deleting the `/` branch is absorbed by the second loop and by the
    //   containment proof in `resolve_existing`. Redundant coverage, not missing coverage —
    //   but nothing here would notice the difference, which is worth knowing before anyone
    //   reads a green run as evidence about the `/` branch specifically.
    for escape in ESCAPES {
        let read = boundary.resolve_read(escape);
        assert_eq!(
            read.as_ref().err().map(|e| e.code),
            Some(ErrorCode::OutsideWorkspace),
            "resolve_read({escape:?}) should be refused as an escape, got {:?}",
            read.map(|p| p.abs)
        );
        let write = boundary.resolve_write(escape);
        assert_eq!(
            write.as_ref().err().map(|e| e.code),
            Some(ErrorCode::OutsideWorkspace),
            "resolve_write({escape:?}) should be refused as an escape, got {:?}",
            write.map(|p| p.abs)
        );
    }
}

#[test]
fn resolve_write_never_panics_and_never_answers_from_outside() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("a.rs"), "fn a() {}\n").unwrap();
    let workspace = ws.path().canonicalize().unwrap();
    let boundary = Arc::new(
        Boundary::new(BoundaryConfig {
            root: ws.path().to_path_buf(),
            limits: Limits::default(),
            read_roots: Vec::new(),
            state_dir: None,
            extra_protected: Vec::new(),
        })
        .unwrap(),
    );
    fuzz::run_cases(
        "core.resolve_write",
        SEED,
        PATHS,
        CASES,
        move |case: &Case| {
            match boundary.resolve_write(&case.input) {
                Ok(p) => assert!(
                    p.abs.starts_with(&workspace),
                    "a writable path left the workspace: {:?}",
                    p.abs
                ),
                Err(e) => assert!(
                    matches!(
                        e.code,
                        ErrorCode::OutsideWorkspace
                            | ErrorCode::NotFound
                            | ErrorCode::InvalidArgs
                            | ErrorCode::ProtectedPath
                            | ErrorCode::UnsupportedTarget
                            | ErrorCode::LimitExceeded
                    ),
                    "a refusal with an unexpected code: {e:?}"
                ),
            }
            // Either way the answer was checked: the two arms assert rather than decline.
            Ran::checked()
        },
    )
    .assert_executed_fraction(CORE_RESOLVE_WRITE_MIN_EXECUTED);
}

/// The workspace id is derived from a path the operator controls, so it gets the same treatment.
/// A refusal is a `ToolError`; a success is exactly `w-` plus 32 hex digits, whatever came in.
#[test]
fn workspace_id_is_either_refused_or_well_formed() {
    let ws = tempfile::tempdir().unwrap();
    fuzz::run_cases(
        "core.workspace_id",
        SEED,
        PATHS,
        CASES,
        move |case: &Case| {
            match workspace_id(&PathBuf::from(&case.input)) {
                Ok(id) => {
                    assert!(id.starts_with("w-"), "{id}");
                    assert_eq!(id.len(), 34, "{id}");
                    assert!(
                        id[2..].chars().all(|c| c.is_ascii_hexdigit()),
                        "{id} is not hex"
                    );
                }
                Err(e) => assert!(
                    matches!(
                        e.code,
                        ErrorCode::NotFound | ErrorCode::IoError | ErrorCode::InvalidArgs
                    ),
                    "unexpected refusal: {e:?}"
                ),
            }
            let _ = &ws;
            // Every case ends in one of two asserted outcomes; neither declines.
            Ran::checked()
        },
    )
    .assert_executed_fraction(CORE_WORKSPACE_ID_MIN_EXECUTED);
}

/// Rendered output is what an agent (and a terminal) sees, so a hostile name must never reach it
/// raw. Escape classes are checked, not the exact spelling.
#[test]
fn renderers_never_emit_an_unescaped_hostile_character() {
    // The hazard list is the render module's own catalogue rather than a hand-picked sample. The
    // sample this replaces asserted on 14 characters, six of which the generator never emitted in
    // 3000 cases - so their assertions could not fire, and adding a leaked code point to the
    // escaper left this suite green. Enumerating the catalogue means a character only enters this
    // test if the escaper claims to handle it, and the reachability count below makes a character
    // the generator never produces loud rather than vacuous.
    let hostile: std::sync::Arc<Vec<char>> = std::sync::Arc::new(
        ESCAPE_CATALOGUE
            .iter()
            .flat_map(|range| (range.first..=range.last).filter_map(char::from_u32))
            .collect(),
    );
    assert!(
        !hostile.is_empty(),
        "the escape catalogue is empty, so this target would assert nothing"
    );

    // Counted per distinct code point, not per range entry, so a 128-entry Tags block cannot drown
    // out the report for the two characters outside it.
    let seen: std::sync::Arc<std::sync::Mutex<Vec<(u32, usize)>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let report = fuzz::run_cases("core.render", SEED, PATHS, CASES, {
        let hostile = std::sync::Arc::clone(&hostile);
        let seen = std::sync::Arc::clone(&seen);
        move |case: &Case| {
            let (inline, _) = escape_inline(&case.input);
            for c in hostile.iter() {
                assert!(
                    !inline.contains(*c),
                    "escape_inline left U+{:04X} raw in {inline:?}",
                    *c as u32
                );
            }
            // A newline must not smuggle a second line of output past a one-line field.
            assert!(!inline.contains('\n'), "{inline:?}");
            assert!(!inline.contains('\t'), "{inline:?}");

            let (block, _) = fenced_block(&case.input, "rust");
            // Inside a fence only the line breaks are kept; every other control class is escaped.
            for c in hostile.iter() {
                if *c == '\n' || *c == '\t' {
                    continue;
                }
                assert!(
                    !block.contains(*c),
                    "fenced_block left U+{:04X} raw in {block:?}",
                    *c as u32
                );
            }
            // Reachability: record which catalogue code points this run actually produced, so the
            // assertions above are known to have been about real input rather than about the input
            // class as a theory.
            {
                let mut tally = seen.lock().expect("reachability tally");
                for c in hostile.iter() {
                    if case.input.contains(*c) {
                        match tally.iter_mut().find(|(cp, _)| cp == &(*c as u32)) {
                            Some(slot) => slot.1 += 1,
                            None => tally.push((*c as u32, 1)),
                        }
                    }
                }
            }
            // Both renderers are pure functions of the input, so every case runs to the end, and both
            // arms above assert, so the case reached the property it exists to test.
            Ran::checked()
        }
    });

    let reached: BTreeMap<u32, usize> = std::sync::Arc::try_unwrap(seen)
        .expect("the tally is still shared")
        .into_inner()
        .expect("reachability tally")
        .into_iter()
        .collect();
    eprintln!(
        "core.render reachability: {}/{} catalogue code points appeared in {CASES} cases",
        reached.len(),
        hostile.len()
    );
    for (cp, n) in &reached {
        eprintln!("  U+{cp:04X} reached {n} case(s)");
    }
    // An unreachable code point is not a failure on its own - a 128-entry Tags block cannot all
    // appear in 3000 short cases, and neither can every code point of any range. But it must be
    // visible, and the sweep must not claim to have covered a class it never produced. Runs of
    // consecutive misses are collapsed to a range with its width, because listing all 451
    // individually buries the two or three that a reader could act on.
    {
        let mut sorted: Vec<u32> = hostile
            .iter()
            .map(|c| *c as u32)
            .filter(|cp| !reached.contains_key(cp))
            .collect();
        sorted.sort_unstable();
        let mut runs: Vec<(u32, u32)> = Vec::new();
        for cp in sorted {
            match runs.last_mut() {
                Some(last) if last.1 + 1 == cp => last.1 = cp,
                _ => runs.push((cp, cp)),
            }
        }
        let total: u32 = runs.iter().map(|(a, b)| b - a + 1).sum();
        let detail: Vec<String> = runs
            .iter()
            .map(|(a, b)| {
                if a == b {
                    format!("U+{a:04X}")
                } else {
                    format!("U+{a:04X}-U+{b:04X} ({})", b - a + 1)
                }
            })
            .collect();
        eprintln!(
            "core.render: {total} catalogue code point(s) never generated, in {} run(s): {}",
            runs.len(),
            detail.join(", ")
        );
    }
    assert!(
        !reached.is_empty(),
        "no catalogue code point was generated, so the escape assertions never had a subject"
    );
    // Both gates at this target's own declared floor of 1.0: every case must have run, and every
    // case must have asserted the escape property. The reachability numbers above are a report,
    // deliberately not a failure - an unreachable code point is expected - but the sweep claiming
    // to have covered a class it never produced is exactly what these two gates forbid.
    report
        .assert_executed_fraction(CORE_RENDER_MIN_EXECUTED)
        .assert_property_fraction(CORE_RENDER_MIN_EXECUTED);
}

/// The escaper and the catalogue must agree, in both directions.
///
/// This is the check that makes a truncated range a test failure rather than a silently narrowed
/// guarantee: `classify` is what runs, `ESCAPE_CATALOGUE` is what the documentation promises, and
/// the isolate controls `U+206A`-`U+206F` leaked through both renderers precisely because the two
/// had been written to the same too-narrow bound.
#[test]
fn the_escape_catalogue_matches_what_the_escaper_actually_escapes() {
    assert!(
        opencrayast_core::render::catalogue_is_exhaustive(),
        "the documented escape catalogue and the escaper's classification disagree; one of them is \
         wrong and the guarantee is only as good as the smaller of the two"
    );
}

/// Limits are the other thing an operator types by hand, so their parser gets the same pass.
#[test]
fn limits_validation_is_total() {
    let base = Arc::new(Limits::default());
    let base_for_case = Arc::clone(&base);
    fuzz::run_cases("core.limits", SEED, PATHS, CASES, move |case: &Case| {
        // Drive the validator with numbers found in the case, so the interesting inputs are
        // `0`, one past a hard maximum, and things that are not numbers at all.
        let mut limits = Limits::default();
        let _ = &base_for_case;
        let pick: Vec<u64> = case
            .bytes
            .chunks(8)
            .filter(|c| c.len() == 8)
            .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
            .take(3)
            .collect();
        for (i, value) in pick.iter().enumerate().take(3) {
            match i {
                0 => limits.max_file_bytes = *value,
                1 => limits.path_max_bytes = *value,
                _ => limits.max_results = *value,
            }
        }
        match limits.validate() {
            Ok(()) => {
                // If it validates, every field is in range: that is the whole contract.
                for (name, value, hard) in limits.table() {
                    assert!(value > 0, "{name} is zero");
                    assert!(
                        value <= hard,
                        "{name} is {value}, over its hard maximum {hard}"
                    );
                }
            }
            Err(e) => assert_eq!(e.code, ErrorCode::InvalidArgs, "{e:?}"),
        }
        // `Limits::validate` is total and both outcomes are asserted, so nothing is declined.
        Ran::checked()
    })
    .assert_executed_fraction(CORE_LIMITS_MIN_EXECUTED);
}
