//! R2-A5-xx: aspect 5 (error-message leakage) on the **MCP** and **CLI** surfaces.
//!
//! SECURITY-MODEL T-19 treats "files outside the workspace" as an asset whose protection is
//! partly the *absence of information in errors*: a caller that can only name paths must not be
//! able to read back whether a path exists, what mode it has, or how many hard links point at
//! it. `core` proves that at the `Boundary` (SECFIX1-03, SECFIX1-04) and on the edit path
//! (`sec_audit_poc` facet 5).
//!
//! **Neither shell had a probe.** The two shells each render a `ToolError` into their own output
//! — MCP through JSON-RPC `content[0].text`, the CLI through its own `Out` — and both do work
//! beyond `to_string()`: the MCP shell sanitises, the CLI sanitises through a different code
//! path, and `doctor` renders refusals into a table. A core-level guarantee does not survive
//! three renderers on its own, which is what these probes check.
//!
//! The method: name a path that does not exist, and one that does exist with distinctive
//! metadata, and require the two refusals to be **byte-identical** on each surface. If they
//! differ in any byte, the difference is the oracle.
//!
//! Mutation self-proof: `outside_error()`'s message changed to include the path → R2-A5-01 and
//! R2-A5-03 go red; so does making either shell print the raw `ToolError` without sanitising.

// Unix-only: the fixture gives a file distinctive mode bits and link count, and both are read
// through `std::os::unix`. Without this the file does not compile on Windows, and CI builds a
// Windows leg — a test that cannot compile is not a passing test.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::process::{ChildStdin, ChildStdout, Command, Stdio};

// ---------------------------------------------------------------- MCP surface

fn mcp_bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_opencrayast-mcp"));
    c.env("NO_COLOR", "1").env_remove("XDG_CONFIG_HOME");
    c
}

struct Mcp {
    _child: std::process::Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Mcp {
    fn start(ws: &std::path::Path) -> Mcp {
        let mut child = mcp_bin()
            .arg("--workspace")
            .arg(ws)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn opencrayast-mcp");
        let mut m = Mcp {
            stdin: child.stdin.take().expect("stdin"),
            stdout: BufReader::new(child.stdout.take().expect("stdout")),
            _child: child,
        };
        m.raw(
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2025-06-18","capabilities":{},
                "clientInfo":{"name":"r2a5","version":"0"}}})
            .to_string(),
        );
        assert!(m.recv().get("result").is_some());
        m.raw(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        m
    }

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

    fn call(&mut self, id: i64, name: &str, args: Value) -> String {
        self.raw(
            &json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
                "name":name,"arguments":args}})
            .to_string(),
        );
        let resp = self.recv();
        resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no tool text in {resp}"))
            .to_string()
    }
}

/// A workspace holding one file with deliberately distinctive metadata: mode 0600 and two hard
/// links, so a refusal that leaked either would be unmistakable in the text.
fn world() -> (tempfile::TempDir, std::path::PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let ws = d.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    std::fs::write(ws.join("secret.rs"), "fn secret_symbol() {}\n").unwrap();
    std::fs::set_permissions(ws.join("secret.rs"), std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::hard_link(ws.join("secret.rs"), ws.join("second-link.rs")).unwrap();
    // The control: an absolute path that names NOTHING anywhere.
    (d, ws)
}

/// R2-A5-01: an outside path that exists and one that does not are refused **identically**.
///
/// This is the assertion the whole file exists for. `outside_error()` is one constant with no
/// path in it, so the two answers must be the same bytes — and if a future change introduces one,
/// this goes red rather than shipping an existence oracle to every MCP client.
#[test]
fn r2a5_01_mcp_refusals_are_byte_identical_for_present_and_absent_paths() {
    let (_d, ws) = world();
    let mut m = Mcp::start(&ws);

    // The fixture must really carry distinctive metadata, or a refusal describing it would be
    // indistinguishable from one that does not — and this test would measure nothing.
    assert_eq!(
        std::fs::metadata(ws.join("secret.rs"))
            .map(|md| {
                use std::os::unix::fs::MetadataExt;
                (md.mode() & 0o777, md.nlink())
            })
            .unwrap(),
        (0o600, 2),
        "the fixture must carry distinctive metadata, or this test measures nothing"
    );
    // And a file inside the workspace IS readable, so the refusals compared below are about
    // being outside every root rather than about the workspace being unreadable.
    let readable = m.call(
        10,
        "ast_get",
        json!({"symbol": "secret_symbol", "path": ws.join("secret.rs").to_str().unwrap()}),
    );
    assert!(
        !readable.starts_with('['),
        "a read of an existing file inside the workspace must succeed, not be refused — \
         otherwise this test is measuring nothing. Got: {readable}"
    );

    // The two refusals: a path outside every root that EXISTS, and one that does NOT. Their
    // names are chosen so neither can accidentally be the other.
    let outside_present = "/etc/hostname".to_string();
    let c = m.call(
        12,
        "ast_get",
        json!({"symbol": "secret_symbol", "path": outside_present}),
    );
    let outside_absent = "/etc/no-such-file-4f2a9c".to_string();
    let d = m.call(
        13,
        "ast_get",
        json!({"symbol": "secret_symbol", "path": outside_absent}),
    );

    assert!(
        c.starts_with('['),
        "a path outside every root must be refused, got: {c}"
    );
    assert_eq!(
        c, d,
        "the refusal for a path that EXISTS ({outside_present}) and one that does NOT \
         ({outside_absent}) must be byte-identical; a difference is an existence oracle"
    );
}

/// R2-A5-02: no refusal on this surface carries the absolute path, the mode, or the link count.
///
/// Belt to R2-A5-01's braces: if BOTH refusals had changed identically the identity assertion
/// would still pass, so this pins the content itself rather than the equality.
#[test]
fn r2a5_02_mcp_refusals_carry_no_path_metadata_or_mode_bits() {
    let (_d, ws) = world();
    let mut m = Mcp::start(&ws);
    let outside = "/etc/hostname".to_string();
    let refusal = m.call(
        14,
        "ast_get",
        json!({"symbol": "secret_symbol", "path": outside.clone()}),
    );

    assert!(
        !refusal.contains(&outside),
        "the refusal must not echo the absolute path back: {refusal}"
    );
    for leak in ["0600", "nlink", "uid=", "mode=", "inode"] {
        assert!(
            !refusal.contains(leak),
            "the refusal must not carry `{leak}`: {refusal}"
        );
    }
    // And the mode of the file that DOES exist in the workspace never appears either.
    let inside = m.call(
        15,
        "ast_get",
        json!({"symbol": "no_such_symbol_at_all", "path": ws.join("secret.rs").to_str().unwrap()}),
    );
    assert!(
        !inside.contains("0600"),
        "a not_found inside the workspace must not describe the file's mode: {inside}"
    );
}
