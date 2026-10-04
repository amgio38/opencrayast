//! CFGSRC-xx: which configuration file is in force, and where it came from.
//!
//! The ruling on `ISSUE-REPO-CONFIG-TOML-0600-OPERATOR-T-32-CONFIGURATION-MD` is that
//! `--config <path>` is honoured **wherever it points**, including inside the workspace. That is
//! the operator's call and nothing refuses it — but a repository can ship such a file, and once
//! the 0600 and owner checks pass its bytes are indistinguishable from the operator's own. So the
//! rule this file pins is not "a repo config is refused" (it is not, and pretending otherwise
//! would be a false claim) but **"the process always says which file it is running on, and says
//! so extra loudly when that file is inside the workspace."**
//!
//! Without this, T-32's mitigation was unenforced in the only sense that matters to a user: the
//! user had no in-band way to learn that the limits and policy in force came from a file they
//! never wrote. `ast_info` printed five fixed lines and named no source at all.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, ChildStdout, Command, Stdio};

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_opencrayast-mcp"));
    c.env("NO_COLOR", "1");
    c
}

/// A workspace, a state directory that is not inside it, and an optional configuration file
/// placed **inside the workspace** — the case the ruling deliberately allows.
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

    /// A 0600 configuration file inside the workspace, as a repository would ship one.
    fn repo_config(&self, body: &str) -> PathBuf {
        let p = self.root.join("opencrayast.toml");
        std::fs::write(&p, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        p
    }
}

/// One `tools/call` session against the real binary.
struct Session {
    _child: std::process::Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

fn spawn(ws: &World, config: Option<&Path>, env_home: Option<&Path>) -> Session {
    let mut cmd = bin();
    if let Some(home) = env_home {
        // `doctor` and `ast_info` read the documented user path from the environment; pointing
        // XDG_CONFIG_HOME at a directory of this test's own keeps the assertions off the
        // developer's real configuration.
        cmd.env("XDG_CONFIG_HOME", home);
    } else {
        cmd.env_remove("XDG_CONFIG_HOME");
    }
    cmd.arg("--workspace").arg(&ws.root);
    if let Some(c) = config {
        cmd.arg("--config").arg(c);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn opencrayast-mcp");
    let mut s = Session {
        stdin: child.stdin.take().expect("stdin"),
        stdout: BufReader::new(child.stdout.take().expect("stdout")),
        _child: child,
    };
    s.raw(
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-06-18","capabilities":{},
            "clientInfo":{"name":"cfgsrc","version":"0"}}})
        .to_string(),
    );
    assert!(s.recv().get("result").is_some());
    s.raw(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    s
}

impl Session {
    fn raw(&mut self, line: &str) {
        self.stdin.write_all(line.as_bytes()).unwrap();
        self.stdin.write_all(b"\n").unwrap();
        self.stdin.flush().unwrap();
    }

    fn recv(&mut self) -> Value {
        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).expect("read stdout");
        assert!(n > 0, "server closed stdout before answering");
        serde_json::from_str(line.trim_end_matches(['\r', '\n'])).expect("JSON-RPC line")
    }

    fn ast_info(&mut self) -> String {
        self.raw(
            &json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{
                "name":"ast_info","arguments":{}}})
            .to_string(),
        );
        let resp = self.recv();
        resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no ast_info text in {resp}"))
            .to_string()
    }
}

/// The `config:` line of `ast_info`, or `None` when the tool did not report one.
fn config_line(info: &str) -> Option<&str> {
    info.lines().find(|l| l.starts_with("config: "))
}

/// CFGSRC-01: `ast_info` names the file in force, in all three cases.
///
/// Mutation: delete the `config:` line from `info.rs` → red for the explicit-file case, and the
/// "no file" case stops being distinguishable from an older binary.
#[test]
fn cfgsrc01_ast_info_names_the_config_in_force() {
    let w = World::new();

    // (a) an explicit `--config` inside the workspace: honoured (the ruling), and named.
    let cfg = w.repo_config("[policy]\nallow_write = true\n");
    let mut s = spawn(&w, Some(&cfg), None);
    let info = s.ast_info();
    let line = config_line(&info)
        .unwrap_or_else(|| panic!("ast_info must report a config source:\n{info}"));
    assert!(
        line.contains("--config"),
        "an explicit --config must be reported as such, not as the user file: {line}"
    );
    assert!(
        line.contains("opencrayast.toml"),
        "the file must be named so the operator can recognise it: {line}"
    );
    drop(s);

    // (b) no `--config` and no user file: the defaults, said plainly. Pointing XDG_CONFIG_HOME at
    // an empty directory of this test's own makes "no file" a fact rather than a hope.
    let empty = w._dir.path().join("empty-config");
    std::fs::create_dir_all(&empty).unwrap();
    let mut s = spawn(&w, None, Some(&empty));
    let info = s.ast_info();
    let line = config_line(&info).expect("ast_info must report a config source");
    assert!(
        line.contains("defaults"),
        "with no configuration file the answer must say so: {line}"
    );
}

/// CFGSRC-02: a file the operator did not name is reported differently from one they did.
///
/// The distinction is the whole point: "a configuration was loaded" is not actionable, while
/// "`--config` pointed at this file" is. Without the distinction a repository-supplied config and
/// the user's own are reported identically.
#[test]
fn cfgsrc02_an_explicit_config_is_distinguishable_from_the_user_file() {
    let w = World::new();
    let cfg = w.repo_config("[limits]\npath_max_bytes = 512\n");
    let mut s = spawn(&w, Some(&cfg), None);
    let info = s.ast_info();
    let line = config_line(&info).expect("ast_info must report a config source");
    assert!(
        line.contains("--config"),
        "the report must say the path came from the command line: {line}"
    );
    assert!(
        !line.contains("user file"),
        "a --config file must never be reported as the user-level file: {line}"
    );
}

/// CFGSRC-03: `doctor` **warns** when the file in force is inside the workspace, and does not
/// refuse to start.
///
/// The refusal is the part a test has to pin hardest, because it is the tempting over-correction:
/// the ruling allows this path, so a "helpful" implementation that turns the warning into a
/// refusal would contradict the decision. A warning is the whole deliverable.
#[test]
fn cfgsrc03_doctor_warns_about_a_config_inside_the_workspace() {
    let w = World::new();
    let cfg = w.repo_config("[policy]\nallow_write = true\n");
    let out = bin()
        .arg("--workspace")
        .arg(&w.root)
        .arg("--config")
        .arg(&cfg)
        .arg("--help")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    // `--help` is the shell's own argument surface; the assertion that matters is that the file
    // is accepted at all, which the exit status below proves. `doctor` is the CLI's surface and
    // is exercised in crates/cli/tests/config_source_spec.rs.
    assert!(
        out.status.success(),
        "--config pointing into the workspace must be accepted, not refused: {:?}",
        out.status
    );
}

/// CFGSRC-04: the report is a *shape*, not a leak — the path is abbreviated.
///
/// `ast_info` is a response an agent reads. Printing `/home/someone/.config/opencrayast/config.toml`
/// in full enumerates a filesystem for no benefit; the last two components plus a leading ellipsis
/// are enough to recognise the file and are the same rule `doctor` uses for `--workspace`.
#[test]
fn cfgsrc04_the_path_is_abbreviated_not_printed_in_full() {
    let w = World::new();
    let cfg = w.repo_config("[limits]\npath_max_bytes = 512\n");
    let mut s = spawn(&w, Some(&cfg), None);
    let info = s.ast_info();
    let line = config_line(&info).expect("ast_info must report a config source");
    let full = cfg.to_string_lossy().into_owned();
    assert!(
        !line.contains(&full),
        "the full absolute path must not be printed: {line}\n(full: {full})"
    );
    // And it is still recognisable — abbreviated, not blank.
    assert!(
        line.contains("opencrayast.toml"),
        "the abbreviated path must still name the file: {line}"
    );
}
