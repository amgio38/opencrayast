//! The two shells must agree about WHAT KIND of failure a configuration file is, even though
//! they print different digits for it.
//!
//! The defect this pins: `opencrayast` returned a hardcoded `exit::EXIT_ENV` (2) for every
//! configuration refusal, and `opencrayast-mcp` collapsed the same refusals into
//! `ExitCode::FAILURE` (1). Every case diverged, and neither shell could say why, because the
//! CLI was inventing a number instead of asking the mapping it documents in `--help`, and the
//! MCP server had no documented exit codes at all to be consistent with.
//!
//! So this asserts the property that actually matters and cannot drift: **the same file, read
//! by both binaries, is classified the same way.** A malformed file is the operator's to fix;
//! a file nobody can trust is the machine's. The digits are each shell's own business and are
//! documented on each surface; the classification is shared, in `ErrorCode::exit_class`.
//!
//! Both binaries are driven here, so this test would have caught the original divergence — and
//! it is in the MCP crate because the layering rules do not let the CLI crate reach the server.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The class a configuration refusal falls into, as both shells agreed on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// The operator's file is wrong: malformed, or names a key/section that does not exist.
    User,
    /// The machine cannot be trusted with this file: wrong owner, wrong permissions, not a file.
    Environment,
    /// The server started, which means the file was acceptable.
    Accepted,
}

/// Find a built binary of the workspace by name.
fn bin(name: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    // CARGO_TARGET_DIR comes first because the coverage gate builds into a private one, and
    // `cargo llvm-cov` puts its own `llvm-cov-target` level under whatever it is told — so both
    // spellings are tried, since the gate and a plain `cargo test` disagree about which one
    // they set.
    let mut target_dirs: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::var_os("CARGO_TARGET_DIR") {
        let dir = PathBuf::from(dir);
        target_dirs.push(dir.join("llvm-cov-target"));
        target_dirs.push(dir);
    }
    target_dirs.push(root.join("target"));
    for target in &target_dirs {
        for profile in ["debug", "release"] {
            let p = target.join(profile).join(name);
            if p.exists() {
                return p;
            }
        }
    }
    panic!("{name} not found under target/; build the workspace first");
}

/// A workspace and a configuration file in it, with a mode the caller chooses.
struct Case {
    _dir: tempfile::TempDir,
    ws: PathBuf,
    config: PathBuf,
}

impl Case {
    fn new(body: &str, mode: u32) -> Case {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        let config = dir.path().join("config.toml");
        fs::write(&config, body).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(mode)).unwrap();
        Case {
            _dir: dir,
            ws,
            config,
        }
    }

    /// Run the CLI against this fixture.
    ///
    /// `current_dir` is set explicitly because `doctor` checks that the working directory is
    /// writable, and inheriting whatever directory the test runner happened to start in made
    /// this fail intermittently (2 runs in 25) for reasons that had nothing to do with the
    /// configuration file under test. The workspace is the thing the test owns, so it is what
    /// the binary is pointed at.
    fn cli(&self, args: &[&str]) -> i32 {
        self.cli_out(args).status.code().unwrap_or(-1)
    }

    fn cli_out(&self, args: &[&str]) -> std::process::Output {
        Command::new(bin("opencrayast"))
            .arg("doctor")
            .args(args)
            .arg("--config")
            .arg(&self.config)
            .current_dir(&self.ws)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn mcp(&self) -> i32 {
        let out = Command::new(bin("opencrayast-mcp"))
            .arg("--workspace")
            .arg(&self.ws)
            .arg("--config")
            .arg(&self.config)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        out.status.code().unwrap_or(-1)
    }

    /// Run both shells and assert they classified this file the same way.
    ///
    /// This is the assertion that matters. The digits are asserted per shell below, because a
    /// wrapper script depends on them, but they are separate assertions on purpose: a change to
    /// either shell's table must not be able to pass by moving both numbers together.
    #[track_caller]
    fn assert_same_class(&self, expected: Class, what: &str) {
        let (cli, mcp) = (self.cli(&[]), self.mcp());
        let actual = |code: i32| match code {
            0 => Class::Accepted,
            // The CLI's documented table.
            1 => Class::User,
            2 => Class::Environment,
            // The MCP server's documented table.
            3 => Class::User,
            4 => Class::Environment,
            other => panic!("{what}: undocumented exit status {other} from a shell"),
        };
        assert_eq!(
            actual(cli),
            expected,
            "{what}: the CLI classified it differently (exit {cli})"
        );
        assert_eq!(
            actual(mcp),
            expected,
            "{what}: the MCP server classified it differently (exit {mcp})"
        );
        assert_eq!(
            actual(cli),
            actual(mcp),
            "{what}: the two shells disagree (cli {cli}, mcp {mcp}) — this is the whole point of \
             this test"
        );
    }
}

/// PARITY-EXIT-01 — a MALFORMED configuration is a user error on both shells.
///
/// A typo, a truncated file, a bad value, an unknown section and a duplicate key are all
/// things the operator edits and retries. Neither shell may report them as an environment
/// problem, and the CLI must not flatten all of them to one number.
#[test]
fn parity_exit_01_a_malformed_config_is_a_user_error_on_both_shells() {
    let cases: [(&str, &str); 6] = [
        ("a typo in a limit", "[limits]\npath_max_dept = 4\n"),
        ("a non-numeric limit", "[limits]\nmax_results = many\n"),
        ("a zero limit", "[limits]\npath_max_depth = 0\n"),
        ("an unknown section", "[nonsense]\nx = 1\n"),
        ("a truncated file", "[limits]\npath_max_depth"),
        (
            "a duplicated key",
            "[policy]\nallow_write = false\nallow_write = true\n",
        ),
    ];
    for (what, body) in cases {
        let c = Case::new(body, 0o600);
        c.assert_same_class(Class::User, what);
        assert_eq!(
            c.cli(&[]),
            1,
            "{what}: the CLI's documented user-error status"
        );
        assert_eq!(
            c.mcp(),
            3,
            "{what}: the MCP server's documented user-error status"
        );
    }
}

/// PARITY-EXIT-02 — an UNTRUSTWORTHY configuration is an environment error on both shells.
///
/// A world-readable file is a way for another local user to turn a limit off, so it is refused.
/// That is a fact about the machine, not about the operator's typing, and it is `chmod` that
/// fixes it — which is why it is 2 and 4 rather than 1 and 3.
///
/// Owner-execute is here too, and not only as a class check: it is the mode defect, and this
/// row fails if the mask is loosened again.
#[test]
fn parity_exit_02_an_untrusted_config_is_an_environment_error_on_both_shells() {
    for mode in [0o644u32, 0o666, 0o777, 0o700] {
        let c = Case::new("[limits]\nmax_results = 5\n", mode);
        let what = format!("mode {mode:04o}");
        c.assert_same_class(Class::Environment, &what);
        assert_eq!(
            c.cli(&[]),
            2,
            "{what}: the CLI's documented environment status"
        );
        assert_eq!(
            c.mcp(),
            4,
            "{what}: the MCP server's documented environment status"
        );
    }
}

/// PARITY-EXIT-03 — the acceptable modes stay acceptable on both shells.
///
/// The control for the row above. A "fix" that refused every configuration file would satisfy
/// both classifications, and an operator with a perfectly good file could not start anything.
#[test]
fn parity_exit_03_the_documented_modes_are_accepted_on_both_shells() {
    for mode in [0o600u32, 0o400] {
        let c = Case::new("[limits]\nmax_results = 5\n", mode);
        c.assert_same_class(Class::Accepted, &format!("mode {mode:04o}"));
        assert_eq!(c.cli(&[]), 0, "mode {mode:04o}");
        assert_eq!(c.mcp(), 0, "mode {mode:04o}");
    }
}

/// PARITY-EXIT-04 — no file at all is success on both shells, and still exit 0.
///
/// A user who has never written a configuration file has not misconfigured anything. This is
/// the case where the two shells already agreed, and it is pinned so that making the refusals
/// precise does not make absence into an error.
#[test]
fn parity_exit_04_a_missing_config_is_success_on_both_shells() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    fs::create_dir_all(&ws).unwrap();
    let missing = dir.path().join("absent.toml");

    let cli = Command::new(bin("opencrayast"))
        .arg("doctor")
        .arg("--config")
        .arg(&missing)
        .current_dir(&ws)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let cli_stderr = String::from_utf8_lossy(&cli.stderr);
    assert!(
        cli.status.success(),
        "a file that does not exist is not a refusal; stderr: {cli_stderr}; stdout: {}",
        String::from_utf8_lossy(&cli.stdout)
    );

    let mcp = Command::new(bin("opencrayast-mcp"))
        .arg("--workspace")
        .arg(&ws)
        .arg("--config")
        .arg(&missing)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let mcp_stderr = String::from_utf8_lossy(&mcp.stderr);
    assert!(
        mcp.status.success(),
        "a file that does not exist is not a refusal; stderr: {mcp_stderr}"
    );
}

/// PARITY-EXIT-05 — a configuration key cannot put a terminal sequence on either surface.
///
/// The defect: the CLI was already safe here, because `Out::diag` escapes before it writes,
/// but the MCP server printed the same message raw through `eprintln!`, so a key spelled
/// `max_ESC[31mRED_ESC[0mults` reached a terminal with a live SGR sequence. Both surfaces are
/// asserted here because the point is that no shell is the odd one out, and a future third
/// shell reading the same `Settings` should not have to rediscover this.
#[test]
fn parity_exit_05_no_shell_prints_a_raw_escape_from_a_config_file() {
    let c = Case::new("[limits]\nmax_\u{1b}[31mRED\u{1b}[0mults = 5\n", 0o600);

    for (name, code, err) in [
        ("opencrayast", c.cli(&[]), 1),
        ("opencrayast-mcp", c.mcp(), 3),
    ] {
        // Re-run capturing stderr: `cli`/`mcp` above only took the status.
        let out = if name == "opencrayast" {
            c.cli_out(&[])
        } else {
            Command::new(bin("opencrayast-mcp"))
                .arg("--workspace")
                .arg(&c.ws)
                .arg("--config")
                .arg(&c.config)
                .stdin(Stdio::null())
                .output()
                .unwrap()
        };
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(code, err, "{name}: expected the user-error status");
        assert!(
            !stderr.contains('\u{1b}'),
            "{name} wrote a raw ESC to stderr: {stderr:?}"
        );
        assert!(
            !stderr.chars().any(|ch| ch.is_control() && ch != '\n'),
            "{name} wrote a raw control character to stderr: {stderr:?}"
        );
        assert!(
            stderr.contains("\\u{1b}"),
            "{name} should show the escape as visible text instead: {stderr:?}"
        );
        // stdout is the protocol wire for the server and ordinary output for the CLI; a
        // refusal must not appear there at all.
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            !stdout.contains('\u{1b}'),
            "{name} wrote a raw ESC to stdout: {stdout:?}"
        );
    }
}
