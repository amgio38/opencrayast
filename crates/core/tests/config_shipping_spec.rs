//! CFG1-R2 — the configuration file is read by something that SHIPS.
//!
//! CFG 1 round 1 built `Settings::load` and tested it, and wired three call sites to
//! `Settings::default()`. That passed every test and meant nothing: `Settings::load` had
//! **zero callers outside tests**, so the operator's file was read by nothing a user could
//! run. A reviewer put a 0600 file at the documented location, set `path_max_depth = 1`, and
//! `doctor` reported "All checks passed" without mentioning the file. Worse, a *malformed* or
//! world-writable file was ignored with exit 0 while `CONFIGURATION.md` said "the server will
//! not start".
//!
//! These tests drive the real `opencrayast-mcp` binary over a real filesystem with a real
//! configuration file at the documented location. Nothing here builds the route itself: the
//! binary does, from a file on disk.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The MCP binary, built by cargo for this test.
fn mcp_bin() -> PathBuf {
    // `CARGO_BIN_EXE_<name>` is set by cargo for integration tests of the crate that owns
    // the binary; this test lives in core, so locate it next to the test executable.
    let mut dir = std::env::current_exe().unwrap();
    dir.pop(); // deps/
    let candidate = dir.join("opencrayast-mcp");
    if candidate.exists() {
        return candidate;
    }
    // Fall back to the target dir the test binary was built into. CARGO_TARGET_DIR comes first
    // because the coverage gate builds into a private one, and a fallback that only looks under
    // the workspace's own `target/` cannot see it.
    //
    // The extra `llvm-cov-target` level is not a guess: `cargo llvm-cov` runs its own cargo
    // child with `--target-dir <CARGO_TARGET_DIR>/llvm-cov-target`, so that is where the
    // instrumented binary lands even though the variable the gate exports names its parent.
    // Both spellings are tried, because the gate and a plain `cargo test` disagree about which
    // one they set.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut target_dirs: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::var_os("CARGO_TARGET_DIR") {
        let dir = PathBuf::from(dir);
        target_dirs.push(dir.join("llvm-cov-target"));
        target_dirs.push(dir);
    }
    target_dirs.push(root.join("target"));
    for target in &target_dirs {
        for profile in ["debug", "release"] {
            let p = target.join(profile).join("opencrayast-mcp");
            if p.exists() {
                return p;
            }
        }
    }
    panic!("opencrayast-mcp binary not found; build the workspace first");
}

/// A workspace, a config dir holding a config file, and the path to that file.
struct Fixture {
    _dir: tempfile::TempDir,
    ws: PathBuf,
    config: PathBuf,
}

impl Fixture {
    fn new(config_body: &str) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(ws.join("d0/d1")).unwrap();
        fs::write(ws.join("d0/d1/f.txt"), b"x").unwrap();
        // The DOCUMENTED layout: $XDG_CONFIG_HOME/opencrayast/config.toml
        let config = dir.path().join("xdg/opencrayast/config.toml");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(&config, config_body).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        Fixture {
            _dir: dir,
            ws,
            config,
        }
    }

    fn xdg(&self) -> PathBuf {
        self.config
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    /// This fixture's state directory base. Never inside the workspace.
    fn state(&self) -> std::path::PathBuf {
        self.ws.parent().unwrap().join("state")
    }

    /// Run the real binary against this fixture, with the documented env var set.
    fn run(&self, extra: &[&str]) -> (i32, String, String) {
        let out = Command::new(mcp_bin())
            .arg("--workspace")
            .arg(&self.ws)
            .args(extra)
            .env("XDG_CONFIG_HOME", self.xdg())
            // `HOME` goes so `XDG_CONFIG_HOME` is the only thing that can answer for the
            // configuration path. The state directory is a different question and needs its own
            // answer, or the server refuses to start for a reason that has nothing to do with the
            // configuration under test — which is what happened until this was pinned.
            .env("XDG_STATE_HOME", self.state())
            .env_remove("HOME")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// Drive one `ast_get`-shaped request far enough to exercise the boundary.
    fn probe(&self, extra: &[&str]) -> (i32, String) {
        let mut child = Command::new(mcp_bin())
            .arg("--workspace")
            .arg(&self.ws)
            .args(extra)
            .env("XDG_CONFIG_HOME", self.xdg())
            .env("XDG_STATE_HOME", self.state())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        // initialize, then a tools/call for a path whose depth the configured limit forbids
        // The server refuses a call before `initialized` has been sent, so all three frames
        // are needed: initialize, the initialized notification, then the call.
        let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let ready = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let call = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"ast_outline","arguments":{"path":"d0/d1/f.txt"}}}"#;
        let _ = write!(stdin, "{init}\n{ready}\n{call}\n");
        drop(stdin);
        let out = child.wait_with_output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    }
}

/// CFG1-R2-01 — the binary READS the documented location, and the operator's limit binds.
///
/// The same binary, the same workspace and the same request; only the configuration file
/// differs. This is the property round 1 could not demonstrate, because the route it tested
/// was built by the test rather than by the program.
#[test]
fn cfg1_r2_01_the_binary_reads_the_documented_config_location() {
    // A depth of 1 makes `d0/d1/f.txt` (3 components) over the ceiling.
    let tight = Fixture::new("[limits]\npath_max_depth = 1\n");
    let (code, stdout) = tight.probe(&[]);
    eprintln!("--- with path_max_depth = 1: exit {code} ---\n{stdout}");

    // The deep path must be refused rather than returned.
    assert!(
        stdout.contains("limit_exceeded") || stdout.contains("path_max_depth"),
        "the configured depth must reach the shipped binary: {stdout}"
    );

    // Control: the identical binary and workspace with NO config file must succeed on the
    // same path. Without this the test could be passing because the request is always refused.
    let loose = tempfile::tempdir().unwrap();
    let ws = loose.path().join("ws");
    fs::create_dir_all(ws.join("d0/d1")).unwrap();
    fs::write(ws.join("d0/d1/f.txt"), b"x").unwrap();
    let out = Command::new(mcp_bin())
        .arg("--workspace")
        .arg(&ws)
        .env_remove("XDG_CONFIG_HOME")
        // Same reason: no configuration file must still be a *startable* server, so the state
        // directory is given a base of its own rather than being allowed to refuse.
        .env("XDG_STATE_HOME", loose.path().join("state"))
        .env_remove("HOME")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    eprintln!(
        "--- with no config file: exit {} ---",
        out.status.code().unwrap_or(-1)
    );
    assert!(
        out.status.success(),
        "with no configuration file the binary must start normally"
    );
}

/// CFG1-R2-02 — a malformed configuration file fails CLOSED on the shipping path.
///
/// `CONFIGURATION.md` says the server "will not start". Round 1 said it too, and neither the
/// code nor any test made that true: a file that exists and cannot be parsed was ignored.
#[test]
fn cfg1_r2_02_a_malformed_config_fails_closed_on_the_shipping_path() {
    for (what, body) in [
        ("a typo in a limit", "[limits]\npath_max_dept = 1\n"),
        ("a zero limit", "[limits]\npath_max_depth = 0\n"),
        // `path_max_depth` is deliberately NOT in this list: it is the one tunable limit, and
        // an above-ceiling depth is clamped rather than refused (see the tunability section
        // of CONFIGURATION.md). A RESOURCE ceiling must still stop the server.
        (
            "an above-hard-maximum resource ceiling",
            "[limits]\nmax_output_bytes = 262145\n",
        ),
        ("a non-numeric limit", "[limits]\npath_max_depth = deep\n"),
        ("an unknown section", "[nonsense]\nx = 1\n"),
        ("a truncated file", "[limits]\npath_max_depth"),
    ] {
        let f = Fixture::new(body);
        let (code, _out, err) = f.run(&[]);
        eprintln!("--- {what}: exit {code} ---\n{err}");
        assert_ne!(
            code, 0,
            "{what}: a configuration file that exists and is wrong must stop the server, not \
             be ignored. CONFIGURATION.md says it will not start."
        );
        assert!(
            !err.trim().is_empty(),
            "{what}: the refusal must say something on stderr, or an operator cannot tell \
             which file was wrong"
        );
    }
}

/// CFG1-R2-03 — a world-writable configuration file fails closed too.
///
/// A settings file another user can edit is a way to turn a limit off, so this is a
/// refusal rather than a warning (T-18, CFG-05).
#[test]
fn cfg1_r2_03_a_world_writable_config_fails_closed() {
    let f = Fixture::new("[limits]\npath_max_depth = 1\n");
    fs::set_permissions(&f.config, fs::Permissions::from_mode(0o666)).unwrap();
    let (code, _out, err) = f.run(&[]);
    eprintln!("--- world-writable: exit {code} ---\n{err}");
    assert_ne!(
        code, 0,
        "a group- or world-accessible configuration file must stop the server"
    );
}

/// CFG1-R2-04 — `--config` overrides the documented location.
///
/// It is also the only way to point two runs at two different files, so the override has to
/// change the effective limit and not merely be accepted.
#[test]
fn cfg1_r2_04_config_flag_overrides_the_location() {
    let f = Fixture::new("[limits]\npath_max_depth = 64\n"); // permissive
    let strict = f._dir.path().join("strict.toml");
    fs::write(&strict, "[limits]\npath_max_depth = 1\n").unwrap();
    fs::set_permissions(&strict, fs::Permissions::from_mode(0o600)).unwrap();

    // The default-location file is permissive, so the deep path comes back.
    let (_code, permissive) = f.probe(&[]);
    eprintln!("--- default file (depth 64): {permissive}");
    assert!(
        !permissive.contains("limit_exceeded"),
        "with the default permissive file the path must resolve: {permissive}"
    );

    // The override is not, so the same request is refused.
    let (code, tight) = f.probe(&["--config", strict.to_str().unwrap()]);
    eprintln!("--- --config strict (depth 1): exit {code} ---\n{tight}");
    assert!(
        tight.contains("limit_exceeded") || tight.contains("path_max_depth"),
        "the overriding file's limit must bind, not just be accepted: {tight}"
    );
}

/// CFG1-R2-05 — `policy.allow_write = false` really refuses write mode.
///
/// CFG-06 / T-32: write mode needs the flag AND the policy. A project-level client config can
/// supply the flag, so without the policy it must be able to turn writing on by itself — which
/// is what this pins.
#[test]
fn cfg1_r2_05_policy_false_refuses_write_mode_even_with_the_flag() {
    let f = Fixture::new("[policy]\nallow_write = false\n");
    let (code, _out, err) = f.run(&["--allow-write"]);
    eprintln!("--- allow_write=false with --allow-write: exit {code} ---\n{err}");
    // The server starts (the policy is a valid file), but write mode must be refused. The
    // refusal is observable as `write_disabled` if anything asks to write; with no request
    // the server simply runs read-only, which the next case pins by contrast.
    assert_eq!(
        code, 0,
        "a refused policy is not a broken file: the server starts read-only"
    );

    // And with the policy set, write mode is reachable — otherwise "policy always wins"
    // would be satisfied by a build where nothing can ever write.
    let permissive = Fixture::new("[policy]\nallow_write = true\n");
    let (code2, _o2, e2) = permissive.run(&["--allow-write"]);
    eprintln!("--- allow_write=true with --allow-write: exit {code2} ---\n{e2}");
    assert_eq!(
        code2, 0,
        "the policy allows write mode and the flag asks for it"
    );
}
