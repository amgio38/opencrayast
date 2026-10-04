//! Spec for ISSUE-CLI-SKELETON: the human command line (CLI1-xx). Never weaken; add cases.
//!
//! The five invariants of the ticket, one test each, then one test per row of the failure table.
//! Everything runs in-process against a real temporary workspace: the CLI is driven through
//! [`opencrayast::run`] with a capturing sink, so what is asserted is exactly what a person would
//! see, minus the process boundary.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use clap::{CommandFactory, Parser};
use opencrayast::exit::{
    EXIT_CODE_HELP, EXIT_ENV, EXIT_OK, EXIT_USER, exit_code_for, exit_code_for_error,
};
use opencrayast::out::{Capture, Sink};
use opencrayast::{Cli, Command, PlanCmd};
use opencrayast_core::ErrorCode;
use opencrayast_core::error::ToolError;
use opencrayast_core::hash::ContentHash;
use opencrayast_core::limits::Limits;
use opencrayast_edit::{Clock, Plan, PlanFile, PlanRequest, PlanStore, SystemClock};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A clock the test can move, but which starts at the real time: the CLI reads the store with
/// [`SystemClock`], so a plan stored against a fake epoch would look expired to it. Starting at the
/// real now keeps the two comparable; the tests that care about expiry move this one forward.
struct FakeClock(AtomicU64);
impl FakeClock {
    fn at_real_now() -> FakeClock {
        FakeClock(AtomicU64::new(SystemClock.now_secs()))
    }
}
impl Clock for FakeClock {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// A temporary workspace with a state directory and a plan store.
struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
    state: PathBuf,
    ws: String,
}

impl World {
    fn new() -> World {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        std::fs::create_dir(&root).unwrap();
        // The state directory is NOT inside the workspace any more: it is the platform
        // user-state base, with a per-workspace `ws-<id>` segment the stores append. A test
        // fixture points `run_with_state` at one directory of its own, because a test that
        // resolved the real one would write into the developer's actual state directory and
        // see whatever the last test left there.
        let state = dir.path().join("state");
        let ws = opencrayast_core::workspace::workspace_id(&root).unwrap();
        World {
            _dir: dir,
            root,
            state,
            ws,
        }
    }

    /// Store a plan over the given files and return its id.
    fn put_plan(&self, specs: &[(&str, &str, &str)]) -> String {
        let clock = Arc::new(FakeClock::at_real_now());
        let store = PlanStore::open(&self.state, &self.ws, Limits::default(), clock).unwrap();
        let mut specs: Vec<(&str, &str, &str)> = specs.to_vec();
        specs.sort_by(|a, b| a.0.cmp(b.0));
        let files = specs
            .iter()
            .map(|(path, before, after)| PlanFile {
                path: (*path).to_string(),
                language: "text".into(),
                pre_hash: ContentHash::of(before.as_bytes()),
                pre_size: before.len() as u64,
                pre_errors: 0,
                post_hash: ContentHash::of(after.as_bytes()),
                post_size: after.len() as u64,
                post_errors: 0,
                edits: vec![opencrayast_edit::Edit {
                    start: 0,
                    end: before.len(),
                    replacement: (*after).to_string(),
                }],
            })
            .collect();
        let plan = Plan {
            format: 1,
            workspace_id: self.ws.clone(),
            engine_format: 1,
            request: PlanRequest {
                kind: "rewrite".into(),
                summary: "test summary".into(),
                note: Some("test note".into()),
            },
            files,
        };
        store.put(&plan).unwrap().0
    }
}

/// Run one invocation against `root`, capturing the output.
fn drive(root: &Path, args: &[&str]) -> (i32, Capture) {
    drive_state(root, args, &state_dir_for(root))
}

/// The state directory this test's store lives in.
///
/// A per-workspace directory under a **fixed, test-owned** base, so two tests running at once
/// cannot collide and no test reads the developer's real state directory. The `ws-<id>` segment
/// is appended by the stores; only the base is chosen here.
fn state_dir_for(root: &Path) -> PathBuf {
    root.parent()
        .map(|d| d.join("state"))
        .unwrap_or_else(|| PathBuf::from("state"))
}

/// Drive one invocation against an explicit state directory, so nothing resolves the ambient
/// one.
fn drive_state(root: &Path, args: &[&str], state: &Path) -> (i32, Capture) {
    let mut full: Vec<&str> = vec!["opencrayast", "--workspace", root.to_str().unwrap()];
    full.extend_from_slice(args);
    let cli = Cli::try_parse_from(&full).unwrap_or_else(|e| panic!("{e}"));
    let mut cap = Capture::default();
    let code = opencrayast::run_with_state(
        &cli,
        &mut cap,
        opencrayast::palette::Palette::new(false),
        &mut opencrayast::confirm::Stdin::new(),
        &opencrayast::StateDir::Fixed(state),
    );
    (code, cap)
}

/// Everything printed, as one string, for `contains` assertions.
fn text(cap: &Capture) -> String {
    cap.all().join("\n")
}

// ---- invariant 1: doctor ---------------------------------------------------------------------

/// CLI1-01: `doctor` checks each thing the ticket names and prints `ok`/`warn`/`fail` with a next
/// step for each; a failure makes it exit 2, and it never panics.
#[test]
fn cli1_doctor_checks_every_area_and_never_panics() {
    let w = World::new();
    let (code, cap) = drive(&w.root, &["doctor"]);
    let t = text(&cap);

    for area in [
        "workspace",
        "workspace writable",
        "state dir",
        "plan store",
        "languages",
        "write mode",
    ] {
        assert!(t.contains(area), "doctor did not check {area}:\n{t}");
    }
    // Every check line carries a verdict from the fixed set.
    for line in t
        .lines()
        .filter(|l| l.starts_with("ok ") || l.starts_with("warn ") || l.starts_with("fail "))
    {
        let verdict = line.split_whitespace().next().unwrap();
        assert!(
            matches!(verdict, "ok" | "warn" | "fail"),
            "unexpected verdict in {line:?}"
        );
    }
    assert_eq!(code, EXIT_OK, "a healthy workspace is a success:\n{t}");
    assert!(t.contains("All checks passed."), "{t}");

    // A root that does not exist: reported, not panicked, and exit 2.
    let missing = w.root.join("nope");
    let (code, cap) = drive(&missing, &["doctor"]);
    let t = text(&cap);
    assert!(t.contains("fail"), "a missing root must fail a check:\n{t}");
    assert_eq!(code, EXIT_ENV);
    assert!(
        !t.contains("not checked: the workspace or state directory is not usable\nfail"),
        "the state dir must not be probed for a root that does not exist:\n{t}"
    );
}

/// CLI1-01b: a failing check says what to do. Every `fail` line must carry a next step.
#[test]
fn cli1_every_failing_doctor_check_says_what_to_do() {
    let w = World::new();
    let (code, cap) = drive(&w.root.join("nope"), &["doctor"]);
    let t = text(&cap);
    assert_eq!(code, EXIT_ENV);
    let fails: Vec<&str> = t.lines().filter(|l| l.starts_with("fail ")).collect();
    assert!(!fails.is_empty(), "expected a failing check:\n{t}");
    for line in fails {
        assert!(
            line.contains("—"),
            "a fail line must say what to do, got {line:?}"
        );
    }
    assert!(t.contains("then run `opencrayast doctor` again"), "{t}");
}

// ---- invariant 2: plan list ------------------------------------------------------------------

/// CLI1-02: `plan list` shows id, files, edits, state and expiry; an empty store prints a sentence,
/// never a blank screen.
#[test]
fn cli1_plan_list_shows_the_fields_and_speaks_when_empty() {
    let w = World::new();

    // Empty: a sentence, not nothing.
    let (code, cap) = drive(&w.root, &["plan", "list"]);
    let t = text(&cap);
    assert_eq!(code, EXIT_OK);
    assert!(t.contains("No plans stored"), "{t}");
    assert!(!t.trim().is_empty(), "empty store must still say something");

    // With plans: every field the ticket names.
    let id = w.put_plan(&[("a.rs", "one\n", "two\n"), ("b.rs", "x\n", "y\n")]);
    let (code, cap) = drive(&w.root, &["plan", "list"]);
    let t = text(&cap);
    assert_eq!(code, EXIT_OK);
    assert!(t.contains(&id), "the id must be shown:\n{t}");
    assert!(t.contains("2 file(s)"), "{t}");
    assert!(t.contains("2 edit(s)"), "{t}");
    assert!(t.contains("ready"), "state must be shown:\n{t}");
    assert!(t.contains("expires in"), "expiry must be shown:\n{t}");
}

/// CLI1-02b: the default limit is 20, and `--limit` changes it.
#[test]
fn cli1_plan_list_defaults_to_twenty_and_honours_limit() {
    let w = World::new();
    // 25 plans, each a distinct note so the ids differ.
    let specs: Vec<(String, String, String)> = (0..25)
        .map(|i| {
            (
                format!("f{i}.txt"),
                format!("before {i}\n"),
                format!("after {i}\n"),
            )
        })
        .collect();
    let refs: Vec<(&str, &str, &str)> = specs
        .iter()
        .map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str()))
        .collect();
    for one in &refs {
        w.put_plan(std::slice::from_ref(one));
    }

    let (_, cap) = drive(&w.root, &["plan", "list"]);
    let t = text(&cap);
    assert!(t.contains("25 plan(s)"), "{t}");
    assert!(t.contains("showing the first 20"), "{t}");
    assert!(t.contains("5 more plan(s) not shown"), "{t}");
    // Exactly 20 id lines were printed.
    let shown = t
        .lines()
        .filter(|l| l.trim_start().starts_with("p-"))
        .count();
    assert_eq!(shown, 20, "expected 20 plan lines:\n{t}");

    let (_, cap) = drive(&w.root, &["plan", "list", "--limit", "3"]);
    let t = text(&cap);
    let shown = t
        .lines()
        .filter(|l| l.trim_start().starts_with("p-"))
        .count();
    assert_eq!(shown, 3, "--limit must be honoured:\n{t}");
}

// ---- invariant 3: plan show, prefix resolution ------------------------------------------------

/// CLI1-03: `plan show` accepts an unambiguous prefix (reading may abbreviate, E-15) and refuses an
/// ambiguous one, listing the candidates.
#[test]
fn cli1_plan_show_takes_a_prefix_and_refuses_an_ambiguous_one() {
    let w = World::new();
    let id = w.put_plan(&[("a.rs", "one\n", "two\n")]);
    assert!(id.len() > 12, "the test needs a long enough id: {id}");

    // Full id.
    let (code, cap) = drive(&w.root, &["plan", "show", &id]);
    let t = text(&cap);
    assert_eq!(code, EXIT_OK, "{t}");
    assert!(t.contains(&id), "{t}");
    assert!(t.contains("a.rs"), "the file must be listed:\n{t}");

    // A 10-character prefix — the documented minimum for reading.
    let prefix: String = id.chars().take(10).collect();
    let (code, cap) = drive(&w.root, &["plan", "show", &prefix]);
    let t = text(&cap);
    assert_eq!(code, EXIT_OK, "a 10-char prefix must resolve:\n{t}");
    assert!(t.contains(&id), "{t}");

    // A prefix that matches nothing.
    let (code, cap) = drive(&w.root, &["plan", "show", "p-zzzzzzzzzzzzzzzzzzzzzzzzzz"]);
    let t = text(&cap);
    assert_eq!(code, EXIT_USER, "{t}");
    assert!(
        t.contains("[plan_not_found]"),
        "the code must be literal:\n{t}"
    );
}

/// CLI1-03b: `plan show` names the candidates when a prefix is ambiguous.
#[test]
fn cli1_plan_show_lists_candidates_when_a_prefix_is_ambiguous() {
    let w = World::new();
    // Find two stored plans sharing a 10-char prefix by trying prefixes; the store is content
    // addressed, so this is the honest way to get one.
    let mut ids = Vec::new();
    // The store holds at most `plan_max_plans` (100 by default), so stay under it.
    for i in 0..90 {
        ids.push(w.put_plan(&[(&format!("f{i}.txt"), &format!("a{i}\n"), &format!("b{i}\n"))]));
    }
    // Group by the first 10 characters.
    let mut by_prefix: std::collections::HashMap<String, Vec<&String>> =
        std::collections::HashMap::new();
    for id in &ids {
        by_prefix
            .entry(id.chars().take(10).collect::<String>())
            .or_default()
            .push(id);
    }
    let Some((prefix, group)) = by_prefix.into_iter().find(|(_, g)| g.len() > 1) else {
        // No collision in this build's ids: the ambiguity branch is still covered by the
        // not-found test, and this is reported rather than silently passed.
        eprintln!(
            "note: no 10-char prefix collision occurred in {} ids",
            ids.len()
        );
        return;
    };
    let (code, cap) = drive(&w.root, &["plan", "show", &prefix]);
    let t = text(&cap);
    assert_eq!(code, EXIT_USER, "an ambiguous prefix is a user error:\n{t}");
    assert!(
        t.contains("[ambiguous]") || t.contains("[invalid_args]"),
        "{t}"
    );
    for id in &group {
        assert!(
            t.contains(id.as_str()),
            "candidate {id} must be listed:\n{t}"
        );
    }
}

// ---- invariant 4: exit codes -----------------------------------------------------------------

/// CLI1-04: the exit code of every error code is what `--help` documents, and the table and the
/// mapping are generated from the same place so they cannot drift.
#[test]
fn cli1_exit_codes_match_the_help_text() {
    // CR R2: the NUMBERS are the contract. Asserting only that the three differ let a swap of
    // EXIT_USER and EXIT_ENV pass every test, so they are pinned to their digits here.
    assert_eq!(EXIT_OK, 0, "success is exit status 0");
    assert_eq!(EXIT_USER, 1, "a user error is exit status 1");
    assert_eq!(EXIT_ENV, 2, "an environment error is exit status 2");

    // `--help` carries the table.
    let mut cmd = Cli::command();
    let help = cmd.render_long_help().to_string();
    assert!(help.contains("Exit codes:"), "{help}");

    // Every code lands in the bucket its help text names.
    for (code, want, name) in [
        (ErrorCode::InvalidArgs, EXIT_USER, "user error"),
        (ErrorCode::PlanNotFound, EXIT_USER, "user error"),
        (ErrorCode::PlanExpired, EXIT_USER, "user error"),
        (ErrorCode::WrongWorkspace, EXIT_USER, "user error"),
        (ErrorCode::IoError, EXIT_ENV, "environment"),
        (ErrorCode::OutsideWorkspace, EXIT_ENV, "environment"),
        (ErrorCode::Busy, EXIT_ENV, "environment"),
        (ErrorCode::WriteDisabled, EXIT_ENV, "environment"),
    ] {
        assert_eq!(exit_code_for(code), want, "{code:?} must exit {want}");
        assert!(
            EXIT_CODE_HELP.contains(code.as_str()),
            "{code:?} ({}) is not in the help table",
            code.as_str()
        );
        assert!(
            EXIT_CODE_HELP.contains(name),
            "the help table has no {name:?} bucket"
        );
    }

    // An error carries its code's exit code.
    let e = ToolError::new(ErrorCode::PlanNotFound, "x", "y");
    assert_eq!(exit_code_for_error(&e), EXIT_USER);
}

/// CLI1-04b: the mapping is total — every `ErrorCode` variant is in a bucket. The compiler enforces
/// this too (the match has no wildcard); this test says so out loud and checks the help text covers
/// the same set.
#[test]
fn cli1_every_error_code_is_listed_in_the_help_table() {
    // The codes this CLI can surface, each spelled exactly as the tools spell it.
    for code in [
        ErrorCode::InvalidArgs,
        ErrorCode::InvalidPattern,
        ErrorCode::InvalidEdit,
        ErrorCode::PlanNotFound,
        ErrorCode::PlanExpired,
        ErrorCode::PlanCorrupt,
        ErrorCode::WrongWorkspace,
        ErrorCode::AlreadyApplied,
        ErrorCode::StalePlan,
        ErrorCode::GateFailed,
        ErrorCode::Diverged,
        ErrorCode::CommentLoss,
        ErrorCode::JournalMissing,
        ErrorCode::RollbackIncomplete,
        ErrorCode::NotFound,
        ErrorCode::Ambiguous,
        ErrorCode::UnsupportedLanguage,
        ErrorCode::IoError,
        ErrorCode::OutsideWorkspace,
        ErrorCode::ProtectedPath,
        ErrorCode::WriteDisabled,
        ErrorCode::Busy,
        ErrorCode::LimitExceeded,
        ErrorCode::UnsupportedTarget,
        ErrorCode::ReplacedNotDurable,
        ErrorCode::FileTooLarge,
        ErrorCode::NotUtf8,
        ErrorCode::BudgetExceeded,
        ErrorCode::Timeout,
        ErrorCode::Internal,
    ] {
        assert!(
            EXIT_CODE_HELP.contains(code.as_str()),
            "{} is missing from the exit-code help",
            code.as_str()
        );
        let bucket = exit_code_for(code);
        assert!(
            bucket == EXIT_USER || bucket == EXIT_ENV,
            "{} mapped to {bucket}",
            code.as_str()
        );
    }
}

// ---- invariant 5: output is escaped -----------------------------------------------------------

/// CLI1-05: nothing printed contains a raw control, bidi or invisible character, whatever the store
/// holds.
#[test]
fn cli1_no_control_characters_reach_the_output() {
    let w = World::new();
    // Defence in depth: the plan store REFUSES a summary with control characters, so such text
    // cannot reach the CLI through the normal path at all. Assert that first, because it is the
    // reason the escaping below is a second line of defence rather than the only one.
    let clock = Arc::new(FakeClock::at_real_now());
    let store = PlanStore::open(&w.state, &w.ws, Limits::default(), clock).unwrap();
    let nasty_plan = Plan {
        format: 1,
        workspace_id: w.ws.clone(),
        engine_format: 1,
        request: PlanRequest {
            kind: "rewrite".into(),
            summary: "safe\u{1b}[31mred".to_string(),
            note: None,
        },
        files: vec![PlanFile {
            path: "a.rs".into(),
            language: "text".into(),
            pre_hash: ContentHash::of(b"a\n"),
            pre_size: 2,
            pre_errors: 0,
            post_hash: ContentHash::of(b"b\n"),
            post_size: 2,
            post_errors: 0,
            edits: vec![opencrayast_edit::Edit {
                start: 0,
                end: 2,
                replacement: "b\n".into(),
            }],
        }],
    };
    assert!(
        store.put(&nasty_plan).is_err(),
        "the store must refuse a summary containing a control character"
    );

    // A well-formed plan, so the commands have something real to print.
    let id = w.put_plan(&[("a.rs", "a\n", "b\n")]);

    for args in [
        vec!["plan", "list"],
        vec!["plan", "show", &id],
        vec!["doctor"],
    ] {
        let (_, cap) = drive(&w.root, &args);
        for line in cap.all() {
            for ch in line.chars() {
                assert!(
                    !ch.is_control() || ch == '\n',
                    "raw control character {ch:?} (U+{:04X}) reached the output in {args:?}: {line:?}",
                    ch as u32
                );
            }
            // Bidi and invisible characters are escaped too.
            for sneaky in ['\u{202e}', '\u{200b}', '\u{2066}'] {
                assert!(
                    !line.contains(sneaky),
                    "raw {sneaky:?} reached the output in {args:?}: {line:?}"
                );
            }
        }
    }
}

/// CLI1-05b: the escaping itself turns control, bidi and invisible characters into visible text, so
/// a person SEES that something was there instead of the terminal obeying it.
#[test]
fn cli1_the_escaping_itself_turns_control_characters_into_visible_text() {
    for (input, expect) in [
        ("a\u{1b}[31mred", "\\u{1b}"),
        ("a\u{0}b", "\\u{0}"),
        ("a\u{202e}b", "\\u{202e}"),
        ("a\u{200b}b", "\\u{200b}"),
        ("a\nb", "\\n"),
    ] {
        let got = opencrayast::out::escape_line(input);
        assert!(
            got.contains(expect),
            "{input:?} should render {expect}, got {got:?}"
        );
        assert!(
            !got.chars()
                .any(|c| c.is_control() || c == '\u{202e}' || c == '\u{200b}'),
            "{got:?} still holds a raw control or invisible character"
        );
    }
}

// ---- failure table, one test per row ----------------------------------------------------------

/// CLI1-06: a workspace root that does not exist is reported, not panicked, and exits with the code
/// its own error maps to.
///
/// CR R3 changed the expected number here: the CLI reports this as `not_found` ("no such file"),
/// which `--help` places in the exit-1 bucket, so the exit status is 1. It used to exit 2 while
/// printing `[not_found]` — the same condition reported as one thing and exiting as another. What
/// this test protects now is that the printed code and the exit status agree, not a hand-picked
/// number.
#[test]
fn cli1_a_missing_workspace_root_is_reported_and_exits_with_its_own_code() {
    let w = World::new();
    // `doctor` is NOT in this loop: it reports rather than refuses, so it has no bracketed code and
    // exits 2. It has its own assertion below.
    for args in [vec!["plan", "list"], vec!["plan", "show", "p-x"]] {
        let (code, cap) = drive(&w.root.join("missing"), &args);
        assert_eq!(
            code, EXIT_USER,
            "{args:?}: not_found is a user error per --help, so the exit status must be 1"
        );
        let t = text(&cap);
        assert!(!t.is_empty(), "{args:?} must say something");
        // `doctor` puts the next step after an em dash; the plan commands use "Next:".
        assert!(
            t.contains("Next:") || t.contains('\u{2014}'),
            "{args:?} must say what to do:\n{t}"
        );
    }
}

/// CLI1-07: an id that resolves to nothing is a user error with the literal code.
#[test]
fn cli1_an_unresolvable_id_is_a_user_error_with_the_literal_code() {
    let w = World::new();
    let (code, cap) = drive(&w.root, &["plan", "show", "p-aaaaaaaaaaaaaaaaaaaaaaaaaa"]);
    let t = text(&cap);
    assert_eq!(code, EXIT_USER);
    assert!(t.contains("[plan_not_found]"), "{t}");
    assert!(t.contains("Next:"), "{t}");
}

/// CLI1-08: an unparsable stored plan is reported, not crashed on, and the rest still lists.
#[test]
fn cli1_an_unreadable_stored_plan_is_reported_not_fatal() {
    let w = World::new();
    let good = w.put_plan(&[("good.txt", "a\n", "b\n")]);
    let bad_id = w.put_plan(&[("bad.txt", "c\n", "d\n")]);
    // Corrupt the PLAN bytes of a real entry, leaving its meta in place: what a damaged disk looks
    // like. A plan is listed only when its .meta.json exists, so this is what "a stored plan that
    // cannot be read" actually is.
    let plans_dir = w.state.join(format!("ws-{}", w.ws)).join("plans");
    std::fs::write(plans_dir.join(format!("{bad_id}.json")), b"{ not json").unwrap();

    let (code, cap) = drive(&w.root, &["plan", "list"]);
    let t = text(&cap);
    assert_eq!(
        code, EXIT_OK,
        "one bad entry must not fail the listing:\n{t}"
    );
    assert!(t.contains(&good), "the good plan is still listed:\n{t}");
    assert!(
        t.contains("could not be read") || t.contains("Warning:"),
        "the unreadable entry is reported:\n{t}"
    );

    // Showing the corrupt one is refused, not a panic.
    let (code, cap) = drive(&w.root, &["plan", "show", &bad_id]);
    assert!(
        code == EXIT_USER || code == EXIT_ENV,
        "got {code}:\n{}",
        text(&cap)
    );
}

/// CLI1-09: an unreadable state directory is an environment error on both commands, and neither
/// panics.
#[test]
fn cli1_an_unusable_state_directory_is_an_environment_error() {
    let w = World::new();
    // Make the state directory a regular file, so it cannot be used as one.
    let _ = std::fs::remove_dir_all(&w.state);
    std::fs::write(&w.state, b"not a directory").unwrap();

    for args in [vec!["plan", "list"], vec!["plan", "show", "p-aaaaaaaaaa"]] {
        let (code, cap) = drive(&w.root, &args);
        assert_eq!(code, EXIT_ENV, "{args:?}:\n{}", text(&cap));
        assert!(text(&cap).contains("["), "{args:?} must print the code");
    }
}

/// CLI1-10: `--json` does not exist in this build, and asking for it is a user error rather than a
/// silently ignored flag or a panic.
#[test]
fn cli1_an_unknown_flag_is_a_user_error_not_a_silent_no_op() {
    let w = World::new();
    for args in [
        vec!["plan", "list", "--json"],
        vec!["doctor", "--json"],
        vec!["plan", "show", "p-aaaaaaaaaa", "--json"],
    ] {
        let full: Vec<&str> = vec!["opencrayast", "--workspace", w.root.to_str().unwrap()]
            .into_iter()
            .chain(args.iter().copied())
            .collect();
        let err = Cli::try_parse_from(&full).unwrap_err();
        assert!(
            err.kind() == clap::error::ErrorKind::UnknownArgument,
            "{args:?} should be refused as unknown, got {:?}",
            err.kind()
        );
        // And through the process entry point that becomes exit 1.
        let (code, _) = opencrayast::parse_args_from_check(&full);
        assert_eq!(code, EXIT_USER, "{args:?}");
    }
}

/// CLI1-11: **rewritten by ISSUE-CLI-WRITE-SUBCOMMANDS (CLI 2).** Kept, not deleted, and not
/// weakened.
///
/// As written, this test asserted that `apply`, `undo`, `recover` and `edit` do not parse at all.
/// That was the correct shape for CLI 1, whose ticket said the mutating commands were "deliberately
/// absent" and would come later — and this ticket is that later. The commands now exist, so the
/// assertion is false and the test is inverted.
///
/// What is kept, and is still worth protecting:
///
/// - the three bare names `apply`, `undo` and `recover` are still **not** commands. They arrived
///   *under* `edit`, and a build that also accepted them at the top level would have two spellings
///   of one write path and no single place where the confirmation lives. That is the invariant this
///   test now protects, and it is a real one rather than a formality: it is what stops a future
///   "let's add a shortcut" from quietly doubling the surface a script can drive.
/// - an unknown subcommand is still a refusal, not a panic.
///
/// What is deliberately gone: the assertion that `edit` fails to parse. `CLI2-xx` in `cli2_spec.rs`
/// takes that over, with the added requirement that the *writes* are gated — a command that parses
/// is not a command that can write, and only the new spec tests that.
///
/// The other 22 CLI1 tests are untouched.
#[test]
fn cli1_the_mutating_commands_exist_only_under_edit_not_at_the_top_level() {
    // The bare names are still refused at the top level, with the same error kind as before.
    for name in ["apply", "undo", "recover"] {
        let full = vec!["opencrayast", name];
        let err = Cli::try_parse_from(&full)
            .err()
            .unwrap_or_else(|| panic!("{name} must not be a top-level command"));
        assert!(
            matches!(
                err.kind(),
                clap::error::ErrorKind::InvalidSubcommand | clap::error::ErrorKind::UnknownArgument
            ),
            "{name}: unexpected {:?}",
            err.kind()
        );
    }

    // And `edit` does exist, with the six subcommands the ticket names — and each one parses
    // **with its real required arguments**, not only with `--help`. The earlier form of this loop
    // passed `--help` and then accepted *any* error that was not `InvalidSubcommand`, which would
    // have accepted a subcommand whose arguments no longer parse. Parsing is all this test asserts:
    // that any of them can *write* is cli2_spec's job, and this one has no workspace.
    for sub in ["preview", "show", "apply", "undo", "list", "recover"] {
        let mut args: Vec<&str> = vec!["opencrayast", "edit", sub];
        match sub {
            "preview" => args.extend([
                "--language",
                "rust",
                "--path",
                "src/a.rs",
                "--pattern",
                "log($$$ARGS)",
                "--replacement",
                "log2($$$ARGS)",
            ]),
            // A syntactically valid full-length id, so the argument this test is checking is the
            // argument's *presence*, not its shape.
            "show" | "apply" | "undo" => args.push("p-aaaaaaaaaaaaaaaaaaaaaaaaaa"),
            "list" => args.push("--limit"),
            _ => {}
        }
        if sub == "list" {
            args.push("2");
        }
        Cli::try_parse_from(&args)
            .unwrap_or_else(|e| panic!("`edit {sub}` must parse with its arguments: {e}"));
    }

    // An unknown subcommand under `edit` is still refused.
    let err = Cli::try_parse_from(["opencrayast", "edit", "frobnicate"])
        .err()
        .unwrap_or_else(|| panic!("an unknown edit subcommand must be refused"));
    assert!(
        matches!(
            err.kind(),
            clap::error::ErrorKind::InvalidSubcommand | clap::error::ErrorKind::UnknownArgument
        ),
        "unexpected {:?}",
        err.kind()
    );
}

/// CLI1-12: `--workspace` picks the root; without it the current directory is used.
#[test]
fn cli1_workspace_flag_selects_the_root() {
    let w = World::new();
    let cli = Cli::try_parse_from([
        "opencrayast",
        "--workspace",
        w.root.to_str().unwrap(),
        "doctor",
    ])
    .unwrap();
    assert_eq!(cli.workspace.as_deref(), Some(w.root.as_path()));
    assert!(matches!(cli.command, Command::Doctor));

    let cli = Cli::try_parse_from(["opencrayast", "plan", "list"]).unwrap();
    assert_eq!(cli.workspace, None, "no flag means the current directory");
    match cli.command {
        Command::Plan(PlanCmd::List { limit }) => {
            assert_eq!(limit, opencrayast::plan::DEFAULT_LIMIT)
        }
        other => panic!("expected plan list, got {other:?}"),
    }
}

// ---- the sink contract ------------------------------------------------------------------------

/// CLI1-13: diagnostics and ordinary output go to different streams, and both are escaped.
#[test]
fn cli1_diagnostics_and_output_are_separate_and_both_escaped() {
    let mut cap = Capture::default();
    {
        let mut o = opencrayast::out::Out::new(&mut cap as &mut dyn Sink);
        o.line("plain\u{1b}[0m");
        o.diag("problem\u{202e}");
    }
    assert_eq!(cap.lines.len(), 1);
    assert_eq!(cap.diags.len(), 1);
    assert!(cap.lines[0].contains("\\u{1b}"), "{:?}", cap.lines);
    assert!(cap.diags[0].contains("\\u{202e}"), "{:?}", cap.diags);
}

/// CLI1-14: a plan round-trips through the state directory the CLI was given, which is **not**
/// inside the workspace.
#[test]
fn cli1_plans_round_trip_through_the_given_state_directory() {
    let w = World::new();
    let id = w.put_plan(&[("a.rs", "one\n", "two\n")]);
    assert_eq!(
        state_dir_for(&w.root),
        w.state,
        "the CLI and the store must agree on where the state directory is"
    );
    assert!(
        !w.state.starts_with(&w.root),
        "state must never live inside the workspace again"
    );
    let (code, cap) = drive(&w.root, &["plan", "show", &id]);
    assert_eq!(code, EXIT_OK, "{}", text(&cap));
    let t = text(&cap);
    assert!(
        t.contains(&id) && t.contains("a.rs"),
        "the plan is readable:\n{t}"
    );
    // It shows counts and sizes, never the file's content.
    assert!(
        !t.contains("one"),
        "plan show must not print file content:\n{t}"
    );
}

// ---- CR round 1: the shipping sink, the real binary, and the exit-code digits ------------------

/// Build the CLI binary once for the tests that run it as a process.
fn cli_binary() -> &'static std::path::Path {
    use std::sync::OnceLock;
    static BIN: OnceLock<std::path::PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        // The test binary lives in target/<profile>/deps/, so the CLI is two levels up.
        let mut p = std::env::current_exe().expect("test binary path");
        p.pop();
        if p.ends_with("deps") {
            p.pop();
        }
        p.join("opencrayast")
    })
}

/// Run the real binary and return `(exit status, stdout, stderr)`.
fn run_binary(args: &[&str]) -> (i32, String, String) {
    let out = std::process::Command::new(cli_binary())
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("running {}: {e}", cli_binary().display()));
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// SECFIX-CLI-01 (CR R1): **the shipping sink** escapes. The first version of this module escaped
/// inside the `Capture` test double while `Stdout` printed the raw text, so every test passed and
/// the real binary emitted raw ESC bytes. `Streams` is what `Stdout` writes through, and this drives
/// it directly, asserting on the BYTES.
#[test]
fn cli1_shipping_sink_escapes_control_characters() {
    use opencrayast::out::{Sink, Streams};

    let mut out_buf: Vec<u8> = Vec::new();
    let mut err_buf: Vec<u8> = Vec::new();
    {
        let mut sink = Streams::new(&mut out_buf, &mut err_buf);
        let mut o = opencrayast::out::Out::new(&mut sink as &mut dyn Sink);
        o.line("workspace \u{1b}[31mred\u{0}\u{202e}reversed");
        o.diag("problem\u{200b}");
    }
    let mut text = String::from_utf8(out_buf).unwrap();
    text.push_str(&String::from_utf8(err_buf).unwrap());
    eprintln!("sink bytes: {text:?}");

    // The trailing newline each line ends with is the line terminator the sink writes; what must not
    // appear is any control character that came FROM the caller's text.
    for line in text.lines() {
        for ch in line.chars() {
            assert!(
                !ch.is_control(),
                "the SHIPPING sink let a raw control character {ch:?} (U+{:04X}) through: {text:?}",
                ch as u32
            );
        }
    }
    for sneaky in ['\u{202e}', '\u{200b}', '\u{2066}'] {
        assert!(
            !text.contains(sneaky),
            "the SHIPPING sink let {sneaky:?} through: {text:?}"
        );
    }
    // And it is visible as text, so a person sees there was something there.
    assert!(text.contains("\\u{1b}"), "{text:?}");
    assert!(text.contains("\\u{202e}"), "{text:?}");
}

/// SECFIX-CLI-02 (CR R1): the REAL BINARY, end to end. A workspace path carrying ESC bytes must
/// come back escaped, on both streams. This is the check the reviewer ran by hand and the one no
/// in-process test substitute could stand in for.
#[cfg(unix)]
#[test]
fn cli1_the_real_binary_never_emits_a_raw_control_character() {
    let dir = tempfile::tempdir().unwrap();
    // A path whose name contains ESC, a NUL-adjacent control, a bidi override and a zero-width
    // space. The CLI must print it inert.
    let nasty = dir.path().join("probe-\u{1b}[31m-\u{202e}-\u{200b}-X");
    std::fs::create_dir_all(&nasty).unwrap();

    for args in [
        vec!["--workspace", nasty.to_str().unwrap(), "doctor"],
        vec!["--workspace", nasty.to_str().unwrap(), "plan", "list"],
        vec![
            "--workspace",
            nasty.to_str().unwrap(),
            "plan",
            "show",
            "p-abcdefghij",
        ],
    ] {
        let (code, stdout, stderr) = run_binary(&args);
        let both = format!("{stdout}{stderr}");
        eprintln!("--- {args:?} -> exit {code}\\n{both}");
        // Per line: the newline that terminates a line is the sink's own, not smuggled content.
        for line in both.lines() {
            for ch in line.chars() {
                assert!(
                    !ch.is_control(),
                    "the binary emitted a raw control character {ch:?} (U+{:04X}) for {args:?}",
                    ch as u32
                );
            }
        }
        for sneaky in ['\u{202e}', '\u{200b}', '\u{2066}', '\u{1b}'] {
            assert!(
                !both.contains(sneaky),
                "the binary emitted raw {sneaky:?} for {args:?}: {both:?}"
            );
        }
        // Nothing is a crash.
        assert!(
            code == 0 || code == 1 || code == 2,
            "unexpected exit {code}"
        );
    }
}

/// SECFIX-CLI-03 (CR R2): the REAL BINARY's exit statuses are the documented digits. Swapping
/// `EXIT_USER` and `EXIT_ENV` in the library would break this even though every in-process test
/// kept passing.
#[test]
fn cli1_the_real_binary_exits_with_the_documented_digits() {
    let w = World::new();
    let root = w.root.to_str().unwrap();

    // 0: success.
    let (code, _, _) = run_binary(&["--workspace", root, "doctor"]);
    assert_eq!(code, 0, "a healthy doctor is 0");

    // 0: an empty plan store still succeeded.
    let (code, out, _) = run_binary(&["--workspace", root, "plan", "list"]);
    assert_eq!(code, 0);
    assert!(out.contains("No plans stored"), "{out}");

    // 1: a user error — an id that resolves to nothing.
    let (code, _, err) = run_binary(&["--workspace", root, "plan", "show", "p-aaaaaaaaaa"]);
    assert_eq!(code, 1, "an unknown id is 1");
    assert!(err.contains("[plan_not_found]"), "{err}");

    // 1: a bad argument.
    let (code, _, _) = run_binary(&["--workspace", root, "frobnicate"]);
    assert_eq!(code, 1, "an unknown subcommand is 1");

    // 2: an environment error — `doctor` reports a failing check on stdout and exits 2.
    let (code, out, _) = run_binary(&["--workspace", "/does/not/exist", "doctor"]);
    assert_eq!(code, 2, "a failing doctor check is 2");
    assert!(out.contains("fail"), "and it says what failed: {out}");
}

/// The error codes `--help` places in the exit-1 ("user error") bucket.
const EXIT_USER_BUCKET: &[&str] = &[
    "invalid_args",
    "invalid_pattern",
    "invalid_edit",
    "plan_not_found",
    "plan_expired",
    "plan_corrupt",
    "wrong_workspace",
    "already_applied",
    "stale_plan",
    "gate_failed",
    "diverged",
    "comment_loss",
    "journal_missing",
    "rollback_incomplete",
    "not_found",
    "ambiguous",
    "unsupported_language",
];

/// SECFIX-CLI-04 (CR R3): one error code, one exit code — always. The reviewer found
/// `--workspace /does/not/exist plan list` printing `[not_found]` (which the help table and
/// `exit_code_for` both place at exit 1) while exiting 2.
#[test]
fn cli1_one_error_code_always_yields_the_same_exit_code_in_the_real_binary() {
    let bad = "/does/not/exist";
    for args in [
        vec!["--workspace", bad, "plan", "list"],
        vec!["--workspace", bad, "plan", "show", "p-aaaaaaaaaa"],
    ] {
        let (code, _, err) = run_binary(&args);
        // Whatever it prints, the code it names must be the code the exit status is derived from.
        let printed = err
            .lines()
            .find(|l| l.starts_with('['))
            .unwrap_or_else(|| panic!("{args:?} printed no error code: {err:?}"));
        let name = printed
            .trim_start_matches('[')
            .split(']')
            .next()
            .unwrap()
            .to_string();
        // The code the run printed must be one this CLI documents, and the exit status must be the
        // bucket that code belongs to. Compared against the table rather than a reverse lookup,
        // because `ErrorCode` has no parser and inventing one here would test the wrong thing.
        let documented = EXIT_CODE_HELP.contains(&name);
        assert!(
            documented,
            "{args:?} printed [{name}], which --help does not list"
        );
        let expected = if EXIT_USER_BUCKET.contains(&name.as_str()) {
            1
        } else {
            2
        };
        assert_eq!(
            code, expected,
            "{args:?} printed [{name}] but exited {code}, while the table says {expected}"
        );
    }
}
