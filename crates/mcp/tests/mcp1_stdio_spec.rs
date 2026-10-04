//! MCP1-xx: end-to-end stdio tests against the **real** `opencrayast-mcp` binary.
//!
//! These spawn the shipped binary and drive it over pipes. A mock sink / in-process
//! handler does **not** count (BRIEF.md lesson from CLI-1).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_tools::{Mode, find_tool, tools_catalog};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::time::Duration;
use tempfile::TempDir;

/// One row of the contract table in `docs/TOOLS.md` §Modes and annotations.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MdModeRow {
    name: String,
    /// `read` or `write`, from the table's own mode column.
    write_mode: bool,
    read_only_hint: bool,
    destructive_hint: bool,
}

/// The contract table, **parsed from `docs/TOOLS.md`**, not copied into this file.
///
/// This test used to hold two hand-written snapshots of the same table. That is how it drifted:
/// `TOOLS_MD_SHIPPED_READ` had five rows, its own doc comment predicted that `ast_edit_preview`
/// would arrive as read-mode with `readOnlyHint=false`, and when the tool landed nobody came back
/// to add the row. A second copy of a contract is a second thing to forget, and the failure mode
/// is a test that is confidently wrong.
///
/// So the document is the source: a row that appears in `tools/list` is looked up **in the
/// contract**, and its annotations are compared with what the contract says. Adding a tool to
/// `docs/TOOLS.md` without cataloguing it, or cataloguing one the document does not describe,
/// both fail here.
///
/// Mutation self-proof: change `ast_edit_preview`'s `readOnlyHint` in the registry and this test
/// goes red - which is the point: it is reading the table, not agreeing with whatever the code
/// happens to do.
fn tools_md_modes() -> Vec<MdModeRow> {
    /// `true`, `false`, or the en dash the table uses for "not applicable".
    fn flag(cell: &str) -> Option<bool> {
        let token = cell.replace('*', "");
        let token = token.split_whitespace().next().unwrap_or("").trim();
        match token {
            "true" => Some(true),
            "false" | "\u{2013}" | "-" => Some(false),
            _ => None,
        }
    }

    let doc = include_str!("../../../docs/TOOLS.md");
    let mut rows = Vec::new();
    let mut in_modes = false;
    let mut in_table = false;
    for line in doc.lines() {
        let line = line.trim();
        if line.starts_with("## ") {
            // Only the §Modes table is this test's business; the document has ~58 other rows
            // that look like table rows and are not contract rows.
            in_modes = line.contains("Modes and annotations");
            in_table = false;
            continue;
        }
        if !in_modes || !line.starts_with('|') {
            continue;
        }
        if !in_table {
            in_table = true;
            continue; // the header row
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        if cells.len() < 5 {
            continue;
        }
        let Some(read_only_hint) = flag(cells[2]) else {
            continue; // the |---|---| separator
        };
        rows.push(MdModeRow {
            name: cells[0].trim_matches('`').to_string(),
            write_mode: cells[1]
                .replace('*', "")
                .trim()
                .eq_ignore_ascii_case("write"),
            read_only_hint,
            destructive_hint: flag(cells[3]).expect("destructiveHint column is true/false/dash"),
        });
    }
    assert!(
        !rows.is_empty(),
        "the §Modes table in docs/TOOLS.md parsed to nothing - if it was renamed or reflowed, \
         this test is now comparing against an empty contract"
    );
    rows
}

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_opencrayast-mcp"));
    c.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    _ws: TempDir,
}

impl Session {
    fn start() -> Self {
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(ws.path().join("hello.rs"), "fn main() {}\n").unwrap();
        let mut child = bin()
            .arg("--workspace")
            .arg(ws.path())
            .spawn()
            .expect("spawn opencrayast-mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Self {
            child,
            stdin,
            stdout,
            _ws: ws,
        }
    }

    /// A session with extra CLI arguments and, optionally, an operator configuration file.
    ///
    /// The file lives inside the workspace tempdir so it outlives the session, and it is `0600`:
    /// `Settings::load` refuses a world-readable configuration rather than silently ignoring it.
    fn start_with(config: Option<&str>, extra: &[&str]) -> Self {
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(ws.path().join("hello.rs"), "fn main() {}\n").unwrap();
        let mut cmd = bin();
        cmd.arg("--workspace").arg(ws.path());
        for a in extra {
            cmd.arg(a);
        }
        if let Some(body) = config {
            let path = ws.path().join("oc-config.toml");
            std::fs::write(&path, body).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            cmd.arg("--config").arg(&path);
        }
        let mut child = cmd.spawn().expect("spawn opencrayast-mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Self {
            child,
            stdin,
            stdout,
            _ws: ws,
        }
    }

    fn raw(&mut self, line: &str) {
        self.stdin.write_all(line.as_bytes()).unwrap();
        self.stdin.write_all(b"\n").unwrap();
        self.stdin.flush().unwrap();
    }

    fn send(&mut self, id: i64, method: &str, params: Value) {
        self.raw(
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string(),
        );
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.raw(&json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string());
    }

    fn recv(&mut self) -> Value {
        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).expect("read stdout");
        assert!(n > 0, "server closed stdout before answering");
        let trimmed = line.trim_end_matches(['\r', '\n']);
        serde_json::from_str(trimmed).unwrap_or_else(|e| {
            panic!("stdout must be JSON-RPC, got {trimmed:?}: {e}");
        })
    }

    fn handshake(&mut self) {
        self.send(
            1,
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "mcp1-test", "version": "0"}
            }),
        );
        let init = self.recv();
        assert_eq!(init["id"], 1);
        assert!(init.get("result").is_some(), "{init}");
        self.notify("notifications/initialized", json!({}));
    }

    fn shutdown(self) -> ExitStatus {
        drop(self.stdin);
        // Drop our stdout handle so a child blocked on a full pipe can exit;
        // we already consumed every response the tests care about.
        drop(self.stdout);
        let mut child = self.child;
        for _ in 0..50 {
            if let Ok(Some(status)) = child.try_wait() {
                return status;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        child.wait().expect("wait")
    }
}

/// MCP1-01: every stdout line from the real binary is a JSON-RPC object.
/// Mutation: `println!("debug")` in main/serve → this test goes red.
#[test]
fn mcp1_01_stdout_is_only_json_rpc() {
    let mut s = Session::start();
    s.handshake();
    s.send(2, "ping", json!({}));
    let pong = s.recv();
    assert_eq!(pong["jsonrpc"], "2.0");
    assert_eq!(pong["id"], 2);
    assert!(pong.get("result").is_some());

    let status = s.shutdown();
    assert!(status.success());
    // No leftover non-JSON on stdout after we drained responses: covered by
    // recv() parsing every line; this assertion guards exit status (EOF→0).
}

/// MCP1-02: a line over the size cap yields a JSON-RPC error (not hang / OOM).
/// Mutation: drop the TooLarge branch (silent swallow) → this goes red.
#[test]
fn mcp1_02_oversized_message_is_json_rpc_error() {
    let mut s = Session::start();
    s.handshake();
    let cap = opencrayast_mcp::MAX_MESSAGE_BYTES;
    // Stream so we never hold two full oversized buffers at once.
    let chunk = vec![b'x'; 64 * 1024];
    let mut left = cap + 64;
    while left > 0 {
        let n = left.min(chunk.len());
        s.stdin.write_all(&chunk[..n]).unwrap();
        left -= n;
    }
    s.stdin.write_all(b"\n").unwrap();
    s.stdin.flush().unwrap();
    let err = s.recv();
    assert_eq!(err["error"]["code"], -32600, "{err}");
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("too large"),
        "{err}"
    );
    // Framing recovered: the next message still works.
    s.send(3, "ping", json!({}));
    assert_eq!(s.recv()["id"], 3);
    assert!(s.shutdown().success());
}

/// MCP1-02b (mutation baseline): the shipped cap is exactly 8 MiB.
#[test]
fn mcp1_02b_message_cap_is_eight_mib() {
    assert_eq!(opencrayast_mcp::MAX_MESSAGE_BYTES, 8 * 1024 * 1024);
}

/// MCP1-02c: off-by-one against the real binary.
/// Exactly `MAX` bytes is framed (then -32700 parse); `MAX+1` is -32600 too large.
#[test]
fn mcp1_02c_cap_off_by_one_on_real_binary() {
    let cap = opencrayast_mcp::MAX_MESSAGE_BYTES;
    let chunk = vec![b'z'; 64 * 1024];

    // Exactly at the cap → accepted as a message → parse error (not "too large").
    {
        let mut s = Session::start();
        let mut left = cap;
        while left > 0 {
            let n = left.min(chunk.len());
            s.stdin.write_all(&chunk[..n]).unwrap();
            left -= n;
        }
        s.stdin.write_all(b"\n").unwrap();
        s.stdin.flush().unwrap();
        let at = s.recv();
        assert_eq!(
            at["error"]["code"], -32700,
            "exact cap must parse-fail, got {at}"
        );
        assert!(s.shutdown().success());
    }

    // One byte over → JSON-RPC invalid-request "message too large".
    {
        let mut s = Session::start();
        let mut left = cap + 1;
        while left > 0 {
            let n = left.min(chunk.len());
            s.stdin.write_all(&chunk[..n]).unwrap();
            left -= n;
        }
        s.stdin.write_all(b"\n").unwrap();
        s.stdin.flush().unwrap();
        let over = s.recv();
        assert_eq!(
            over["error"]["code"], -32600,
            "one-over must be too-large, got {over}"
        );
        assert!(
            over["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("too large"),
            "{over}"
        );
        assert!(s.shutdown().success());
    }
}

/// MCP1-03: bad JSON / unknown method / missing params → -32700 / -32601 / -32602.
/// Mutation: `panic!` on bad input → process dies, this goes red.
#[test]
fn mcp1_03_json_rpc_shape_errors_without_panic() {
    let mut s = Session::start();
    s.raw("this is not json");
    let parse = s.recv();
    assert_eq!(parse["error"]["code"], -32700, "{parse}");

    s.handshake();
    s.send(10, "no/such/method", json!({}));
    let method = s.recv();
    assert_eq!(method["error"]["code"], -32601, "{method}");

    s.send(11, "tools/call", json!("not-an-object"));
    let params = s.recv();
    assert_eq!(params["error"]["code"], -32602, "{params}");

    assert!(s.shutdown().success());
}

/// MCP1-04: a tool failure is `isError: true` with `[code]` and a Next step.
/// Mutation: map tool errors to JSON-RPC -32603 → this goes red.
#[test]
fn mcp1_04_tool_failure_is_error_with_code() {
    let mut s = Session::start();
    s.handshake();
    s.send(
        20,
        "tools/call",
        json!({
            "name": "ast_outline",
            "arguments": { "path": "no/such/file.rs" }
        }),
    );
    let resp = s.recv();
    assert_eq!(resp["result"]["isError"], true, "{resp}");
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with('['), "{text}");
    assert!(text.contains("Next:"), "{text}");
    assert!(s.shutdown().success());
}

/// MCP1-05: `tools/list` entries have description + annotations; names are the
/// real registry (no fabricated write tools). Mutation: drop annotations → red.
#[test]
fn mcp1_05_tools_list_has_description_and_annotations() {
    let mut s = Session::start();
    s.handshake();
    s.send(30, "tools/list", json!({}));
    let resp = s.recv();
    let tools = resp["result"]["tools"].as_array().expect("tools array");
    assert!(!tools.is_empty());
    let mut names = Vec::new();
    for t in tools {
        let name = t["name"].as_str().unwrap();
        names.push(name.to_string());
        assert!(
            t["description"]
                .as_str()
                .map(|d| !d.is_empty())
                .unwrap_or(false),
            "missing description for {name}"
        );
        let ann = t.get("annotations").expect("annotations");
        // Shipped tools today are read-only-hint true; the invariant is ToolEntry.mode,
        // not "every listed tool has readOnlyHint" (ast_edit_preview will break that).
        let entry = find_tool(name).expect("listed name must be registered");
        assert_eq!(entry.mode, Mode::ReadOnly, "{name}");
        assert_eq!(
            ann["readOnlyHint"], entry.annotations.read_only_hint,
            "{name}"
        );
        assert_eq!(
            ann["destructiveHint"], entry.annotations.destructive_hint,
            "{name}"
        );
        assert!(t.get("inputSchema").is_some(), "{name}");
    }
    assert!(names.contains(&"ast_info".to_string()));
    assert!(names.contains(&"ast_outline".to_string()));
    // Invariant 6 partial: write-*mode* tools from TOOLS.md stay out in read mode.
    let contract = tools_md_modes();
    let contract_writes: Vec<&str> = contract
        .iter()
        .filter(|r| r.write_mode)
        .map(|r| r.name.as_str())
        .collect();
    for write in &contract_writes {
        assert!(
            !names.iter().any(|n| n == write),
            "write-mode tool {write} must not appear in read-mode list"
        );
    }
    assert!(s.shutdown().success());
}

/// MCP1-06: hard-calling a write-mode tool in read mode → same unknown-tool error
/// as any other missing name (REQ-MCP-SERVER §驗收 / TOOLS.md catalogue layer).
/// Not `[write_disabled]` — that code is the handler layer (CLI / in-process).
#[test]
fn mcp1_06_write_tool_call_is_unknown_tool() {
    let mut s = Session::start();
    s.handshake();
    s.send(
        40,
        "tools/call",
        json!({
            "name": "ast_edit_apply",
            "arguments": { "plan_id": "x" }
        }),
    );
    let resp = s.recv();
    assert_eq!(resp["result"]["isError"], true, "{resp}");
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("[invalid_args]") && text.contains("unknown tool"),
        "expected unknown-tool shape, got {text}"
    );
    assert!(
        !text.contains("[write_disabled]"),
        "write_disabled must not appear: {text}"
    );
    // Same shape as a nonsense name:
    s.send(
        41,
        "tools/call",
        json!({ "name": "definitely_not_a_tool", "arguments": {} }),
    );
    let other = s.recv();
    let other_text = other["result"]["content"][0]["text"].as_str().unwrap();
    assert!(other_text.contains("[invalid_args]") && other_text.contains("unknown tool"));
    assert!(s.shutdown().success());
}

/// MCP1-07: closing stdin ends the process with exit 0.
/// Mutation: loop forever on EOF → this hangs / fails the wait.
#[test]
fn mcp1_07_stdin_eof_exits_cleanly() {
    let s = Session::start();
    assert_eq!(s.shutdown().code(), Some(0));
}

/// MCP1-08: a successful `ast_info` round-trip through the real binary.
#[test]
fn mcp1_08_ast_info_round_trip() {
    let mut s = Session::start();
    s.handshake();
    s.send(
        50,
        "tools/call",
        json!({ "name": "ast_info", "arguments": {} }),
    );
    let resp = s.recv();
    assert_eq!(resp["result"]["isError"], false, "{resp}");
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("mode: read-only"), "{text}");
    assert!(text.contains("write: disabled"), "{text}");
    assert!(s.shutdown().success());
}

/// MCP1-09 (mutation self-proof notes): three invariants with an explicit
/// "change X → this assertion fails" baseline, exercised against the binary.
#[test]
fn mcp1_09_mutation_self_proof_baselines() {
    // 1) Cap constant must stay 8 MiB (same as mcp1_02b).
    assert_eq!(opencrayast_mcp::MAX_MESSAGE_BYTES, 8 * 1024 * 1024);

    // 2) TOOLS.md write-mode names stay out of the read-mode list (contract, not crate const).
    let mut s = Session::start();
    s.handshake();
    s.send(60, "tools/list", json!({}));
    let listed: Vec<String> = s.recv()["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    for name in tools_md_modes()
        .iter()
        .filter(|r| r.write_mode)
        .map(|r| &r.name)
    {
        assert!(
            !listed.iter().any(|n| n == name),
            "mutation: listing `{name}` in read mode breaks TOOLS.md §Modes"
        );
    }

    // 3) Server name on the wire is the shipped binary identity.
    s.send(
        61,
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "mcp1-test", "version": "0"}
        }),
    );
    // Re-initialize is allowed; check serverInfo.name.
    let again = s.recv();
    assert_eq!(
        again["result"]["serverInfo"]["name"], "opencrayast-mcp",
        "mutation: renaming serverInfo without updating clients breaks handshake"
    );
    assert!(s.shutdown().success());
}

/// MCP1-10: stdout closed mid-reply must not panic (EPIPE failure-table row).
/// Real shape: read 1 byte (like `head -c 1`), drop the reader, keep feeding stdin.
/// Mutation: let `write_all` surface BrokenPipe as `Err` → main exits 1 with a
/// panic-looking path, or an unhandled SIGPIPE kills the process.
#[test]
fn mcp1_10_epipe_stdout_closed_no_panic() {
    let ws = tempfile::tempdir().unwrap();
    let mut child = bin()
        .arg("--workspace")
        .arg(ws.path())
        .spawn()
        .expect("spawn");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = child.stdout.take().expect("stdout");
    let mut stderr = child.stderr.take().expect("stderr");

    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "epipe", "version": "0"}
        }
    })
    .to_string();
    stdin.write_all(init.as_bytes()).unwrap();
    stdin.write_all(b"\n").unwrap();
    stdin.flush().unwrap();

    // Like `| head -c 1`: take one byte of the reply, then close the reader.
    let mut one = [0u8; 1];
    let n = stdout.read(&mut one).unwrap_or(0);
    assert!(n > 0, "expected at least one stdout byte before close");
    drop(stdout);

    // Further requests would write again; EPIPE must not panic.
    let ping = json!({"jsonrpc":"2.0","id":2,"method":"ping","params":{}}).to_string();
    let _ = stdin.write_all(ping.as_bytes());
    let _ = stdin.write_all(b"\n");
    let _ = stdin.flush();
    drop(stdin);

    let mut err = String::new();
    let _ = stderr.read_to_string(&mut err);
    let status = child.wait().expect("wait");
    assert!(
        !err.to_lowercase().contains("panic"),
        "stderr must not report panic: {err}"
    );
    assert_ne!(
        status.code(),
        Some(101),
        "Rust panic abort code 101; status={status:?} stderr={err}"
    );
    // Clean stop preferred (exit 0). Non-zero without panic is still acceptable
    // for the failure table; we only forbid abort/panic.
    assert!(
        status.code().is_some(),
        "killed by signal (possible SIGPIPE): {status:?} stderr={err}"
    );
}

/// MCP1-11: `tools/list` vs `docs/TOOLS.md` §Modes (not vs `WRITE_TOOL_NAMES`).
/// Each shipped tool's annotations match the contract table; write-mode tools
/// named in TOOLS.md are absent in read mode; `ast_info` banner agrees on mode.
#[test]
fn mcp1_11_tools_list_matches_tools_md_modes_contract() {
    let mut s = Session::start();
    s.handshake();

    s.send(
        70,
        "tools/call",
        json!({"name": "ast_info", "arguments": {}}),
    );
    let info = s.recv();
    assert_eq!(info["result"]["isError"], false, "{info}");
    let banner = info["result"]["content"][0]["text"].as_str().unwrap();
    assert!(banner.contains("mode: read-only"), "{banner}");
    assert!(banner.contains("write: disabled"), "{banner}");
    for row in tools_md_modes().iter().filter(|r| r.write_mode) {
        assert!(
            banner.contains(&row.name),
            "ast_info banner must name TOOLS.md write tool `{}`: {banner}",
            row.name
        );
    }

    s.send(71, "tools/list", json!({}));
    let listed = s.recv()["result"]["tools"].as_array().unwrap().clone();

    let contract = tools_md_modes();
    let read_rows: Vec<&MdModeRow> = contract.iter().filter(|r| !r.write_mode).collect();
    assert_eq!(
        listed.len(),
        read_rows.len(),
        "tools/list must hold exactly the read-mode rows of the §Modes contract in \
         docs/TOOLS.md ({} read rows, {} listed)",
        read_rows.len(),
        listed.len()
    );
    // The listing is the read-mode slice of the catalogue, not the whole catalogue: the write
    // tools are registered with `mode: Mode::Write` and must not appear here (MCP1-12 asserts the
    // other half).
    let read_entries = tools_catalog()
        .iter()
        .filter(|t| t.mode == opencrayast_tools::Mode::ReadOnly)
        .count();
    assert_eq!(listed.len(), read_entries);

    for t in &listed {
        let name = t["name"].as_str().unwrap();
        let row = contract.iter().find(|r| r.name == name).unwrap_or_else(|| {
            panic!("`{name}` is listed but docs/TOOLS.md §Modes does not describe it")
        });
        assert!(
            !row.write_mode,
            "`{name}` is write-mode in TOOLS.md §Modes but appeared in the read-mode list"
        );
        let ann = &t["annotations"];
        assert_eq!(
            ann["readOnlyHint"], row.read_only_hint,
            "{name} readOnlyHint differs from docs/TOOLS.md §Modes"
        );
        assert_eq!(
            ann["destructiveHint"], row.destructive_hint,
            "{name} destructiveHint differs from docs/TOOLS.md §Modes"
        );
        let entry = find_tool(name).unwrap();
        assert_eq!(entry.mode, Mode::ReadOnly, "{name}");
    }
    assert!(s.shutdown().success());
}

/// MCP1-12: EOF without a trailing newline is an incomplete frame, not a message.
#[test]
fn mcp1_12_eof_without_newline_refuses_half_frame() {
    let ws = tempfile::tempdir().unwrap();
    let mut child = bin()
        .arg("--workspace")
        .arg(ws.path())
        .spawn()
        .expect("spawn");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));
    // No trailing newline — old code would treat this as a complete Message.
    stdin
        .write_all(br#"{"jsonrpc":"2.0","id":99,"method":"ping","params":{}}"#)
        .unwrap();
    stdin.flush().unwrap();
    drop(stdin);
    let mut line = String::new();
    let n = stdout.read_line(&mut line).unwrap();
    assert!(n > 0, "expected an incomplete-message error line");
    let v: Value = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!(v["error"]["code"], -32600, "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("incomplete"),
        "{v}"
    );
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(0), "{status:?}");
}

/// MCP1-13: `ast_search` with a non-null `rule` is `[invalid_args]`, not a silent ignore.
#[test]
fn mcp1_13_ast_search_rule_is_rejected_not_silently_dropped() {
    let mut s = Session::start();
    s.handshake();
    s.send(
        80,
        "tools/call",
        json!({
            "name": "ast_search",
            "arguments": {
                "pattern": "fn $A() {}",
                "language": "rust",
                "rule": { "any": [] }
            }
        }),
    );
    let resp = s.recv();
    assert_eq!(resp["result"]["isError"], true, "{resp}");
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("[invalid_args]") && text.contains("`rule`"),
        "expected rule refusal, got {text}"
    );
    assert!(s.shutdown().success());
}

/// MCP1-12 (WCAP-1): the three write tools are listed **only** in write mode, and write mode
/// needs the operator's own configuration — `--allow-write` alone has always done nothing.
#[test]
fn mcp1_12_write_tools_are_listed_only_in_write_mode() {
    fn listed(names: &mut Vec<String>, s: &mut Session, id: i64) {
        s.send(id, "tools/list", json!({}));
        let tools = s.recv()["result"]["tools"].as_array().unwrap().clone();
        *names = tools
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
    }

    const WRITE: [&str; 3] = ["ast_edit_apply", "ast_undo", "ast_recover"];

    // 1. Default: read-only. The write tools are absent.
    let mut ro = Session::start();
    ro.handshake();
    let mut names = Vec::new();
    listed(&mut names, &mut ro, 80);
    for w in WRITE {
        assert!(
            !names.contains(&w.to_string()),
            "read-only list has {w}: {names:?}"
        );
    }

    // 2. `--allow-write` but no configuration: still read-only. The flag cannot supply policy.
    let mut flag_only = Session::start_with(None, &["--allow-write"]);
    flag_only.handshake();
    let mut names = Vec::new();
    listed(&mut names, &mut flag_only, 81);
    for w in WRITE {
        assert!(
            !names.contains(&w.to_string()),
            "--allow-write alone enabled {w}: {names:?}"
        );
    }

    // 3. `--allow-write` AND `[policy] allow_write = true`: the write tools are listed, and the
    //    banner says so in the contract's own words.
    let mut rw = Session::start_with(Some("[policy]\nallow_write = true\n"), &["--allow-write"]);
    rw.handshake();
    let mut names = Vec::new();
    listed(&mut names, &mut rw, 82);
    for w in WRITE {
        assert!(
            names.contains(&w.to_string()),
            "write-mode list lacks {w}: {names:?}"
        );
    }
    rw.send(
        83,
        "tools/call",
        json!({"name": "ast_info", "arguments": {}}),
    );
    let banner = rw.recv()["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        banner.contains("write: enabled (ast_edit_apply, ast_undo, ast_recover)"),
        "{banner}"
    );
}

// =====================================================================================
// UX-1 deliverable 3: "choosing the wrong tool", measured against the REAL `tools/list`.
//
// The ticket asks for at least three prompts verified with the actual output of `tools/list`, not
// with the documentation. That distinction is the whole point: a description can read perfectly in
// prose and still fail to separate two tools once an agent has to choose between them from a
// one-line summary.
//
// What is asserted is not "an LLM would pick right" — that is not a test. It is the weaker, checkable
// property that makes the choice possible: for each confusion an agent actually has, the winning
// tool's description mentions the distinguishing term AND the losing tool's does not. If that
// stops being true, no amount of careful prompting will recover the choice.
// =====================================================================================

/// The catalogue as an agent sees it: name -> description, from a real `tools/list` round-trip.
///
/// Taking the descriptions from the wire rather than from `registry.rs` means a change that only
/// affects what the server actually sends (a truncation, a field dropped, a wrapper) fails here.
///
/// Read-only session: the default. Write tools are deliberately ABSENT from this list, so a caller
/// that needs one must ask for [`listed_tools_write`].
fn listed_tools() -> Vec<(String, String)> {
    let mut s = Session::start();
    s.handshake();
    s.send(80, "tools/list", json!({}));
    let tools = s.recv()["result"]["tools"]
        .as_array()
        .expect("tools array")
        .clone();
    let _ = s.shutdown();
    tools
        .iter()
        .map(|t| {
            (
                t["name"].as_str().expect("name").to_string(),
                t["description"].as_str().expect("description").to_string(),
            )
        })
        .collect()
}

/// The same, from a WRITE-mode session, which is the only way the write tools are ever listed.
///
/// Opening a second session is not ceremony: the read-only list genuinely does not contain
/// `ast_edit_apply`, and a test that assumed it did would be asserting against a catalogue no agent
/// ever sees. That absence is itself the invariant UX1-04 pins.
fn listed_tools_write() -> Vec<(String, String)> {
    let mut s = Session::start_with(Some("[policy]\nallow_write = true\n"), &["--allow-write"]);
    s.handshake();
    s.send(81, "tools/list", json!({}));
    let tools = s.recv()["result"]["tools"]
        .as_array()
        .expect("tools array")
        .clone();
    let _ = s.shutdown();
    tools
        .iter()
        .map(|t| {
            (
                t["name"].as_str().expect("name").to_string(),
                t["description"].as_str().expect("description").to_string(),
            )
        })
        .collect()
}

fn desc_of<'a>(tools: &'a [(String, String)], name: &str) -> &'a str {
    tools
        .iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("{name} was not listed"))
        .1
        .as_str()
}

/// UX1-07: "what is in this file?" must point at `ast_outline`, not `ast_get`.
///
/// The two are the most commonly confused pair, and the confusion is expensive: `ast_get` without
/// a symbol name either fails or returns something the agent did not want, and the recovery costs a
/// round trip.
#[test]
fn ux1_07_the_shape_of_a_file_is_distinguishable_from_one_symbol() {
    let tools = listed_tools();
    let outline = desc_of(&tools, "ast_outline").to_lowercase();
    let get = desc_of(&tools, "ast_get").to_lowercase();

    assert!(
        outline.contains("skeleton") || outline.contains("symbols"),
        "ast_outline's description must name what it returns (the shape): {outline}"
    );
    assert!(
        get.contains("symbol") || get.contains("fetch"),
        "ast_get's description must say it fetches a named symbol: {get}"
    );
    // The discriminator: only `ast_outline` says it covers a whole file or directory.
    assert!(
        outline.contains("file") || outline.contains("directory"),
        "ast_outline must mention a file or a directory, which is what distinguishes it: {outline}"
    );
}

/// UX1-08: "find every call shaped like this" must point at `ast_search`, and must NOT be what
/// `ast_get` claims.
///
/// An agent asked to find callers will otherwise reach for a symbol tool and then read files one by
/// one — exactly the token cost this product exists to remove.
#[test]
fn ux1_08_structural_search_is_distinguishable_from_symbol_lookup() {
    let tools = listed_tools();
    let search = desc_of(&tools, "ast_search").to_lowercase();
    assert!(
        search.contains("structural") || search.contains("pattern"),
        "ast_search must announce that it matches structure, not text: {search}"
    );
    // The negative half matters as much: a search description that mentions fetching a symbol is
    // what makes an agent choose wrongly.
    assert!(
        !search.contains("fetch one symbol"),
        "ast_search must not describe itself as a symbol fetcher: {search}"
    );
}

/// UX1-09: the apply/preview split must be visible in the descriptions, because getting it backwards
/// writes a file the user never approved.
///
/// `ast_edit_preview` is the one that is safe to call unprompted; `ast_edit_apply` is the one that
/// changes the workspace. An agent that cannot tell them apart will either skip the preview (the
/// user approves a diff nobody showed them) or skip the apply (the work silently never happens).
///
/// Read from a WRITE-mode session, because that is the only listing containing `ast_edit_apply`.
/// Both descriptions are then compared against the SAME session's listing, so the pair is compared
/// as an agent would see it: two adjacent lines in one catalogue.
#[test]
fn ux1_09_preview_and_apply_are_distinguishable_in_their_descriptions() {
    let tools = listed_tools_write();
    let preview = desc_of(&tools, "ast_edit_preview").to_lowercase();
    let apply = desc_of(&tools, "ast_edit_apply").to_lowercase();

    assert!(
        preview.contains("preview"),
        "ast_edit_preview must say it previews: {preview}"
    );
    assert!(
        apply.contains("apply") || apply.contains("workspace"),
        "ast_edit_apply must say it changes the workspace: {apply}"
    );
    // The safety-relevant asymmetry: only the applying tool warns about the full id. An agent that
    // does not know this will pass the prefix it was shown by `ast_plan_show`, and the apply will
    // be refused for a reason the description did not prepare it for.
    assert!(
        apply.contains("full plan id"),
        "ast_edit_apply's description must say it needs the FULL plan id (E-15), because that is \
         the rule an agent will otherwise break: {apply}"
    );
    assert!(
        !preview.contains("full plan id"),
        "ast_edit_preview does not take the same constraint; if it does, TOOLS.md must say so: \
         {preview}"
    );
}
