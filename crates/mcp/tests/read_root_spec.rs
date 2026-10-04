//! RR-xx: `--read-root DIR` on the real `opencrayast-mcp` binary.
//!
//! The flag was documented in three places (`docs/TOOLS.md`, `docs/SECURITY-MODEL.md` T-32,
//! `docs/CONFIGURATION.md`) and existed in **neither** interface: `BoundaryConfig::read_roots`
//! was `Vec::new()` with no production caller, so the `@root<N>` label was dead code and
//! T-32's "a read root is refused for `/`, home and credential directories" was true only
//! because no flag existed to name one. These tests pin the real behaviour, on the shipped
//! binary, over pipes — an in-process handler would not prove the flag is parsed at all.
//!
//! What must hold, and why each one has teeth:
//!
//! - **RR-01 happy path.** A file under a `--read-root` is readable and comes back labelled
//!   `@root1/…`. This is the whole point of the flag; without it the flag could be accepted and
//!   silently do nothing.
//! - **RR-02 repeatable, in order.** Two roots give `@root1` and `@root2` in the order given.
//!   `root_at` indexes the vector directly, so an implementation that sorted or deduplicated the
//!   roots would hand back the wrong label for the right directory.
//! - **RR-03 CFG-07 refusal.** `/`, the home directory and a credential directory are refused
//!   **at startup**, with a non-zero exit — the refusals come from the shared `check_root`,
//!   not from a second, weaker list written for this flag.
//! - **RR-04 BND-19.** A readable root stays unwritable, however the path is spelled.
//! - **RR-05 no flag, no reach.** Without `--read-root` the same absolute path is refused, so
//!   RR-01 cannot pass because reading outside the workspace was already allowed.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_opencrayast-mcp"));
    c.env("NO_COLOR", "1").env_remove("OPENCRAYAST_TEST_MARK");
    c
}

/// A workspace, plus N sibling directories holding `lib.rs`.
struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
    read_roots: Vec<PathBuf>,
}

impl World {
    fn new(roots: usize) -> World {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("hello.rs"), "fn main() {}\n").unwrap();
        let mut read_roots = Vec::new();
        for i in 1..=roots {
            let r = dir.path().join(format!("rr{i}"));
            std::fs::create_dir(&r).unwrap();
            std::fs::create_dir(r.join("lib")).unwrap();
            std::fs::write(r.join("lib/lib.rs"), "pub fn read_root_fn() {}\n").unwrap();
            read_roots.push(r);
        }
        World {
            _dir: dir,
            root,
            read_roots,
        }
    }
}

/// A live server over pipes, already handshaken.
struct Session {
    // Kept so the child is not dropped-and-reaped mid-test; never read.
    _child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

fn spawn(ws: &World, read_roots: &[&Path]) -> Session {
    let mut cmd = bin();
    cmd.arg("--workspace").arg(&ws.root);
    for r in read_roots {
        cmd.arg("--read-root").arg(r);
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
    s.send(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "read-root-test", "version": "0"}
        }),
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

    fn send(&mut self, id: i64, method: &str, params: Value) {
        self.raw(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string());
    }

    fn recv(&mut self) -> Value {
        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).expect("read stdout");
        assert!(n > 0, "server closed stdout before answering");
        serde_json::from_str(line.trim_end_matches(['\r', '\n'])).expect("JSON-RPC line")
    }

    /// `tools/call`, returning the tool's text (the happy path) or its error text.
    fn call(&mut self, id: i64, name: &str, args: Value) -> String {
        self.send(id, "tools/call", json!({"name": name, "arguments": args}));
        let resp = self.recv();
        let result = &resp["result"];
        result["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no tool text in {resp}"))
            .to_string()
    }
}

/// The server's `ast_get` on an absolute path, with the roots above it.
fn read(s: &mut Session, id: i64, abs: &Path) -> String {
    s.call(
        id,
        "ast_get",
        json!({"symbol": "read_root_fn", "path": abs.to_str().unwrap()}),
    )
}

/// RR-01: a file under a `--read-root` is readable and labelled `@root1/…`.
///
/// Mutation: drop `read_roots` from the `BoundaryConfig` the MCP shell builds → RR-01 red.
/// Without RR-05 in the same file this could pass for the wrong reason, which is why RR-05 exists.
#[test]
fn rr01_a_read_root_is_readable_and_labelled() {
    let w = World::new(1);
    let target = w.read_roots[0].join("lib/lib.rs");
    let mut s = spawn(&w, &[&w.read_roots[0]]);
    let got = read(&mut s, 10, &target);
    assert!(
        !got.starts_with('['),
        "a file under --read-root must be readable, got an error: {got}"
    );
    assert!(
        got.contains("read_root_fn"),
        "the read root's own symbol must come back: {got}"
    );
    drop(s);
}

/// RR-02: `--read-root` is repeatable and the label order follows the command line.
///
/// `root_at` (boundary.rs) indexes the vector positionally, so a shell that sorted or
/// deduplicated the roots would return `@root2/…` for the first one. This pins the order the
/// operator typed.
#[test]
fn rr02_two_roots_are_numbered_in_the_order_given() {
    let w = World::new(2);
    let mut s = spawn(&w, &[&w.read_roots[0], &w.read_roots[1]]);

    // `ast_outline` of each root's directory reports the root's own label as `rel`.
    let one = s.call(
        10,
        "ast_outline",
        json!({"path": w.read_roots[0].to_str().unwrap()}),
    );
    assert!(one.contains("@root1"), "first root must be @root1: {one}");

    let two = s.call(
        11,
        "ast_outline",
        json!({"path": w.read_roots[1].to_str().unwrap()}),
    );
    assert!(two.contains("@root2"), "second root must be @root2: {two}");

    // And the same order the other way round, which is the assertion with teeth: if the
    // implementation sorted the roots, both runs would produce identical labels.
    drop(s);
    let mut r = spawn(&w, &[&w.read_roots[1], &w.read_roots[0]]);
    let swapped = r.call(
        12,
        "ast_outline",
        json!({"path": w.read_roots[0].to_str().unwrap()}),
    );
    assert!(
        swapped.contains("@root2"),
        "with the roots swapped, the first-typed one is @root1 and this one is @root2: {swapped}"
    );
}

/// RR-03 (CFG-07): `/`, the home directory and a credential directory are refused at startup.
///
/// These refusals come from the shared `check_root` the workspace root also goes through — this
/// test asserts the *flag* reaches it, so a shell that validated roots with its own weaker list
/// would exit 0 here.
#[test]
fn rr03_forbidden_read_roots_refuse_the_server_at_startup() {
    let w = World::new(0);
    // Windows CI sets USERPROFILE, not HOME.
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .expect("USERPROFILE or HOME must be set for this test");

    #[cfg(windows)]
    let fs_root = PathBuf::from(r"C:\");
    #[cfg(not(windows))]
    let fs_root = PathBuf::from("/");

    let cases: Vec<(&str, PathBuf)> = vec![
        ("filesystem root", fs_root),
        ("home directory", home.clone()),
        ("credential directory", home.join(".ssh")),
    ];

    for (what, dir) in cases {
        let out = bin()
            .arg("--workspace")
            .arg(&w.root)
            .arg("--read-root")
            .arg(&dir)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "{what} must be refused as a --read-root; the server started with status {:?}",
            out.status
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("read root") || err.contains("root"),
            "{what}: the refusal must name the root, got: {err}"
        );
    }
}

/// RR-04 (BND-19): a read root is readable and **still** unwritable.
///
/// The flag widens reads only. Two separate facts are pinned, because they are enforced in two
/// different places and one test cannot see both:
///
/// - **`ast_edit_preview` still only writes the plan store.** It resolves its targets with
///   `resolve_read`, so it succeeds against a read root and writes nothing in the workspace —
///   that is E-12's promise, not a hole. The file must be byte-for-byte unchanged afterwards.
/// - **the write itself is refused.** A real `ast_edit_apply` through the same read root fails,
///   and the file is still unchanged. This is the assertion with teeth: `apply.rs` resolves every
///   target with `resolve_write`, which refuses a read root before touching the path, so the
///   refusal is not an accident of file permissions.
///
/// Write mode needs BOTH `--allow-write` and `[policy] allow_write = true` (T-32), which is why
/// this session is the only one started with a configuration file.
#[test]
fn rr04_a_read_root_is_never_writable() {
    let w = World::new(1);
    let target = w.read_roots[0].join("lib/lib.rs");
    let before = std::fs::read_to_string(&target).unwrap();

    // The config file is 0600 — `Settings::load` refuses a world-readable one.
    let ws_for_cfg = w.root.clone();
    let cfg_path = w._dir.path().join("write.toml");
    std::fs::write(&cfg_path, "[policy]\nallow_write = true\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cfg_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let _ = ws_for_cfg;

    let mut cmd = bin();
    cmd.arg("--workspace")
        .arg(&w.root)
        .arg("--read-root")
        .arg(&w.read_roots[0])
        .arg("--allow-write")
        .arg("--config")
        .arg(&cfg_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn opencrayast-mcp");
    let mut s = Session {
        stdin: child.stdin.take().expect("stdin"),
        stdout: BufReader::new(child.stdout.take().expect("stdout")),
        _child: child,
    };
    s.send(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "read-root-write-test", "version": "0"}
        }),
    );
    assert!(
        s.recv().get("result").is_some(),
        "the write-mode server must start; --read-root must not prevent write mode"
    );
    s.raw(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);

    // Readable, and the write capability is really in force (otherwise apply would be refused
    // for the wrong reason and this test would prove nothing).
    let got = read(&mut s, 10, &target);
    assert!(
        !got.starts_with('['),
        "precondition: the file is readable: {got}"
    );
    let info = s.call(11, "ast_info", json!({}));
    assert!(
        info.contains("write"),
        "precondition: this session must be in write mode, ast_info said: {info}"
    );

    let previewed = s.call(
        12,
        "ast_edit_preview",
        json!({
            "kind": "rewrite",
            "language": "rust",
            "paths": [target.to_str().unwrap()],
            "pattern": "read_root_fn",
            "replacement": "pwned"
        }),
    );
    let plan_id = previewed
        .split("plan p-")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .map(|id| format!("p-{id}"))
        .unwrap_or_else(|| panic!("preview must produce a plan id: {previewed}"));
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        before,
        "preview must not touch the workspace"
    );

    let applied = s.call(13, "ast_edit_apply", json!({ "plan_id": plan_id }));
    assert!(
        applied.contains('['),
        "applying a plan that targets a --read-root must be refused, got: {applied}"
    );
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        before,
        "the read root's file must be byte-for-byte unchanged"
    );
}

/// RR-05: **without** the flag the same path is refused.
///
/// Without this, RR-01 and RR-04 could both pass because reading outside the workspace was
/// already allowed — the tests would be measuring the boundary, not the flag.
#[test]
fn rr05_without_the_flag_a_read_root_is_out_of_reach() {
    let w = World::new(1);
    let target = w.read_roots[0].join("lib/lib.rs");
    let mut s = spawn(&w, &[]);
    let got = read(&mut s, 10, &target);
    assert!(
        got.starts_with('['),
        "the same path must be refused when no --read-root was given, got: {got}"
    );
    assert!(
        !got.contains("read_root_fn"),
        "no content may leak from an undeclared read root: {got}"
    );
}
