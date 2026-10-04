//! Spec for the non-interactive guard on `edit apply` (REQ-CLI-HUMAN, acceptance criterion
//! 「非互動環境沒有 `--yes` 時拒絕套用」).
//!
//! # How these tests are deterministic
//!
//! **No test in this file asks whether stdin is a terminal.** Every case drives
//! [`opencrayast::run_with`] with an explicit [`Interaction`] and a
//! [`ScriptedConfirmer`], so "interactive", "non-interactive", "confirmed" and "declined" are
//! arguments, not properties of the machine running the suite. That is deliberate and it is the
//! point of the whole module: a test that inspected its own stdin would pass on a developer's
//! terminal, fail in CI where stdin is `/dev/null`, and the only way to make it pass in CI is to
//! delete it — a green suite that has quietly stopped testing the branch.
//!
//! # What a refusal must prove
//!
//! Not only an exit code: **zero writes**. A guard that prints the right thing and then applies
//! anyway is worse than no guard, because it looks like one. So the refusal cases assert the file
//! on disk still holds its original bytes *and* that the plan is still unapplied in the journal —
//! and they assert it by reading the filesystem, not by trusting the exit code.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use clap::{CommandFactory, Parser};
use opencrayast::confirm::{self, Interaction, ScriptedConfirmer};
use opencrayast::exit::{CONFIRM_HELP, EXIT_OK, EXIT_USER};
use opencrayast::out::Capture;
use opencrayast::palette::Palette;
use opencrayast::{Cli, Command, EditCmd};
use opencrayast_core::ErrorCode;
use opencrayast_core::error::ToolError;
use opencrayast_core::hash::ContentHash;
use opencrayast_core::limits::Limits;
use opencrayast_edit::{
    JournalState, JournalStore, Plan, PlanFile, PlanRequest, PlanStore, SystemClock,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A temporary workspace with a state directory and one stored plan.
struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
    state: PathBuf,
    ws: String,
}

/// The one file every case applies to, with its original and replaced contents.
const FILE: &str = "src/a.rs";
const BEFORE: &str = "fn main() {}\n";
const AFTER: &str = "fn main() { run(); }\n";

impl World {
    fn new() -> World {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join(FILE), BEFORE).unwrap();
        // Outside the workspace: the tool's state is no longer a dotfile inside the tree an
        // agent can read, and a test that resolved the real one would write into the
        // developer's `$XDG_STATE_HOME`.
        let state = dir.path().join("state");
        let ws = opencrayast_core::workspace::workspace_id(&root).unwrap();
        World {
            _dir: dir,
            root,
            state,
            ws,
        }
    }

    /// Store a plan replacing [`BEFORE`] with [`AFTER`] in [`FILE`]. Returns its full id.
    fn put_plan(&self) -> String {
        let clock = Arc::new(SystemClock);
        let store = PlanStore::open(&self.state, &self.ws, Limits::default(), clock).unwrap();
        let plan = Plan {
            format: 1,
            workspace_id: self.ws.clone(),
            engine_format: 1,
            request: PlanRequest {
                kind: "rewrite".into(),
                summary: "guard spec".into(),
                note: None,
            },
            files: vec![PlanFile {
                path: FILE.to_string(),
                language: "rust".into(),
                pre_hash: ContentHash::of(BEFORE.as_bytes()),
                pre_size: BEFORE.len() as u64,
                pre_errors: 0,
                post_hash: ContentHash::of(AFTER.as_bytes()),
                post_size: AFTER.len() as u64,
                post_errors: 0,
                edits: vec![opencrayast_edit::Edit {
                    start: 0,
                    end: BEFORE.len(),
                    replacement: AFTER.to_string(),
                }],
            }],
        };
        store.put(&plan).unwrap().0
    }

    /// Configuration with writing enabled, as an operator would set it up. The write capability is
    /// minted from the parsed file exactly as `doctor` does, so these cases exercise the same door
    /// production does and cannot pass by accident through a test-only bypass (WCAP-1).
    ///
    /// The file is set to 0600 explicitly: `Settings::load` refuses a configuration that is
    /// readable by group or others, and a `tempfile` directory's mode depends on the process
    /// umask — which would make this fixture fail on one machine and pass on another for a reason
    /// that has nothing to do with the code under test.
    fn write_settings(&self) -> PathBuf {
        let cfg = self.root.join("config.toml");
        std::fs::write(&cfg, "[policy]\nallow_write = true\n").unwrap();
        set_private(&cfg);
        // Loading it here proves the fixture's own configuration is acceptable, so a later
        // failure is about the CLI and not about the fixture.
        opencrayast_core::config::Settings::load(&cfg).unwrap();
        cfg
    }

    /// The bytes currently in the file.
    fn file_bytes(&self) -> String {
        std::fs::read_to_string(self.root.join(FILE)).unwrap()
    }

    /// Whether a journal says this plan was applied.
    fn is_applied(&self, plan_id: &str) -> bool {
        let clock = Arc::new(SystemClock);
        let store = JournalStore::open(&self.state, &self.ws, Limits::default(), clock).unwrap();
        matches!(
            store.load(plan_id).map(|m| m.state),
            Ok(JournalState::Applied)
        )
    }
}

/// Drive one invocation with an explicit environment. This is the seam that makes the whole matrix
/// reproducible on any machine.
fn drive(
    root: &Path,
    config: &Path,
    args: &[&str],
    interaction: Interaction,
    answers: &[bool],
) -> (i32, Capture) {
    // The state directory this fixture owns, beside the workspace and not inside it — the same
    // seam the palette and the confirmer are.
    let state = root.parent().unwrap_or(Path::new(".")).join("state");
    drive_expecting(root, &state, config, args, interaction, answers, true)
}

/// [`drive`], plus the right to say that a scripted answer was *meant* to go unasked.
///
/// `expect_all_asked` is false only for cases that deliberately fail before the gate — a broken
/// configuration, say. Asserting "every scripted answer was used" is a strong check on the cases
/// that are about the gate (it catches a command that silently stopped asking), and a wrong check on
/// the ones that are about something else, so it is a parameter rather than being deleted.
#[allow(clippy::too_many_arguments)]
fn drive_expecting(
    root: &Path,
    state: &Path,
    config: &Path,
    args: &[&str],
    interaction: Interaction,
    answers: &[bool],
    expect_all_asked: bool,
) -> (i32, Capture) {
    // `--write` plus the fixture's `allow_write = true`: write mode is two gates, and this suite
    // is about the third (the confirmation), so it satisfies the first two and varies only the
    // third. `cli2_write_mode_needs_the_flag_and_the_configuration` is the test that pins them.
    let mut full: Vec<String> = vec![
        "opencrayast".into(),
        "--workspace".into(),
        root.to_str().unwrap().into(),
        "--config".into(),
        config.to_str().unwrap().into(),
        "--write".into(),
    ];
    full.extend(args.iter().map(|s| (*s).to_string()));
    let refs: Vec<&str> = full.iter().map(String::as_str).collect();
    let cli = Cli::try_parse_from(&refs).unwrap_or_else(|e| panic!("{e}"));
    // The interaction is the spec's statement about *this run's* environment, and it is the one
    // thing `run_with` no longer takes directly: it is answered by the confirmer's `may_decide`,
    // which is where the gate reads it from. The palette is injected as off so no assertion here
    // depends on the terminal this suite runs in.
    let mut confirmer = ScriptedConfirmer::attended(interaction, answers.iter().copied());
    let mut cap = Capture::default();
    let code = opencrayast::run_with_state(
        &cli,
        &mut cap,
        Palette::new(false),
        &mut confirmer,
        &opencrayast::StateDir::Fixed(state),
    );
    if expect_all_asked {
        // Both directions. "Some scripted answers went unused" means the command never asked;
        // "more questions than scripted" means it asked something this case did not anticipate.
        // Either way the command did not take the path this case is about. (An unscripted question
        // is answered "no", so a stray extra prompt shows up as a refused apply rather than as a
        // silent pass.)
        assert_eq!(
            confirmer.asked(),
            answers.len(),
            "{args:?}: expected {} question(s), the command asked {} — it did not take the path \
             this case is about",
            answers.len(),
            confirmer.asked()
        );
        assert_eq!(
            confirmer.remaining(),
            0,
            "{args:?}: {} scripted answer(s) went unasked",
            answers.len()
        );
    }
    (code, cap)
}

fn text(cap: &Capture) -> String {
    cap.all().join("\n")
}

/// Make `p` readable and writable by its owner only.
///
/// `Settings::load` refuses a configuration anyone else can read, which is right in production and
/// a nuisance in a fixture: the mode `tempfile` gives a file depends on the process umask. Setting
/// it explicitly is what makes this spec give the same answer on every machine. Unix-only, as the
/// rest of this workspace's permission tests are (`core/tests/boundary_spec.rs`).
#[cfg(unix)]
fn set_private(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[cfg(not(unix))]
fn set_private(_p: &Path) {}

// ---- the four cases of the matrix -----------------------------------------------------------

/// CLI2-C01: interactive and confirmed → the apply happens, and the file on disk changed.
#[test]
fn cli2_c01_interactive_confirmed_applies() {
    let w = World::new();
    let id = w.put_plan();
    let settings_path = w.write_settings();

    // An apply needs BOTH write gates, and this spec is about the human gate, so both must be
    // open for the prompt to be the thing under test. `--write` is supplied by `drive`, which
    // puts it on every invocation; the configuration comes from the fixture.
    let (code, cap) = drive(
        &w.root,
        &settings_path,
        &["edit", "apply", &id],
        Interaction::Interactive,
        &[true],
    );
    let t = text(&cap);

    assert_eq!(code, EXIT_OK, "a confirmed apply succeeds:\n{t}");
    assert!(
        t.contains(confirm::APPLY_PROMPT),
        "the prompt must be shown:\n{t}"
    );
    assert!(
        t.contains(FILE),
        "the file list must be shown before asking:\n{t}"
    );
    assert_eq!(w.file_bytes(), AFTER, "the file must actually have changed");
    assert!(w.is_applied(&id), "the journal must record the apply");
}

/// CLI2-C02: non-interactive **with** `--yes` → the apply happens. `--yes` is consent given in
/// advance, and it is the only door into an unattended apply.
#[test]
fn cli2_c02_non_interactive_with_yes_applies() {
    let w = World::new();
    let id = w.put_plan();
    let settings_path = w.write_settings();

    let (code, cap) = drive(
        &w.root,
        &settings_path,
        // Both write gates open, so what is under test is consent, not policy: `--write` comes
        // from `drive` and `policy.allow_write` from the fixture.
        &["edit", "apply", &id, "--yes"],
        Interaction::NonInteractive,
        &[], // no answer scripted: --yes must answer the question by itself
    );
    let t = text(&cap);

    assert_eq!(code, EXIT_OK, "--yes in a pipe is the supported path:\n{t}");
    assert_eq!(w.file_bytes(), AFTER, "the file must actually have changed");
    assert!(w.is_applied(&id), "the journal must record the apply");
    assert!(
        !t.contains(confirm::APPLY_PROMPT),
        "--yes answered in advance; nobody should be asked:\n{t}"
    );
}

/// CLI2-C03: **the acceptance criterion.** Non-interactive without `--yes` → refuses, with the
/// documented code, a next step, and **nothing written**.
#[test]
fn cli2_c03_non_interactive_without_yes_refuses_and_writes_nothing() {
    let w = World::new();
    let id = w.put_plan();
    let settings_path = w.write_settings();

    let (code, cap) = drive(
        &w.root,
        &settings_path,
        &["edit", "apply", &id],
        Interaction::NonInteractive,
        &[],
    );
    let t = text(&cap);

    // The documented code and its documented exit status. The exit code is asked of the shared
    // `exit_code_for`, not restated here, so this test would catch the CLI inventing a private
    // exit bucket for its own refusal.
    assert_eq!(code, EXIT_USER, "a refusal is a user error:\n{t}");
    assert_eq!(confirm::refusal_code(), ErrorCode::InvalidArgs);
    assert!(
        t.contains(&format!("[{}]", ErrorCode::InvalidArgs.as_str())),
        "the refusal must print the documented code:\n{t}"
    );

    // A next step, naming the command that works — a refusal that does not say what to do gets
    // `--yes` added by reflex.
    assert!(t.contains("Next:"), "a refusal must say what to do:\n{t}");
    assert!(t.contains("--yes"), "the next step must name --yes:\n{t}");

    // Zero writes. This is the part a bare exit-code assertion would miss.
    assert_eq!(
        w.file_bytes(),
        BEFORE,
        "a refused apply must not have changed the file"
    );
    assert!(
        !w.is_applied(&id),
        "a refused apply must not have written a journal entry"
    );
}

/// CLI2-C04: interactive but declined → refuses cleanly. A decline is not an error and not a
/// panic; it is a person saying no, and the workspace is untouched.
#[test]
fn cli2_c04_interactive_declined_refuses_cleanly() {
    let w = World::new();
    let id = w.put_plan();
    let settings_path = w.write_settings();

    let (code, cap) = drive(
        &w.root,
        &settings_path,
        &["edit", "apply", &id],
        Interaction::Interactive,
        &[false],
    );
    let t = text(&cap);

    assert_eq!(
        code, EXIT_USER,
        "declining is a user error, not a crash:\n{t}"
    );
    assert!(t.contains("declined"), "the decline must be reported:\n{t}");
    assert_eq!(
        w.file_bytes(),
        BEFORE,
        "a declined apply must not have changed the file"
    );
    assert!(!w.is_applied(&id), "a declined apply writes no journal");
}

/// CLI2-C05: the matrix, exhaustively. Every cell of interactive × `--yes` × answer gets the
/// outcome the help text promises, so a future change to one branch cannot quietly disagree with
/// another.
#[test]
fn cli2_c05_the_whole_matrix_holds() {
    // (interactive, yes, answers, expect_applied)
    let cases: &[(bool, bool, &[bool], bool)] = &[
        (true, false, &[true], true),
        (true, false, &[false], false),
        (true, true, &[], true),    // --yes wins over the prompt
        (false, true, &[], true),   // the documented unattended path
        (false, false, &[], false), // THE ACCEPTANCE CRITERION
    ];

    for (interactive, yes, answers, expect_applied) in cases {
        let w = World::new();
        let id = w.put_plan();
        let settings_path = w.write_settings();

        // Both write gates stay open for the whole matrix, so the only variable is the human gate.
        // `drive` puts `--write` on every invocation; a cell without it would refuse earlier with
        // `[write_disabled]` and prove nothing about consent — that is the SEC-2 round-2 finding,
        // pinned separately in `sec_audit2_r2_cli_poc`.
        let mut args = vec!["edit", "apply", id.as_str()];
        if *yes {
            args.push("--yes");
        }
        let interaction = if *interactive {
            Interaction::Interactive
        } else {
            Interaction::NonInteractive
        };

        let (code, cap) = drive(&w.root, &settings_path, &args, interaction, answers);
        let t = text(&cap);

        assert_eq!(
            w.file_bytes(),
            if *expect_applied { AFTER } else { BEFORE },
            "interactive={interactive} yes={yes} answers={answers:?} applied={expect_applied} \
             but the file says otherwise:\n{t}"
        );
        assert_eq!(
            w.is_applied(&id),
            *expect_applied,
            "interactive={interactive} yes={yes} answers={answers:?}: the journal disagrees with \
             the file:\n{t}"
        );
        assert_eq!(
            code,
            if *expect_applied { EXIT_OK } else { EXIT_USER },
            "interactive={interactive} yes={yes} answers={answers:?} gave the wrong exit code:\n{t}"
        );
    }
}

// ---- what the refusal claims, and what it must therefore contain ---------------------------

/// CLI2-C06: the refusal goes through the shared error taxonomy, so a script sees the same code a
/// tool would give and the same exit-code table `--help` documents.
#[test]
fn cli2_c06_refusal_uses_the_shared_error_taxonomy() {
    let r = confirm::Refusal {
        code: confirm::refusal_code(),
        message: "m".into(),
        next: confirm::REFUSAL_NEXT.into(),
    };
    assert_eq!(r.code, ErrorCode::InvalidArgs);
    // Through `exit_code_for`, the same function every other error uses — so the CLI cannot have
    // a refusal exit code that `--help` does not document.
    assert_eq!(r.exit_code(), EXIT_USER);
    let e: ToolError = r.to_error();
    assert_eq!(e.code, ErrorCode::InvalidArgs);
    assert!(!e.next.is_empty(), "an error with no next step is not one");

    // And the help table does document this code, in the exit-1 bucket.
    assert!(
        opencrayast::exit::EXIT_CODE_HELP.contains("invalid_args"),
        "invalid_args must appear in the help table"
    );
}

/// CLI2-C07: the help text and the refusal message agree about what to do. A person who reads
/// `--help` and a script that reads stderr are being told the same thing; if the messages drift
/// apart, one of them is wrong and this test says which.
#[test]
fn cli2_c07_help_documents_exactly_what_the_code_does() {
    assert!(CONFIRM_HELP.contains("--yes"));
    assert!(CONFIRM_HELP.contains("invalid_args"));
    // The exit status is wrapped across two lines in the help, so assert on what is true rather
    // than on the layout: the refusal names `invalid_args` and the number 1.
    assert!(CONFIRM_HELP.contains("exit"), "{}", CONFIRM_HELP);
    assert!(
        CONFIRM_HELP.contains("[invalid_args] and exit\n  1"),
        "the refusal's exit status must be stated: {}",
        CONFIRM_HELP
    );
    // "Nothing is written in either refusal" is a claim about the filesystem; C03/C04 test it.
    assert!(CONFIRM_HELP.contains("Nothing is written"));
    // All three of undo/recover are confirmed by the same gate, so the help says so.
    for word in ["apply", "undo", "recover"] {
        assert!(CONFIRM_HELP.contains(word), "CONFIRM_HELP must name {word}");
    }
    // And the three sections `--help` prints are the three this module exports, read through one
    // function so the copies inside HELP_TAIL cannot drift from the named constants.
    for (name, needle) in [("write", "allow_write = true"), ("color", "NO_COLOR")] {
        assert!(
            opencrayast::exit::help_section(name).contains(needle),
            "the {name} section must state {needle}"
        );
        assert!(
            opencrayast::exit::HELP_TAIL.contains(needle),
            "HELP_TAIL repeats the {name} section and has drifted from it"
        );
    }
    // The next step names the same command the help describes.
    assert!(
        confirm::REFUSAL_NEXT.contains("--yes"),
        "the refusal's next step must name --yes"
    );
    assert!(
        confirm::REFUSAL_NEXT.contains("opencrayast edit apply"),
        "the refusal's next step must name the real command"
    );
}

/// CLI2-C08: `--help` for the command itself, not just the global text.
#[test]
fn cli2_c08_the_apply_subcommand_documents_its_gate() {
    let mut cmd = Cli::command();
    let help = cmd.render_long_help().to_string();
    assert!(
        help.contains("Confirmation (edit apply, undo, recover)"),
        "{help}"
    );
    // The write gates are documented on the same page a person reaches before the confirmation.
    assert!(help.contains("Write mode needs two gates"), "{help}");
    // The flag exists and is described, so `--yes` is discoverable before it is needed.
    assert!(help.contains("--yes"), "{help}");

    let sub = opencrayast::EditCmd::Apply {
        plan_id: "p-x".into(),
    };
    assert!(matches!(sub, EditCmd::Apply { .. }));
    let _ = Command::Edit(EditCmd::Apply {
        plan_id: "p-x".into(),
    });
}

/// A syntactically valid, full-length plan id that resolves to nothing. 26 body characters in the
/// base32 alphabet `is_full_plan_id` accepts, so this is "absent", not "malformed".
///
/// Full-length on purpose: this case is "the plan does not exist", and "that is a prefix" is a
/// different condition with its own test (`cli2_row_a_prefix_given_to_apply_or_undo`). Using a short
/// id here would test the prefix rule twice and this case never.
const MISSING_PLAN_ID: &str = "p-aaaaaaaaaaaaaaaaaaaaaaaaaa";

/// CLI2-C09: `--yes` does not paper over a plan that does not exist, and does not make a missing
/// workspace environment error look like a refusal. The gate answers "may this apply"; it does not
/// decide whether the apply is possible.
#[test]
fn cli2_c09_the_gate_does_not_mask_other_errors() {
    let w = World::new();
    let settings_path = w.write_settings();

    // A plan id that resolves to nothing is `plan_not_found` (exit 1), not a confirmation refusal.
    let (code, cap) = drive(
        &w.root,
        &settings_path,
        &["edit", "apply", MISSING_PLAN_ID, "--yes"],
        Interaction::NonInteractive,
        &[],
    );
    let t = text(&cap);
    assert_eq!(code, EXIT_USER, "{t}");
    assert!(
        t.contains(ErrorCode::PlanNotFound.as_str()),
        "an unknown plan must be reported as such, not as a refusal:\n{t}"
    );
    assert_eq!(w.file_bytes(), BEFORE, "and nothing is written");
}

/// CLI2-C10: a bad configuration is reported before the gate — because there is no point asking
/// a person to confirm an apply that policy forbids.
///
/// This used to assert exit 2, on the reasoning that a configuration problem is "the
/// environment". That flattened every configuration refusal to one number and is what made the
/// CLI and the MCP server disagree about all of them. A file that does not parse is the
/// operator's own text being wrong: retrying unchanged fails identically, and no permission or
/// `chmod` fixes it, so it is a **user** error, exit 1. An untrustworthy file — wrong owner,
/// wrong permissions — is the machine's, and stays at exit 2 (see
/// `crates/mcp/tests/exit_code_parity_spec.rs`, which compares both shells).
#[test]
fn cli2_c10_configuration_is_reported_before_the_gate() {
    let w = World::new();
    let id = w.put_plan();
    let bad = w.root.join("bad.toml");
    std::fs::write(&bad, "this is not a config file\n").unwrap();
    set_private(&bad);

    // Interactive, so that if the gate ran at all it would ask — the answer being left unasked is
    // what proves it did not, so this case opts out of that assertion.
    let (code, cap) = drive_expecting(
        &w.root,
        &w.state,
        &bad,
        &["edit", "apply", &id],
        Interaction::Interactive,
        &[true],
        false,
    );
    let t = text(&cap);

    assert_eq!(
        code, EXIT_USER,
        "a malformed configuration is the operator's text, not the machine:\n{t}"
    );
    assert!(
        t.contains(ErrorCode::InvalidArgs.as_str()),
        "and it is reported as a syntax refusal:\n{t}"
    );
    assert_eq!(w.file_bytes(), BEFORE);
}

/// CLI2-C11: the two refusals are distinguishable in the output. Both are the same code and exit
/// status — they are the same kind of event — but a person reading the terminal needs to know
/// which happened, and "one of the two noes" is not a usable message.
#[test]
fn cli2_c11_the_two_refusals_read_differently() {
    let w = World::new();
    let settings_path = w.write_settings();
    let id = w.put_plan();

    let (_, non_interactive) = drive(
        &w.root,
        &settings_path,
        &["edit", "apply", &id],
        Interaction::NonInteractive,
        &[],
    );
    let (_, declined) = drive(
        &w.root,
        &settings_path,
        &["edit", "apply", &id],
        Interaction::Interactive,
        &[false],
    );

    let a = text(&non_interactive);
    let b = text(&declined);
    assert!(a.contains("non-interactive"), "{a}");
    assert!(b.contains("declined"), "{b}");
    assert_ne!(a, b, "the two refusals must not read identically");
}

/// CLI2-C12: the file list at the prompt comes from the stored plan, and the prompt escapes it on
/// the way out.
///
/// The first version of this test tried to store a plan over a file named `src/a\u{1b}[31m.rs`
/// and assert the ESC was escaped at the prompt. The plan store refused to store it at all:
/// `plan_corrupt` — "file 0 path contains a backslash or control character". That is a better
/// guarantee than the one the test was reaching for, so the test asserts *both* halves of it: L3
/// refuses such a path outright, and the CLI's own escaping funnel is there regardless.
#[cfg(unix)]
#[test]
fn cli2_c12_a_control_character_in_a_planned_path_never_reaches_the_prompt() {
    let w = World::new();
    // Proving the fixture's configuration is loadable is part of the setup; this case never
    // reaches it, because the plan cannot be stored.
    let _ = w.write_settings();

    let clock = Arc::new(SystemClock);
    let store = PlanStore::open(&w.state, &w.ws, Limits::default(), clock).unwrap();
    let evil = "src/a\u{1b}[31m.rs";
    std::fs::write(w.root.join(evil), BEFORE).unwrap();
    let plan = Plan {
        format: 1,
        workspace_id: w.ws.clone(),
        engine_format: 1,
        request: PlanRequest {
            kind: "rewrite".into(),
            summary: "s".into(),
            note: None,
        },
        files: vec![PlanFile {
            path: evil.to_string(),
            language: "rust".into(),
            pre_hash: ContentHash::of(BEFORE.as_bytes()),
            pre_size: BEFORE.len() as u64,
            pre_errors: 0,
            post_hash: ContentHash::of(AFTER.as_bytes()),
            post_size: AFTER.len() as u64,
            post_errors: 0,
            edits: vec![opencrayast_edit::Edit {
                start: 0,
                end: BEFORE.len(),
                replacement: AFTER.to_string(),
            }],
        }],
    };
    // The store is the one that refuses: a control character in a planned path is not stored, so
    // there is no plan for the prompt to display and nothing for a terminal to interpret.
    let refused = store.put(&plan).unwrap_err();
    assert_eq!(
        refused.code,
        ErrorCode::PlanCorrupt,
        "a control character in a planned path must not be storable: {refused:?}"
    );

    // And the CLI's escaping funnel is independently live: `Out` escapes, so the file list printed
    // at the prompt cannot carry a raw control byte even if some future L3 let one through.
    assert_eq!(opencrayast::out::escape_line(evil), "src/a\\u{1b}[31m.rs");
    assert!(
        !opencrayast::out::escape_line(evil).contains('\u{1b}'),
        "the escape must be visible text, not the control byte itself"
    );
}

/// CLI2-C13: the question the person is actually asked is the documented one.
///
/// Asserting only the *count* of questions would pass if the command asked something else entirely
/// — "proceed? [y/N]" and "delete your home directory? [y/N]" both ask once. This pins the wording
/// to [`confirm::APPLY_PROMPT`], which the help text and the module docs also refer to, so the
/// prompt cannot quietly become a different question while every other test stays green.
#[test]
fn cli2_c13_the_question_asked_is_the_documented_one() {
    let w = World::new();
    let id = w.put_plan();
    let settings_path = w.write_settings();

    let full: Vec<String> = vec![
        "opencrayast".into(),
        "--workspace".into(),
        w.root.to_str().unwrap().into(),
        "--config".into(),
        settings_path.to_str().unwrap().into(),
        "--write".into(),
        "edit".into(),
        "apply".into(),
        id.clone(),
    ];
    let refs: Vec<&str> = full.iter().map(String::as_str).collect();
    let cli = Cli::try_parse_from(&refs).unwrap_or_else(|e| panic!("{e}"));
    let mut confirmer = ScriptedConfirmer::attended(Interaction::Interactive, [true]);
    let mut cap = Capture::default();
    opencrayast::run_with_state(
        &cli,
        &mut cap,
        Palette::new(false),
        &mut confirmer,
        &opencrayast::StateDir::Fixed(&w.state),
    );

    // Exactly one question, and it is the documented one with the plan named: a person answering
    // "yes" has to know which plan they said yes to.
    let want = confirm::question(confirm::APPLY_PROMPT, Some(&id));
    assert_eq!(
        confirmer.prompts(),
        std::slice::from_ref(&want),
        "exactly one question, and it is the documented one"
    );
    assert!(
        confirm::APPLY_PROMPT.contains("Apply"),
        "the prompt must say what is being applied: {:?}",
        confirm::APPLY_PROMPT
    );
    assert!(
        want.contains(id.as_str()),
        "the question must name the plan: {want}"
    );
}

/// CLI2-C14: the documentation says what the code does.
///
/// A doc that overstates what a tool does is a defect the same way a help string is
/// (ISSUE-CLI-WRITE-SUBCOMMANDS invariant 7). This checks the claims that could silently rot:
/// each doc that mentions `opencrayast edit apply` must also mention the gate, so a future
/// rewrite that drops the caveat is caught here rather than by a person following stale advice
/// into a command that refuses them.
#[test]
fn cli2_c14_the_docs_document_the_gate() {
    // CARGO_MANIFEST_DIR uses OS separators; do not string-replace "/crates/cli".
    let docs = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("docs");
    for (file, needle) in [
        ("TOOLS.md", "--yes"),
        ("ARCHITECTURE.md", "--yes"),
        ("AGENT-GUIDE.md", "--yes"),
    ] {
        let path = std::path::Path::new(&docs).join(file);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        assert!(
            text.contains("opencrayast edit apply"),
            "{file} no longer mentions the apply command; this test needs updating"
        );
        assert!(
            text.contains(needle),
            "{file} tells people to run `opencrayast edit apply` without documenting {needle} — \
             that is the same gap this ticket is about"
        );
    }

    // The TOOLS.md table itself must state the refusal outcome, not imply the apply always runs.
    let tools = std::fs::read_to_string(std::path::Path::new(&docs).join("TOOLS.md")).unwrap();
    assert!(
        tools.contains("refuses"),
        "TOOLS.md must say the non-interactive case refuses"
    );
    assert!(
        tools.contains("Nothing is written on either refusal"),
        "TOOLS.md must state the zero-write property, which C03/C04 test"
    );
}
