//! CFGSRC-CLI-xx: `doctor` says which configuration file is in force.
//!
//! The MCP half of this ruling lives in `crates/mcp/tests/config_source_spec.rs` (`ast_info`).
//! This file covers the human surface, and it covers the part the MCP half cannot reach at all:
//! **the warning when the file in force is inside the workspace**.
//!
//! That warning is the deliverable. `--config` pointing into the workspace is **allowed** (the
//! ruling), so nothing may refuse it — but the operator should learn it from the tool they already
//! run when something looks wrong. A `warn`, never a `fail`: a `warn` still exits 0, and that
//! distinction is asserted here so a later "helpful" tightening cannot pass unnoticed.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use clap::Parser;
use opencrayast::Cli;
use opencrayast::exit::{EXIT_OK, EXIT_USER};
use opencrayast::out::Capture;
use std::path::{Path, PathBuf};

struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl World {
    fn new() -> World {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("hello.rs"), "fn main() {}\n").unwrap();
        World { _dir: dir, root }
    }

    /// A 0600 configuration file, at the caller's chosen location.
    fn config_at(&self, rel: &str, body: &str) -> PathBuf {
        let p = self._dir.path().join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        p
    }
}

fn drive(w: &World, config: Option<&Path>, args: &[&str]) -> (i32, String) {
    let mut full: Vec<String> = vec![
        "opencrayast".into(),
        "--workspace".into(),
        w.root.to_string_lossy().into_owned(),
    ];
    if let Some(c) = config {
        full.push("--config".into());
        full.push(c.to_string_lossy().into_owned());
    }
    full.extend(args.iter().map(|a| (*a).to_string()));
    let cli = Cli::try_parse_from(&full).unwrap_or_else(|e| panic!("{e}"));
    let state = w._dir.path().join("state");
    let mut cap = Capture::default();
    let code = opencrayast::run_with_state(
        &cli,
        &mut cap,
        opencrayast::palette::Palette::new(false),
        &mut opencrayast::confirm::Stdin::new(),
        &opencrayast::StateDir::Fixed(&state),
    );
    (code, cap.all().join("\n"))
}

/// CFGSRC-CLI-01: `doctor` always prints a `config` line naming the file in force.
///
/// Three cases, three different answers, all on one line — the point being that they are
/// *distinguishable*. "A configuration was loaded" is not an answer anyone can act on.
#[test]
fn cfgsrc_cli01_doctor_always_names_the_config_in_force() {
    let w = World::new();

    // (a) a file outside the workspace, named by --config: fine, and named.
    let outside = w.config_at("user/opencrayast.toml", "[limits]\npath_max_bytes = 512\n");
    let (code, t) = drive(&w, Some(&outside), &["doctor"]);
    assert_eq!(code, EXIT_OK, "{t}");
    let line = t
        .lines()
        .find(|l| l.contains("config"))
        .unwrap_or_else(|| panic!("doctor must print a config line:\n{t}"));
    assert!(
        line.contains("opencrayast.toml"),
        "the file must be named: {line}"
    );
    assert!(
        !line.contains("INSIDE"),
        "a file outside the workspace must not be reported as inside it: {line}"
    );

    // (b) no --config at all: the defaults, said plainly.
    let (code, t) = drive(&w, None, &["doctor"]);
    assert_eq!(code, EXIT_OK, "{t}");
    assert!(
        t.lines().any(|l| l.contains("defaults")),
        "with no configuration file doctor must say the defaults are in force:\n{t}"
    );
}

/// CFGSRC-CLI-02: a config **inside** the workspace is a `warn`, not a `fail`.
///
/// This is the assertion with teeth on both sides. It must warn, because a repository can ship
/// that file and the operator would otherwise have no way to learn their policy came from it. And
/// it must NOT fail, because the ruling allows the path — turning the warning into a refusal would
/// be a silent policy change made by an implementation.
#[test]
fn cfgsrc_cli02_a_config_inside_the_workspace_warns_and_still_exits_zero() {
    let w = World::new();
    // Inside the workspace: exactly where a repository would put one.
    let inside = w.config_at("ws/opencrayast.toml", "[policy]\nallow_write = true\n");

    let (code, t) = drive(&w, Some(&inside), &["doctor"]);
    let line = t
        .lines()
        .find(|l| l.contains("INSIDE"))
        .unwrap_or_else(|| panic!("a config inside the workspace must be called out:\n{t}"));
    assert!(
        line.starts_with("warn"),
        "it must be a warning, not a failure: {line}"
    );
    assert!(
        line.contains("opencrayast.toml"),
        "the warning must name the file: {line}"
    );
    assert!(
        line.to_lowercase().contains("workspace"),
        "the warning must say why it matters: {line}"
    );
    assert_eq!(
        code, EXIT_OK,
        "a warning alone must still exit 0 — the ruling allows this path:\n{t}"
    );
    // And nothing anywhere in the output claims a failure.
    assert!(
        !t.lines().any(|l| l.starts_with("fail ")),
        "nothing may fail here:\n{t}"
    );
}

/// CFGSRC-CLI-03: the warning does not change what is actually in force.
///
/// The warning is a diagnostic; it must not become a second policy engine. This session had
/// `allow_write = true` in a workspace config and still ran read-only, because `--write` was not
/// passed — the capability gate is unchanged by anything this ruling added.
#[test]
fn cfgsrc_cli03_the_warning_does_not_change_the_effective_policy() {
    let w = World::new();
    let inside = w.config_at("ws/opencrayast.toml", "[policy]\nallow_write = true\n");
    let (_code, t) = drive(&w, Some(&inside), &["doctor"]);
    // `doctor`'s own write-mode line is the observable: without --write it must say writing is off.
    let write_line = t
        .lines()
        .find(|l| l.contains("write mode"))
        .unwrap_or_else(|| panic!("doctor must report write mode:\n{t}"));
    assert!(
        !write_line.starts_with("ok  write mode"),
        "write mode must stay off without --write, whatever the config file says: {write_line}"
    );
}

/// CFGSRC-CLI-04: a malformed config still fails, and `doctor` says so.
///
/// The reporting added here must not soften the existing refusal: a file that exists and is
/// unacceptable is an error, and `doctor` must not fall back to defaults and print a cheerful
/// `config: defaults` line.
///
/// The exit code is `EXIT_USER`, not `EXIT_ENV`: the documented mapping
/// (`docs/CONFIGURATION.md` §Exit codes, `exit::exit_code_for_error`) classifies **malformed** as
/// the user's to fix and reserves the environment class for a file that cannot be *trusted*
/// (wrong owner, wrong permissions). Asserting the exact code here is what keeps a future change
/// from quietly reclassifying one as the other.
#[test]
fn cfgsrc_cli04_a_malformed_config_is_still_refused() {
    let w = World::new();
    let bad = w.config_at("ws/broken.toml", "[limits\npath_max_bytes = 512\n");
    let (code, t) = drive(&w, Some(&bad), &["doctor"]);
    assert_eq!(code, EXIT_USER, "a malformed config must not exit 0:\n{t}");
    assert!(
        t.contains("configuration"),
        "the refusal must name configuration as the cause:\n{t}"
    );
    assert!(
        !t.contains("config: defaults"),
        "a refused configuration must never be reported as the defaults:\n{t}"
    );
}
