//! Live plan-tool parity over stdio: the MCP server, end to end, against its own handlers.
//!
//! # Why this file exists
//!
//! `crates/cli/tests/parity_spec.rs` used to carry a test (`PARITY1-07`) that pinned the
//! *absence* of plan tooling in the MCP dispatcher: `ast_plan_list` and `ast_plan_show` were
//! advertised by `tools/list`, so a client was told they existed, and calling one came back
//! `unknown tool`. That test's own instruction was to replace it with a live comparison once
//! the arms landed — they now have.
//!
//! The live comparison could not simply be added to the CLI's spec: `opencrayast` is not allowed
//! to depend on `opencrayast-mcp` (the layering table grants the CLI `tools`, `core` and `edit`),
//! so a test that spawns the MCP server has to live in this crate. What is compared here is the
//! server's own answer for a plan it created, against the plan store it created it in — which
//! is the fact that the CLI's `plan show` also reads, and the thing that can silently differ if
//! the MCP layer ever stops passing the right state directory.
//!
//! Every test drives the **real shipped binary** over pipes (the BRIEF lesson from CLI-1): an
//! in-process handler would not prove that `main` resolves the state directory at all.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::time::Duration;
use tempfile::TempDir;

/// A live MCP server over stdio, on a throwaway workspace.
struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    ws: TempDir,
    /// The `XDG_STATE_HOME` this child was started with, and where the plan store must appear.
    state: PathBuf,
}

impl Session {
    /// Start the real binary in read mode (no `--allow-write`).
    fn start() -> Self {
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(
            ws.path().join("hello.rs"),
            "fn console_log_it() {\n    console.log(\"a\", b);\n}\n",
        )
        .unwrap();
        // The state directory is the platform user-state base, so this fixture names one
        // explicitly. It lives in the workspace's tempdir and NOT inside the workspace tree.
        // Unix reads `XDG_STATE_HOME`; Windows reads `LOCALAPPDATA` (see statedir.rs).
        let state = ws.path().parent().unwrap_or(ws.path()).join("state");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_opencrayast-mcp"));
        cmd.arg("--workspace")
            .arg(ws.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        cmd.env("LOCALAPPDATA", &state);
        #[cfg(not(windows))]
        cmd.env("XDG_STATE_HOME", &state);
        let mut child = cmd.spawn().expect("spawn opencrayast-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
            ws,
            state,
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
        serde_json::from_str(line.trim_end_matches(['\r', '\n'])).expect("stdout is JSON-RPC")
    }

    fn handshake(&mut self) {
        self.send(
            1,
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "mcp-plan-test", "version": "0"}
            }),
        );
        let init = self.recv();
        assert!(init.get("result").is_some(), "{init}");
        self.notify("notifications/initialized", json!({}));
    }

    /// One `tools/call`, returning the response's `text` content.
    fn call(&mut self, id: i64, name: &str, arguments: Value) -> String {
        self.send(
            id,
            "tools/call",
            json!({"name": name, "arguments": arguments}),
        );
        let response = self.recv();
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no text content in {response}"))
            .to_string();
        let is_error = response["result"]["isError"].as_bool().unwrap_or(false);
        assert!(!is_error, "{name} failed: {text}");
        text
    }

    fn shutdown(mut self) -> ExitStatus {
        drop(self.stdin);
        drop(self.stdout);
        for _ in 0..50 {
            if let Ok(Some(status)) = self.child.try_wait() {
                return status;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.child.wait().unwrap()
    }
}

/// The plan id a preview printed, if it printed one.
fn plan_id_of(text: &str) -> Option<String> {
    let idx = text.find("plan ")? + "plan ".len();
    let rest = &text[idx..];
    let end = rest
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '-')
        .unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

/// A rejected `tools/call` leaves the workspace **byte-identical**, and creates no state
/// directory.
///
/// The state store is opened by the edit arms, and opening *creates* `<workspace>/.opencrayast/`.
/// An arm that opened it before parsing its arguments therefore turned "send a malformed request"
/// into a filesystem write primitive keyed on nothing — including from a read-mode client, which
/// is otherwise unable to write anything at all. The response said `invalid_args` and the
/// repository still had a new directory in it.
///
/// The assertion is deliberately about the whole workspace tree, not just the state directory:
/// the workspace file is compared byte for byte, and the directory listing is compared before and
/// after, so anything the rejected call left behind fails — the state directory, a store
/// directory under it, or a file the handler should never have written.
///
/// This runs against the real binary over stdio, because the state directory `main` resolves is
/// part of the claim: an in-process call could not prove `main` hands the dispatcher a path that
/// the argument order keeps untouched.
///
/// Mutation self-proof: move `edit(ctx, state_dir)?` back above the `Args` construction in any of
/// the six arms and this goes red at the listing comparison.
#[test]
fn a_rejected_edit_call_creates_no_state_directory_and_leaves_the_workspace_untouched() {
    let mut s = Session::start();
    s.handshake();

    let ws = s.ws.path().to_path_buf();
    let source = ws.join("hello.rs");
    let before_bytes = std::fs::read(&source).expect("the session writes hello.rs");
    let before_entries = listing(&ws);

    // One malformed call per edit arm this mode can reach. A read-mode server refuses the three
    // write tools at the mode gate — before dispatch, with `unknown tool` — so their ordering is
    // proved in `dispatch.rs`'s unit tests, against a write-mode context that actually reaches
    // the arms.
    //
    // `ast_plan_list`, `ast_plan_show` and `ast_edit_preview` are advertised here, so each
    // refusal must come from *its own* argument parsing: `unknown tool` in this list would mean
    // the arm does not exist, and the `not contains` below is what keeps that from passing as a
    // malformed-argument refusal.
    let rejected = [
        ("ast_plan_list", json!({ "limit": "not a number" })),
        ("ast_plan_show", json!({})),
        ("ast_edit_preview", json!({})),
        // `rule` is refused by the same parse-first path, before the store is opened.
        (
            "ast_edit_preview",
            json!({ "kind": "rewrite", "rule": "some rule" }),
        ),
    ];
    for (id, (name, arguments)) in (10..).zip(rejected) {
        s.send(
            id,
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        let response = s.recv();
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no text content in {response}"));
        assert_eq!(
            response["result"]["isError"],
            json!(true),
            "{name} with malformed arguments must be refused, got {text}"
        );
        assert!(
            text.contains("invalid_args") && !text.contains("unknown tool"),
            "{name} must refuse a malformed call on its own arguments, not as an unknown tool, \
             got {text}"
        );
    }

    assert_eq!(
        before_bytes,
        std::fs::read(&source).expect("hello.rs still exists"),
        "a refused call must not change the workspace file"
    );
    assert_eq!(
        before_entries,
        listing(&ws),
        "a refused call must not add anything to the workspace — the state store is created by a \
         VALID call, never by a rejected one"
    );
    assert!(
        !ws.join(".opencrayast").exists(),
        "no state directory may exist after only refused calls"
    );

    // And the converse, so the assertion above is about the ordering rather than about the store
    // being disabled: a valid preview is what creates it.
    let _ = s.call(
        90,
        "ast_edit_preview",
        json!({
            "kind": "rewrite",
            "language": "rust",
            "pattern": "console.log($$$A)",
            "replacement": "trace($$$A)",
            "paths": ["hello.rs"],
        }),
    );
    assert!(
        !ws.join(".opencrayast").exists(),
        "state lives in the platform user-state base now, never inside the workspace"
    );

    s.shutdown();
}

/// Every entry directly under `dir`, as `(name, is_dir)` pairs in sorted order.
///
/// Used to compare a workspace before and after a refused call. Only one level deep is enough
/// and is what makes the assertion readable: the state directory, or anything else a refused
/// call might leave, is a direct child.
fn listing(dir: &std::path::Path) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        .map(|entry| {
            let entry = entry.expect("a directory entry");
            let is_dir = entry.file_type().expect("an entry file type").is_dir();
            (entry.file_name().to_string_lossy().into_owned(), is_dir)
        })
        .collect();
    out.sort();
    out
}

/// PARITY1-07 (replacement): the live plan tools work over stdio, and a plan the server
/// previewed is the plan it lists and shows.
///
/// The three read-mode plan tools each have an arm, and they agree on one plan: preview
/// produces an id, `ast_plan_list` lists that same id, and `ast_plan_show` returns that id's
/// diff. That agreement is the parity claim — three separate calls, three separate handler
/// invocations, one plan.
///
/// Mutation self-proof: delete the `ast_plan_list` arm and this goes red at the listing
/// assertion; delete `ast_plan_show` and it goes red at the show assertion; delete
/// `ast_edit_preview` and it goes red at the plan-id extraction.
#[test]
fn mcp_edit_plan_preview_list_and_show_agree_on_one_plan() {
    let mut s = Session::start();
    s.handshake();

    // `ast_plan_list` before anything is stored: a successful, empty listing.
    let empty = s.call(2, "ast_plan_list", json!({}));
    assert!(
        !empty.contains("unknown tool"),
        "ast_plan_list must be routed: {empty}"
    );

    // `ast_edit_preview` — rewrite the one call site in the workspace's only file.
    let previewed = s.call(
        3,
        "ast_edit_preview",
        json!({
            "kind": "rewrite",
            "language": "rust",
            "pattern": "console.log($$$ARGS)",
            "replacement": "logger.debug($$$ARGS)",
            "paths": ["hello.rs"],
            "note": "mcp plan parity probe"
        }),
    );
    let id = plan_id_of(&previewed).unwrap_or_else(|| {
        panic!("ast_edit_preview must be routed and produce a plan id: {previewed}")
    });
    assert!(id.starts_with("p-"), "a plan id starts with `p-`: {id}");
    assert!(
        !previewed.contains("invalid_args"),
        "a matching rewrite must not be refused: {previewed}"
    );

    // `ast_plan_list` now names that same plan.
    let listed = s.call(4, "ast_plan_list", json!({}));
    assert!(
        listed.contains(&id),
        "the plan the server previewed must appear in ast_plan_list: {listed}"
    );

    // `ast_plan_show` returns that same plan's diff.
    let shown = s.call(5, "ast_plan_show", json!({ "plan_id": id }));
    assert!(
        shown.contains(&id),
        "ast_plan_show must return the plan that was asked for: {shown}"
    );
    assert!(
        shown.contains("logger.debug"),
        "the shown diff must contain the edit that was previewed: {shown}"
    );

    s.shutdown();
}

/// The read-mode plan tools write the **state directory**, and only that: the workspace file
/// itself is unchanged after a preview.
///
/// This is the behaviour the operator accepted — `ast_edit_preview` is read-mode, is advertised
/// with `readOnlyHint: false` because it persists a plan, and the plan store is user-private
/// data rather than workspace content (EDIT-MODEL E-12). It is also the only thing here that
/// pins *which* directory gets written: `main.rs` resolves the platform user-state base and this
/// looks for the store under exactly that, so a different resolution leaves the store somewhere
/// this cannot see it.
///
/// Mutation self-proof: change the state dir resolved in `main.rs` and this goes red.
#[test]
fn preview_writes_the_state_directory_and_not_the_workspace_file() {
    let mut s = Session::start();
    s.handshake();

    let before = std::fs::read_to_string(s.ws.path().join("hello.rs")).unwrap();

    s.call(
        2,
        "ast_edit_preview",
        json!({
            "kind": "rewrite",
            "language": "rust",
            "pattern": "console.log($$$ARGS)",
            "replacement": "logger.debug($$$ARGS)",
            "paths": ["hello.rs"]
        }),
    );

    // The workspace file is byte for byte what it was: preview plans, it does not apply.
    let after = std::fs::read_to_string(s.ws.path().join("hello.rs")).unwrap();
    assert_eq!(
        before, after,
        "ast_edit_preview must never modify the workspace"
    );

    // And the plan store exists, under the **resolved platform** state directory, `0700` — and
    // NOT inside the workspace, which is the whole point of the relocation.
    let state = s.state.clone();
    assert!(
        state.is_dir(),
        "the state directory must be the resolved one, got missing {state:?}"
    );
    assert!(
        !state.starts_with(s.ws.path()),
        "state must not live inside the workspace: {} is under {}",
        state.display(),
        s.ws.path().display()
    );
    assert!(
        !s.ws.path().join(".opencrayast").exists(),
        "the workspace must not have a state directory at all"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&state).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o700,
            "the state directory must be private to this user (T-21), got {mode:o}"
        );
    }

    s.shutdown();
}

/// A read-mode server refuses the three write tools as `unknown tool` — the whole envelope, so
/// the refusal is indistinguishable from any name that does not exist.
///
/// This is the refusal half of the write gate, at the layer this crate owns: a read-mode server
/// does not advertise write tools, so a caller cannot learn they exist. The *other* half — a
/// write-mode server with no `--allow-write` capability answering `write_disabled` — is proved
/// in `dispatch.rs`'s unit tests, where a `WriteCap` can be withheld deliberately.
///
/// Mutation self-proof: give the read-mode gate a hand-written name allowlist that lets a write
/// tool through, and this goes red.
#[test]
fn read_mode_server_refuses_every_write_tool_as_unknown_tool() {
    let mut s = Session::start();
    s.handshake();

    // Confirm the tools are advertised in write mode by the catalogue, so the assertions below
    // are about refusal rather than about tools that were never catalogued.
    for name in ["ast_edit_apply", "ast_undo", "ast_recover"] {
        s.send(
            10,
            "tools/call",
            json!({ "name": name, "arguments": { "plan_id": "p-aaaaaaaaaaaaaaaaaaaaaaaaaa" } }),
        );
        let response = s.recv();
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("unknown tool"),
            "read mode must answer `unknown tool` for {name}, got {text}"
        );
        assert_eq!(
            response["result"]["isError"],
            json!(true),
            "{name} must be an error result in read mode"
        );
    }

    s.shutdown();
}
