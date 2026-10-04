//! Spec for three CLI branches that no test reached.
//!
//! Each of these is a line that a mutation can delete and the workspace stays green, because the
//! tests that "covered" the neighbourhood only covered the neighbouring line:
//!
//! - [`exit_code_of`] was pinned for `--help` but not for `--version`, so dropping
//!   `DisplayVersion` from the `EXIT_OK` arm made `--version` exit 1 and nothing noticed;
//! - [`workspace_or_report`] reports the error's own code, but flattening it to `ErrorCode::Env`
//!   was equally invisible, because the only failing case a test had was one whose own code
//!   already mapped to the environment bucket;
//! - the configuration is fail-closed, which means an unacceptable file must be an **error** —
//!   not a default. That promise is stated in the `--config` help and honoured in the error arm,
//!   but the arm that *would* silently swallow it was not reachable from any test.
//!
//! # The code is correct; the tests were missing
//!
//! Every assertion here describes behaviour that already held. These are regression fences, not
//! bug reports — except where a comment says otherwise.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use clap::Parser;
use opencrayast::exit::{EXIT_ENV, EXIT_OK, EXIT_USER};
use opencrayast::out::Capture;
use opencrayast::palette::Palette;
use opencrayast::{Cli, run_with};
use opencrayast_core::ErrorCode;
use std::path::Path;

// ---------------------------------------------------------------------------
// (5c) `--version` is an answer, not a user error.
// ---------------------------------------------------------------------------

/// `--version` exits 0.
///
/// `exit_code_of` maps a clap error kind to a process exit code, and `DisplayVersion` sits in the
/// same arm as `DisplayHelp`: a person who asked what version this is got an answer, so the
/// process succeeded. Deleting `DisplayVersion` from that arm makes `--version` exit 1 — the code
/// that means "you used this program wrong" — and the previous suite was green, because the only
/// test of that arm was a `--help` test.
///
/// This is a real behaviour, not a cosmetic one: scripts gate on exit status, and a version probe
/// that reports failure looks like a broken install rather than an old binary.
#[test]
fn version_exits_ok_because_the_question_was_answered() {
    let (code, err) = opencrayast::parse_args_from_check(&["opencrayast", "--version"]);
    assert_eq!(
        err.kind(),
        clap::error::ErrorKind::DisplayVersion,
        "the request must actually reach the version branch"
    );
    assert_eq!(
        code, EXIT_OK,
        "--version is an answer, so it exits 0, not the user-error bucket"
    );

    // And it is reported differently from a genuine user error, which is the contrast that makes
    // the first assertion mean something: both are clap errors, and the code is what separates
    // "here is your version" from "you typed that wrong".
    let (bad_code, _) = opencrayast::parse_args_from_check(&["opencrayast", "--config"]);
    assert_eq!(
        bad_code, EXIT_USER,
        "a missing argument is still a user error, so the two must not collapse together"
    );
}

/// `--help` and `--version` agree with each other, and both disagree with a mistake.
///
/// The two are the same class of event and go through one function on purpose, so they are
/// asserted together: a change that moved one out of the `EXIT_OK` arm without moving the other
/// would be exactly the drift this pins.
#[test]
fn help_and_version_agree_and_both_differ_from_an_unknown_flag() {
    for args in [["opencrayast", "--help"], ["opencrayast", "-h"]] {
        let (code, _) = opencrayast::parse_args_from_check(&args);
        assert_eq!(code, EXIT_OK, "{args:?} should exit ok");
    }
    let (code, _) = opencrayast::parse_args_from_check(&["opencrayast", "--not-a-flag"]);
    assert_eq!(
        code, EXIT_USER,
        "an unknown flag is a user error; if this ever becomes EXIT_OK, clap errors are being \
         treated as answers"
    );
}

// ---------------------------------------------------------------------------
// (5b) A workspace failure surfaces its own code, not a flattened one.
// ---------------------------------------------------------------------------

fn text(cap: &Capture) -> String {
    cap.all().join("\n")
}

/// A command that reaches `workspace_or_report` and nothing else.
const PLAN_ARGS: [&str; 2] = ["plan", "list"];

/// Assert that an invocation over a workspace which cannot be resolved reports `expected`,
/// rather than a hardcoded environment error.
///
/// This is the regression fence for the flattening that CR R3 removed: `workspace_or_report` used
/// to print `[not_found]` and then return `ErrorCode::Env`, so the same condition announced itself
/// as one thing and exited as another. Both `NotFound` and `IoError` map to the environment bucket
/// (`EXIT_ENV`), which is precisely why a test that only checked the exit code could not tell the
/// flattening apart from correct behaviour — so the **code in the diagnostic** is what is asserted
/// here, not the number.
fn assert_workspace_failure_reports(root: &Path, config: &Path, what: &str) {
    let mut cap = Capture::default();
    let cli = Cli::try_parse_from([
        "opencrayast",
        "--workspace",
        root.to_str().unwrap(),
        "--config",
        config.to_str().unwrap(),
        PLAN_ARGS[0],
        PLAN_ARGS[1],
    ])
    .expect("arguments parse");
    let code = run_with(
        &cli,
        &mut cap,
        Palette::new(false),
        &mut opencrayast::confirm::NoOne::new(),
    );
    let t = text(&cap);

    // `not_found` is classified as a **user** error ("the request itself is wrong, or the thing it
    // names is not there"), so this exits 1. That is the interesting part: the flattening CR R3
    // removed was to an environment-classified code, which exits 2. So the exit code already
    // distinguishes the two here — and the diagnostic below says which condition occurred, which
    // is what a reader needs.
    assert_eq!(
        code, EXIT_USER,
        "{what}: `not_found` is the caller's mistake (exit 1); exit 2 would mean it was \
         flattened to an environment error:\n{t}"
    );
    assert!(
        t.contains(ErrorCode::NotFound.as_str()),
        "{what}: the failure must be reported with its OWN code, not a flattened one:\n{t}"
    );
    assert!(
        !t.contains(ErrorCode::IoError.as_str()),
        "{what}: a workspace failure was flattened, so the diagnostic names a condition that \
         did not occur:\n{t}"
    );
}

/// A workspace root that does not exist is reported as `not_found`, not as a generic environment
/// error.
///
/// The single most common real case — a `--workspace` typo, or a directory deleted between
/// planning and acting.
#[test]
fn a_missing_workspace_root_reports_not_found_not_a_flattened_env() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no-such-directory");
    assert!(!missing.exists(), "the fixture must not exist");

    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, "[policy]\nallow_write = true\n").unwrap();
    set_mode(&cfg, 0o600);

    assert_workspace_failure_reports(&missing, &cfg, "a missing workspace root");
}

/// A workspace root that exists but is a **file** is `invalid_args`, again its own code.
///
/// A second code, because one is not enough to prove the flattening is gone: if the implementation
/// mapped everything to `NotFound` this case would catch that too. `invalid_args` maps to the
/// *user* bucket, so it also pins the exit code as a side effect.
#[test]
fn a_workspace_root_that_is_a_file_reports_invalid_args() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("not-a-directory");
    std::fs::write(&file, b"just a file\n").unwrap();

    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, "[policy]\nallow_write = true\n").unwrap();
    set_mode(&cfg, 0o600);

    let mut cap = Capture::default();
    let cli = Cli::try_parse_from([
        "opencrayast",
        "--workspace",
        file.to_str().unwrap(),
        "--config",
        cfg.to_str().unwrap(),
        "plan",
        "list",
    ])
    .expect("arguments parse");
    let code = run_with(
        &cli,
        &mut cap,
        Palette::new(false),
        &mut opencrayast::confirm::NoOne::new(),
    );
    let t = text(&cap);

    assert_eq!(
        code, EXIT_USER,
        "a path that is not a directory is the caller's mistake, so it is a user error:\n{t}"
    );
    assert!(
        t.contains(ErrorCode::InvalidArgs.as_str()),
        "and it must report its own code:\n{t}"
    );
}

// ---------------------------------------------------------------------------
// (5a) The configuration is fail-closed: an unacceptable file is an error, never a default.
// ---------------------------------------------------------------------------

/// A configuration that cannot be trusted is an **error**, not a silent fallback to defaults.
///
/// `run_with` reads the operator's configuration once, before any command runs, and the `--config`
/// help promises "a file that exists but is unacceptable (malformed, world-writable, someone
/// else's) is an error, never a silent fallback to defaults". A fallback would be the more
/// dangerous outcome by far: the operator who wrote a file restricting writes would get a program
/// that silently ignored it, and every limit in it with it.
///
/// Each case below is driven through the real [`run_with`] and asserts the command did **not**
/// proceed — the diagnostic carries the error and the process exits on it. What would catch a
/// regression is replacing the error arm with `Ok(Settings::default())`: the exit code would
/// become the command's own, the diagnostic would carry the command's output instead of
/// `[invalid_args]`, and this test goes red.
#[test]
fn an_unacceptable_configuration_is_an_error_and_never_a_default() {
    // Each entry is (name, contents, mode). The mode matters as much as the text: a world-readable
    // configuration is refused for being world-readable even when its contents are perfect.
    //
    // Note there is no "empty file" case here. An empty configuration is **valid** — a document
    // with no keys is exactly `Settings::default()` — so it parses and the command proceeds. That
    // is not a silent fallback (nothing untrustworthy was ignored; there was no configuration to
    // distrust), and it is pinned in the other direction by
    // `an_absent_configuration_defaults_but_an_untrustworthy_one_does_not`.
    let cases: [(&str, &str, u32); 3] = [
        ("malformed", "this is not a configuration file\n", 0o600),
        ("truncated", "[policy\nallow_write = true\n", 0o600),
        ("world-readable", "[policy]\nallow_write = true\n", 0o644),
    ];

    for (name, contents, mode) in cases {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        std::fs::create_dir_all(&root).unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, contents).unwrap();
        set_mode(&cfg, mode);

        let mut cap = Capture::default();
        let cli = Cli::try_parse_from([
            "opencrayast",
            "--workspace",
            root.to_str().unwrap(),
            "--config",
            cfg.to_str().unwrap(),
            "plan",
            "list",
        ])
        .expect("arguments parse");
        let code = run_with(
            &cli,
            &mut cap,
            Palette::new(false),
            &mut opencrayast::confirm::NoOne::new(),
        );
        let t = text(&cap);

        assert_ne!(
            code, EXIT_OK,
            "a {name} configuration must not let the command proceed as if it were fine:\n{t}"
        );
        assert!(
            t.contains(ErrorCode::InvalidArgs.as_str())
                || t.contains(ErrorCode::ConfigUntrusted.as_str()),
            "a {name} configuration must be reported as a configuration refusal, not run \
             against defaults:\n{t}"
        );
        // The tell-tale of a silent fallback: the command's own output would appear instead.
        assert!(
            !t.contains("No plans") && !t.contains("plan list"),
            "a {name} configuration looks like it fell back to defaults and ran the command:\n{t}"
        );
    }
}

/// The two refusals are distinguishable, which is what "reported" has to mean.
///
/// `cli2_c10_configuration_is_reported_before_the_gate` already pins the *malformed* case at
/// exit one. This names the other half and asserts both in one place, so the taxonomy is
/// documented by a test rather than only by a comment: a file that does not parse is the
/// operator's text (`invalid_args`, exit one), and a file nobody can trust is the machine's
/// (`config_untrusted`, exit two). Flattening them together — the exact defect CR R3 describes
/// for the configuration surface — goes red here.
#[test]
fn malformed_exits_as_a_user_error_and_an_untrustworthy_file_as_an_environment_error() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ws");
    std::fs::create_dir_all(&root).unwrap();

    let exit_for = |contents: &str, mode: u32| -> (i32, String) {
        let cfg = dir.path().join("c.toml");
        std::fs::write(&cfg, contents).unwrap();
        set_mode(&cfg, mode);
        let mut cap = Capture::default();
        let cli = Cli::try_parse_from([
            "opencrayast",
            "--workspace",
            root.to_str().unwrap(),
            "--config",
            cfg.to_str().unwrap(),
            "plan",
            "list",
        ])
        .expect("arguments parse");
        let code = run_with(
            &cli,
            &mut cap,
            Palette::new(false),
            &mut opencrayast::confirm::NoOne::new(),
        );
        (code, text(&cap))
    };

    let (malformed_code, malformed_t) = exit_for("this is not a configuration file\n", 0o600);
    assert_eq!(
        malformed_code, EXIT_USER,
        "a file that does not parse is the operator's text:\n{malformed_t}"
    );
    assert!(
        malformed_t.contains(ErrorCode::InvalidArgs.as_str()),
        "{malformed_t}"
    );

    let (untrusted_code, untrusted_t) = exit_for("[policy]\nallow_write = true\n", 0o644);
    assert_eq!(
        untrusted_code, EXIT_ENV,
        "a file anyone else can read is the machine's problem:\n{untrusted_t}"
    );
    assert!(
        untrusted_t.contains(ErrorCode::ConfigUntrusted.as_str()),
        "{untrusted_t}"
    );
}

/// The two halves of fail-closed, stated as one property.
///
/// A configuration that does **not exist** is legitimately `Settings::default()` — there is
/// nothing to distrust — and a configuration that exists but cannot be trusted is an error. The
/// difference is existence, and it is the boundary the `Err` arm guards. This asserts both sides so
/// a "fix" that made every unreadable configuration fall back (the easy way to stop these
/// failures) would go red on the second half.
#[test]
fn an_absent_configuration_defaults_but_an_untrustworthy_one_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ws");
    std::fs::create_dir_all(&root).unwrap();

    // Absent: the command runs. There is nothing untrustworthy about a file that is not there.
    let absent = dir.path().join("no-such-config.toml");
    assert!(!absent.exists());
    let mut cap = Capture::default();
    let cli = Cli::try_parse_from([
        "opencrayast",
        "--workspace",
        root.to_str().unwrap(),
        "--config",
        absent.to_str().unwrap(),
        "plan",
        "list",
    ])
    .expect("arguments parse");
    let absent_code = run_with(
        &cli,
        &mut cap,
        Palette::new(false),
        &mut opencrayast::confirm::NoOne::new(),
    );
    assert_ne!(
        absent_code,
        EXIT_USER,
        "an absent configuration is not an error; refusing it would make the tool unusable \
         without one:\n{}",
        text(&cap)
    );

    // Present and untrustworthy: refused. `config_untrusted` is an environment-classified code,
    // so this exits 2 rather than the malformed file's 1 — see
    // `malformed_exits_as_a_user_error_and_an_untrustworthy_file_as_an_environment_error`.
    let world_readable = dir.path().join("world.toml");
    std::fs::write(&world_readable, "[policy]\nallow_write = true\n").unwrap();
    set_mode(&world_readable, 0o644);
    let mut cap = Capture::default();
    let cli = Cli::try_parse_from([
        "opencrayast",
        "--workspace",
        root.to_str().unwrap(),
        "--config",
        world_readable.to_str().unwrap(),
        "plan",
        "list",
    ])
    .expect("arguments parse");
    let untrusted_code = run_with(
        &cli,
        &mut cap,
        Palette::new(false),
        &mut opencrayast::confirm::NoOne::new(),
    );
    assert_ne!(
        untrusted_code,
        EXIT_OK,
        "but an untrustworthy one is a refusal:\n{}",
        text(&cap)
    );
}

/// A world-writable configuration is refused **because of its mode**, not its contents.
///
/// `Settings::load` refuses mode bits `0o177`, so group- and other-access and owner-execute all
/// fail. Asserting the code rather than the sentence keeps this alive if the message is reworded;
/// asserting that a world-readable file is refused *while a well-formed one with the same contents
/// is not* is what proves the mode is the deciding factor.
#[test]
fn the_mode_decides_not_the_contents() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ws");
    std::fs::create_dir_all(&root).unwrap();
    let contents = "[policy]\nallow_write = true\n";

    let refused = dir.path().join("refused.toml");
    std::fs::write(&refused, contents).unwrap();
    set_mode(&refused, 0o644);

    let accepted = dir.path().join("accepted.toml");
    std::fs::write(&accepted, contents).unwrap();
    set_mode(&accepted, 0o600);

    let run_one = |cfg: &Path| -> (i32, String) {
        let mut cap = Capture::default();
        let cli = Cli::try_parse_from([
            "opencrayast",
            "--workspace",
            root.to_str().unwrap(),
            "--config",
            cfg.to_str().unwrap(),
            "plan",
            "list",
        ])
        .expect("arguments parse");
        let code = run_with(
            &cli,
            &mut cap,
            Palette::new(false),
            &mut opencrayast::confirm::NoOne::new(),
        );
        (code, text(&cap))
    };

    let (refused_code, refused_t) = run_one(&refused);
    let (accepted_code, accepted_t) = run_one(&accepted);

    assert_ne!(
        refused_code, EXIT_OK,
        "byte-identical contents, but 0644 must be refused:\n{refused_t}"
    );
    assert!(
        refused_t.contains(ErrorCode::ConfigUntrusted.as_str()),
        "and refused for its mode, not its text:\n{refused_t}"
    );
    assert_eq!(
        accepted_code, EXIT_OK,
        "the same contents at 0600 must be accepted, or the mode is not what is being checked:\n\
         {accepted_t}"
    );
}

// ---- helpers ---------------------------------------------------------------------------

/// Set the exact mode of a file, so the result does not depend on the process umask.
fn set_mode(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
}
