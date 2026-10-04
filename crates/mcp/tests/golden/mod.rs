//! Shared harness for the MCP golden-transcript system (`MCP-08`).
//!
//! # What a transcript is
//!
//! A transcript is a real, recorded request/response exchange with the **shipped
//! `opencrayast-mcp` binary** over stdio, stored as a reviewable text file under
//! `tests/golden/transcripts/`. Nothing in it is hand-written: [`capture`] writes
//! every byte from the server's own stdout.
//!
//! # File format
//!
//! ```text
//! # golden-transcript v1
//! # case: <id>
//! # covers: <what this transcript pins>
//! # responses: <count>
//! # ... (regeneration instructions) ...
//! > <client line, verbatim, sent after the handshake>
//! {"jsonrpc":"2.0","id":2,...}      <- server line, verbatim
//! ```
//!
//! `#` lines are header metadata, `> ` marks a client line, and every other
//! non-blank line is a server response **exactly as the server wrote it**.
//!
//! # Normalisation
//!
//! Replay asserts **byte equality on the whole file**, not merely on the response
//! lines: the header, the `# responses:` count, the client lines and the fixed
//! regeneration instructions are reconstructed from the case and compared too
//! (see [`parse_strict`] and [`check`]). Exactly one thing is normalised, by the
//! same rule on both sides so the comparison stays meaningful:
//!
//! * `(id w-<32 hex>)` → `(id <WORKSPACE_ID>)`. `ast_info` prints a workspace
//!   identity derived from the absolute path of the workspace root. Recording and
//!   replay each run against a **freshly created temporary directory**, so that hex
//!   differs on every run by construction and cannot be recorded literally.
//!
//! Nothing else is normalised. No timestamps, durations, pids or absolute paths
//! appear in the recorded files. If a response stops being byte-stable, the
//! transcript is re-recorded — the comparison is never quietly loosened.
//!
//! # Why the whole file, and not just the responses
//!
//! The parser recovers four fields (`case`, `covers`, the `> ` client lines and
//! the response lines) and skips everything else, so comparing only what it
//! returns leaves the rest of the file unverified. An early version of this
//! harness did exactly that, and editing a `#` header — including the very error
//! code the transcript exists to pin — left the suite green.
//!
//! [`parse_strict`] closes the gap by rebuilding the entire file from the parsed
//! fields and comparing it to the original, so a drift in any byte is reported
//! with its line number. [`self_test`] proves it by corrupting temporary copies
//! of the real transcripts and asserting each corruption is caught.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

/// The placeholder both sides use for the path-derived workspace identity.
const WORKSPACE_ID: &str = "<WORKSPACE_ID>";

/// The exact first line a transcript file must start with.
///
/// Held as a constant so [`render`] and [`parse_strict`] cannot drift apart: if
/// the format version is bumped, the parser fails loudly on every committed
/// transcript instead of quietly accepting a file it no longer understands.
const FORMAT_HEADER: &str = "# golden-transcript v1";

/// Directory the committed transcripts live in, relative to the crate manifest.
pub const TRANSCRIPT_SUBDIR: &str = "tests/golden/transcripts";

// ======================================================================================
// Workspace fixtures
// ======================================================================================

/// The files a transcript runs against, written into a fresh temp dir per run.
///
/// Because the workspace is always a fresh temp dir, no recorded line can contain
/// a machine-specific absolute path.
pub struct Fixture {
    /// `(relative path, UTF-8 contents)`.
    pub files: &'static [(&'static str, &'static str)],
    /// `(relative path, raw bytes)` for the non-UTF-8 case.
    pub binary_files: &'static [(&'static str, &'static [u8])],
    /// Optional `oc-config.toml` body, written `0600` as `Settings::load` requires.
    pub config: Option<&'static str>,
}

/// One Rust file plus one file with no grammar.
const DEFAULT_FILES: &[(&str, &str)] = &[
    (
        "hello.rs",
        "fn helper() -> i32 { 1 }\n\nfn main() { let _ = helper(); }\n",
    ),
    ("notes.txt", "plain text, no grammar\n"),
];

/// Default workspace.
pub const FIX_DEFAULT: Fixture = Fixture {
    files: DEFAULT_FILES,
    binary_files: &[],
    config: None,
};

/// Two files defining `dup`, so `ast_get` has something to be ambiguous about.
pub const FIX_AMBIGUOUS: Fixture = Fixture {
    files: &[
        ("hello.rs", "fn helper() -> i32 { 1 }\n"),
        ("a.rs", "fn dup() -> i32 { 1 }\n"),
        ("d2/a.rs", "fn dup() -> i32 { 1 }\n"),
    ],
    binary_files: &[],
    config: None,
};

/// A file of 32 non-UTF-8 bytes (every byte >= 0x80).
pub const FIX_NOT_UTF8: Fixture = Fixture {
    files: &[],
    binary_files: &[("binary.rs", &[0x80u8; 32])],
    config: None,
};

/// A 65-byte file against a configured 64-byte `max_file_bytes`: the
/// `file_too_large` message is small and fully deterministic. (The default 4 MiB
/// limit would need a 4 MiB fixture, and the reported byte count would then be the
/// thing most likely to drift.)
pub const FIX_SMALL_LIMIT: Fixture = Fixture {
    files: &[("hello.rs", "fn helper() -> i32 { 1 }\n")],
    binary_files: &[("over.rs", &[b'x'; 65])],
    config: Some("[limits]\nmax_file_bytes = 64\n"),
};

// ======================================================================================
// Case list
// ======================================================================================

/// One client message and whether the server answers it.
pub struct Step {
    pub line: String,
    pub expect_reply: bool,
}

/// One recorded exchange.
pub struct Case {
    /// Stable file stem; also the `case:` header.
    pub id: String,
    /// What the transcript pins, recorded in the `covers:` header.
    pub covers: String,
    /// Workspace fixture.
    pub fixture: &'static Fixture,
    /// Extra CLI arguments after `--workspace <dir>`.
    pub args: &'static [&'static str],
    /// Whether to send `initialize` + `notifications/initialized` first.
    pub handshake: bool,
    /// Client lines sent after the handshake.
    pub sends: Vec<Step>,
}

/// A `tools/call` for `name` with `args`.
fn call(name: &str, args: &str) -> Step {
    Step {
        line: format!(
            r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"{name}","arguments":{args}}}}}"#
        ),
        expect_reply: true,
    }
}

/// A request for `method` that must be answered.
fn req(id: &str, method: &str, params: &str) -> Step {
    Step {
        line: format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{params}}}"#),
        expect_reply: true,
    }
}

/// A notification, which by protocol is never answered.
fn note(method: &str, params: &str) -> Step {
    Step {
        line: format!(r#"{{"jsonrpc":"2.0","method":"{method}","params":{params}}}"#),
        expect_reply: false,
    }
}

fn case(id: &str, covers: &str, fixture: &'static Fixture, sends: Vec<Step>) -> Case {
    Case {
        id: id.to_string(),
        covers: covers.to_string(),
        fixture,
        args: &[],
        handshake: true,
        sends,
    }
}

/// Every golden case, built once.
pub fn cases() -> &'static [Case] {
    static CASES: OnceLock<Vec<Case>> = OnceLock::new();
    CASES.get_or_init(|| {
        let mut v = vec![
            // ---- protocol ----------------------------------------------------------
            case(
                "protocol_tools_list",
                "tools/list catalogue in read-only mode",
                &FIX_DEFAULT,
                vec![req("2", "tools/list", "{}")],
            ),
            case(
                "protocol_ping",
                "ping",
                &FIX_DEFAULT,
                vec![req("2", "ping", "{}")],
            ),
            case(
                "protocol_initialize_negotiation",
                "initialize falls back to PROTOCOL_VERSION for an unknown requested version",
                &FIX_DEFAULT,
                vec![req(
                    "2",
                    "initialize",
                    r#"{"protocolVersion":"1999-01-01","capabilities":{},"clientInfo":{"name":"golden","version":"0"}}"#,
                )],
            ),
            case(
                "protocol_notification_no_reply",
                "a notification is never answered",
                &FIX_DEFAULT,
                vec![note("notifications/cancelled", r#"{"requestId":2}"#)],
            ),
            Case {
                id: "protocol_not_initialized".to_string(),
                covers: "error code -32002 (tools used before notifications/initialized)"
                    .to_string(),
                fixture: &FIX_DEFAULT,
                args: &[],
                handshake: false,
                sends: vec![req("2", "tools/list", "{}"), req("3", "tools/call",
                    r#"{"name":"ast_info","arguments":{}}"#)],
            },
            // ---- framing / parse layer errors ----------------------------------------
            case(
                "error_jsonrpc_parse_error",
                "error code -32700 (parse error)",
                &FIX_DEFAULT,
                vec![Step {
                    line: "this line is not JSON".to_string(),
                    expect_reply: true,
                }],
            ),
            case(
                "error_jsonrpc_invalid_request_batch",
                "error code -32600 (invalid request: JSON array / batching)",
                &FIX_DEFAULT,
                vec![Step {
                    line: r#"[{"jsonrpc":"2.0","id":2,"method":"ping"}]"#.to_string(),
                    expect_reply: true,
                }],
            ),
            case(
                "error_jsonrpc_invalid_request_no_version",
                "error code -32600 (invalid request: missing jsonrpc field)",
                &FIX_DEFAULT,
                vec![Step {
                    line: r#"{"id":2,"method":"ping"}"#.to_string(),
                    expect_reply: true,
                }],
            ),
            case(
                "error_jsonrpc_invalid_request_null_id",
                "error code -32600 (invalid request: explicit null id)",
                &FIX_DEFAULT,
                vec![Step {
                    line: r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#.to_string(),
                    expect_reply: true,
                }],
            ),
            case(
                "error_jsonrpc_method_not_found",
                "error code -32601 (method not found)",
                &FIX_DEFAULT,
                vec![req("2", "no/such/method", "{}")],
            ),
            case(
                "error_jsonrpc_invalid_params_params_not_object",
                "error code -32602 (params must be an object)",
                &FIX_DEFAULT,
                vec![Step {
                    line: r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":[1,2]}"#
                        .to_string(),
                    expect_reply: true,
                }],
            ),
            case(
                "error_jsonrpc_invalid_params_missing_name",
                "error code -32602 (missing params.name)",
                &FIX_DEFAULT,
                vec![req("2", "tools/call", "{}")],
            ),
            case(
                "error_jsonrpc_invalid_params_arguments_not_object",
                "error code -32602 (arguments must be an object)",
                &FIX_DEFAULT,
                vec![Step {
                    line: r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"ast_info","arguments":[1,2]}}"#
                        .to_string(),
                    expect_reply: true,
                }],
            ),
            // ---- happy path, one per tool the dispatch layer routes ------------------
            case(
                "tool_ast_info",
                "tool ast_info (happy path)",
                &FIX_DEFAULT,
                vec![call("ast_info", "{}")],
            ),
            case(
                "tool_ast_outline",
                "tool ast_outline (happy path, single file)",
                &FIX_DEFAULT,
                vec![call("ast_outline", r#"{"path":"hello.rs"}"#)],
            ),
            case(
                "tool_ast_outline_directory",
                "tool ast_outline (happy path, directory walk)",
                &FIX_DEFAULT,
                vec![call("ast_outline", r#"{"path":"."}"#)],
            ),
            case(
                "tool_ast_get",
                "tool ast_get (happy path)",
                &FIX_DEFAULT,
                vec![call("ast_get", r#"{"symbol":"helper","path":"hello.rs"}"#)],
            ),
            case(
                "tool_ast_search",
                "tool ast_search (happy path, one match)",
                &FIX_DEFAULT,
                vec![call(
                    "ast_search",
                    r#"{"pattern":"fn $NAME() -> $T { $BODY }","paths":["hello.rs"]}"#,
                )],
            ),
            case(
                "tool_ast_search_zero_matches",
                "tool ast_search (zero matches is a successful result, not an error)",
                &FIX_DEFAULT,
                vec![call(
                    "ast_search",
                    r#"{"pattern":"fn nosuch() -> $T { $BODY }","paths":["hello.rs"]}"#,
                )],
            ),
            case(
                "tool_ast_explain_pattern",
                "tool ast_explain_pattern (happy path)",
                &FIX_DEFAULT,
                vec![call(
                    "ast_explain_pattern",
                    r#"{"pattern":"fn $NAME() -> $T { $BODY }","language":"rust"}"#,
                )],
            ),
            // ---- the plan tools: the three arms wired after ISSUE-MCP-CATALOGUE -------
            //
            // `ast_edit_preview`'s happy path is deliberately NOT here, and the reason is
            // recorded rather than worked around: its output carries `(expires HH:MM UTC)`,
            // which is wall-clock. A transcript asserting it byte for byte would go red at
            // every hour boundary — and a golden file that must be re-recorded on a clock is
            // not pinning the protocol, it is pinning the time of day. The behaviour is
            // covered behaviourally instead, in `mcp_edit_plan_spec.rs`, which is where a
            // clock-dependent claim belongs.
            //
            // What IS deterministic, and therefore belongs here, is the refusal half: each
            // of the three tools' own argument rules, answered byte for byte.
            case(
                "error_tool_plan_not_found",
                "error code plan_not_found (ast_plan_show with an id that does not exist)",
                &FIX_DEFAULT,
                vec![call(
                    "ast_plan_show",
                    r#"{"plan_id":"p-aaaaaaaaaaaaaaaaaaaaaaaaaa"}"#,
                )],
            ),
            case(
                "error_tool_plan_show_rejects_a_short_id",
                "error code invalid_args (ast_plan_show refuses an id too short to be unambiguous)",
                &FIX_DEFAULT,
                vec![call("ast_plan_show", r#"{"plan_id":"short"}"#)],
            ),
            case(
                "error_tool_plan_list_rejects_limit_zero",
                "error code invalid_args (ast_plan_list enforces its own limit range)",
                &FIX_DEFAULT,
                vec![call("ast_plan_list", r#"{"limit":0}"#)],
            ),
            case(
                "tool_write_refused_in_read_mode",
                "MCP-02: every write tool is refused in read-only mode, as unknown tool",
                &FIX_DEFAULT,
                vec![
                    call("ast_edit_apply", r#"{"plan_id":"p-0000000000"}"#),
                    call("ast_undo", r#"{"plan_id":"p-0000000000"}"#),
                    call("ast_recover", "{}"),
                ],
            ),
            // ---- tool-level error codes ----------------------------------------------
            case(
                "error_tool_invalid_args_missing_path",
                "error code invalid_args (missing required argument)",
                &FIX_DEFAULT,
                vec![call("ast_outline", "{}")],
            ),
            case(
                "error_tool_invalid_args_wrong_type",
                "error code invalid_args (mistyped argument)",
                &FIX_DEFAULT,
                vec![call("ast_outline", r#"{"path":"hello.rs","depth":"two"}"#)],
            ),
            case(
                "error_tool_invalid_args_limit_out_of_range",
                "error code invalid_args (out-of-range argument)",
                &FIX_DEFAULT,
                vec![call("ast_outline", r#"{"path":"hello.rs","limit":99999}"#)],
            ),
            case(
                "error_tool_invalid_args_unknown_tool",
                "error code invalid_args (unknown tool name)",
                &FIX_DEFAULT,
                vec![call("ast_no_such_tool", "{}")],
            ),
            case(
                "error_tool_invalid_args_unknown_language",
                "error code invalid_args (unknown language id)",
                &FIX_DEFAULT,
                vec![call(
                    "ast_explain_pattern",
                    r#"{"pattern":"fn $N() { $B }","language":"cobol"}"#,
                )],
            ),
            case(
                "error_tool_invalid_args_search_rule_rejected",
                "error code invalid_args (rule is refused on the MCP tools/call path)",
                &FIX_DEFAULT,
                vec![call(
                    "ast_search",
                    r#"{"pattern":"fn $N() { $B }","paths":["hello.rs"],"rule":{"kind":"fn"}}"#,
                )],
            ),
            case(
                "error_tool_not_found_path",
                "error code not_found (no such file)",
                &FIX_DEFAULT,
                vec![call("ast_outline", r#"{"path":"missing.rs"}"#)],
            ),
            case(
                "error_tool_not_found_symbol",
                "error code not_found (no such symbol)",
                &FIX_DEFAULT,
                vec![call("ast_get", r#"{"symbol":"nosuchfn"}"#)],
            ),
            case(
                "error_tool_outside_workspace",
                "error code outside_workspace",
                &FIX_DEFAULT,
                vec![call("ast_outline", r#"{"path":"../etc"}"#)],
            ),
            case(
                "error_tool_unsupported_language",
                "error code unsupported_language",
                &FIX_DEFAULT,
                vec![call("ast_outline", r#"{"path":"notes.txt"}"#)],
            ),
            case(
                "error_tool_not_utf8",
                "error code not_utf8",
                &FIX_NOT_UTF8,
                vec![call("ast_outline", r#"{"path":"binary.rs"}"#)],
            ),
            case(
                "error_tool_file_too_large",
                "error code file_too_large",
                &FIX_SMALL_LIMIT,
                vec![call("ast_outline", r#"{"path":"over.rs"}"#)],
            ),
            case(
                "error_tool_invalid_pattern",
                "error code invalid_pattern",
                &FIX_DEFAULT,
                vec![call("ast_search", r#"{"pattern":"fn (((","paths":["hello.rs"]}"#)],
            ),
            case(
                "error_tool_ambiguous",
                "error code ambiguous",
                &FIX_AMBIGUOUS,
                vec![call("ast_get", r#"{"symbol":"dup"}"#)],
            ),
        ];
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    })
}

/// Every case id, sorted.
pub fn case_ids() -> Vec<&'static str> {
    let mut ids: Vec<&'static str> = cases().iter().map(|c| c.id.as_str()).collect();
    ids.sort_unstable();
    ids
}

// ======================================================================================
// Driving the real binary
// ======================================================================================

/// A temp workspace that deletes itself.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Create a fresh temp dir. The name is pid + a per-process counter: unique
    /// within a run and free of any random or machine-specific component.
    pub fn new() -> std::io::Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("oc-mcp-golden-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn write_fixture(ws: &Path, fixture: &'static Fixture) -> std::io::Result<()> {
    for (rel, body) in fixture.files {
        let path = ws.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, body)?;
    }
    for (rel, bytes) in fixture.binary_files {
        let path = ws.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, bytes)?;
    }
    Ok(())
}

/// A live stdio session against the shipped binary.
pub struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    /// Alive for the whole session: the server reads these files.
    ws: TempDir,
}

impl Session {
    pub fn start(fixture: &'static Fixture, args: &[&str]) -> std::io::Result<Self> {
        let ws = TempDir::new()?;
        write_fixture(ws.path(), fixture)?;

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_opencrayast-mcp"));
        cmd.arg("--workspace")
            .arg(ws.path())
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(config) = fixture.config {
            let path = ws.path().join("oc-config.toml");
            std::fs::write(&path, config)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            }
            cmd.arg("--config").arg(&path);
        }

        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
        Ok(Self {
            child,
            stdin,
            stdout,
            ws,
        })
    }

    /// Keep the workspace alive after the streams are dropped (used by shutdown).
    pub fn keep_alive(self) -> TempDir {
        drop(self.stdin);
        drop(self.stdout);
        let mut child = self.child;
        for _ in 0..300 {
            match child.try_wait() {
                Ok(Some(status)) => {
                    assert!(
                        status.success(),
                        "server exited with {status} after stdin EOF"
                    );
                    return self.ws;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(e) => panic!("wait on server: {e}"),
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!("server did not exit after stdin EOF");
    }

    pub fn raw(&mut self, line: &str) -> std::io::Result<()> {
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()
    }

    /// One response line, without its trailing newline.
    pub fn recv(&mut self) -> std::io::Result<String> {
        let mut line = String::new();
        let n = self.stdout.read_line(&mut line)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "server closed stdout before answering",
            ));
        }
        Ok(line.trim_end_matches(['\r', '\n']).to_string())
    }
}

/// The handshake lines, sent first unless `case.handshake` is false.
const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"golden","version":"0"}}}"#;
const INITIALIZED: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#;

/// Replay `case` against the real server; return the normalised response lines.
pub fn capture(case: &Case) -> Result<Vec<String>, String> {
    let mut s = Session::start(case.fixture, case.args).map_err(|e| format!("spawn: {e}"))?;
    let mut out = Vec::new();
    if case.handshake {
        s.raw(INITIALIZE).map_err(|e| format!("initialize: {e}"))?;
        s.recv().map_err(|e| format!("initialize reply: {e}"))?;
        s.raw(INITIALIZED)
            .map_err(|e| format!("initialized: {e}"))?;
    }
    for step in &case.sends {
        s.raw(&step.line)
            .map_err(|e| format!("send {}: {e}", step.line))?;
        if step.expect_reply {
            let reply = s
                .recv()
                .map_err(|e| format!("reply to {}: {e}", step.line))?;
            out.push(normalise(&reply));
        }
    }
    let _ws = s.keep_alive();
    Ok(out)
}

// ======================================================================================
// Normalisation, rendering, parsing
// ======================================================================================

/// Replace the path-derived workspace identity with a stable placeholder.
///
/// Line endings are also folded to `\n` so a Windows checkout that rewrote the
/// committed LF transcripts to CRLF still compares equal to `render` (which always
/// emits `\n`). The workspace-id substitution remains the only *content*
/// normalisation; see the module docs.
pub fn normalise(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let marker = "(id w-";
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(pos) = rest.find(marker) {
        out.push_str(&rest[..pos]);
        out.push_str("(id ");
        out.push_str(WORKSPACE_ID);
        rest = &rest[pos + marker.len()..];
        let hex_end = rest
            .find(|c: char| !c.is_ascii_hexdigit())
            .unwrap_or(rest.len());
        rest = &rest[hex_end..];
        // Emit the closing paren we are consuming, so `(id w-abc)` becomes
        // `(id <WORKSPACE_ID>)` and not a truncated `(id <WORKSPACE_ID>`.
        if let Some(stripped) = rest.strip_prefix(')') {
            out.push(')');
            rest = stripped;
        }
    }
    out.push_str(rest);
    out
}

/// Serialise a transcript in the documented format.
pub fn render(case: &Case, responses: &[String]) -> String {
    let mut out = String::new();
    out.push_str(FORMAT_HEADER);
    out.push('\n');
    out.push_str(&format!("# case: {}\n", case.id));
    out.push_str(&format!("# covers: {}\n", case.covers));
    out.push_str(&format!("# responses: {}\n", responses.len()));
    out.push_str("#\n");
    out.push_str("# Recorded from the shipped opencrayast-mcp binary; replayed byte for byte.\n");
    out.push_str("#\n");
    out.push_str(
        "# The only normalisation is (id w-<hex>) -> (id <WORKSPACE_ID>): that identity\n",
    );
    out.push_str("# is derived from the absolute path of the temporary workspace, so it differs\n");
    out.push_str("# on every run by construction. Everything else is asserted exactly.\n");
    out.push_str("#\n");
    out.push_str("# Regenerate every transcript (overwrites this directory):\n");
    out.push_str("#   cargo test -p opencrayast-mcp --test mcp8_golden -- --ignored record\n");
    out.push_str("# Read the diff, then commit it: a change here is a change to the protocol.\n");
    for step in &case.sends {
        out.push_str(&format!("> {}\n", step.line));
    }
    for r in responses {
        out.push_str(r);
        out.push('\n');
    }
    out
}

/// A parsed transcript.
pub struct Transcript {
    pub id: String,
    pub covers: String,
    pub sends: Vec<String>,
    pub responses: Vec<String>,
}

/// Parse a transcript file.
///
/// Deliberately **permissive**: it recovers the four documented fields and skips
/// anything else. It is not the gate — [`parse_strict`] is. Keeping this one
/// permissive matters because the corpus of malformed inputs is exactly what the
/// self-tests feed it; a `parse` that panicked on unknown shapes would make
/// those tests untestable.
///
/// The consequence, spelled out so it is not a trap: **never compare a file
/// against `parse(&file)` to prove it was read whole.** Equal before and after
/// proves nothing, because `parse` silently ignores what it does not recognise.
/// To assert that a file has not drifted in *any* byte, use `parse_strict`, or
/// [`check`] / [`self_test`], which compare the reconstructed text.
pub fn parse(text: &str) -> Transcript {
    let mut id = String::new();
    let mut covers = String::new();
    let mut sends = Vec::new();
    let mut responses = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# case: ") {
            id = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("# covers: ") {
            covers = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("> ") {
            sends.push(rest.to_string());
        } else if line.trim().is_empty() || line.starts_with('#') {
            // header or blank
        } else {
            responses.push(line.to_string());
        }
    }
    Transcript {
        id,
        covers,
        sends,
        responses,
    }
}

// ======================================================================================
// Strict parsing: the gate that makes every byte of a transcript load-bearing
// ======================================================================================

/// Why a transcript file is not byte-identical to a freshly rendered one.
#[derive(Debug, PartialEq, Eq)]
pub struct Drift {
    /// 1-based line number in the file.
    pub line: usize,
    /// What the committed file has on that line.
    pub recorded: String,
    /// What re-rendering the case produces.
    pub expected: String,
}

impl std::fmt::Display for Drift {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "line {}:\n    recorded: {:?}\n    expected: {:?}",
            self.line, self.recorded, self.expected
        )
    }
}

/// Parse **and** prove the file is byte-identical to `render(case, responses)`.
///
/// [`parse`] recovers four fields and skips the rest, so on its own it cannot see a
/// change to a `#` header, to the declared `responses:` count, or to the fixed
/// regeneration instructions. This function closes that: every line of the file is
/// accounted for, and the whole text is reconstructed from the parsed fields and
/// compared to the original.
///
/// Because the header is reconstructed too, a transcript cannot advertise a
/// different case id or a different `covers:` string than the case it replays,
/// and `# responses:` must equal the number of recorded response lines.
pub fn parse_strict(case: &Case, text: &str) -> Result<Transcript, Drift> {
    // Fold CRLF from a Windows checkout before any byte comparison; `render` always
    // emits `\n`. Content normalisation (workspace id) is applied to response lines
    // inside `capture`, not here.
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let t = parse(&text);

    // The format version line is checked directly, so a bumped format fails loudly
    // here rather than being compared like any other comment.
    let first = text.lines().next().unwrap_or("");
    if first != FORMAT_HEADER {
        return Err(Drift {
            line: 1,
            recorded: first.to_string(),
            expected: FORMAT_HEADER.to_string(),
        });
    }

    // Full-file reconstruction: the only comparison that can see every byte.
    let rebuilt = render(case, &t.responses);
    if rebuilt != text {
        let recorded_lines: Vec<&str> = text.lines().collect();
        let expected_lines: Vec<&str> = rebuilt.lines().collect();
        for i in 0..recorded_lines.len().max(expected_lines.len()) {
            let r = recorded_lines.get(i).copied().unwrap_or("<missing>");
            let e = expected_lines.get(i).copied().unwrap_or("<missing>");
            if r != e {
                return Err(Drift {
                    line: i + 1,
                    recorded: r.to_string(),
                    expected: e.to_string(),
                });
            }
        }
        // Same lines, different bytes: a trailing newline or a lone \r.
        return Err(Drift {
            line: recorded_lines.len(),
            recorded: format!("<{} bytes>", text.len()),
            expected: format!("<{} bytes>", rebuilt.len()),
        });
    }

    Ok(t)
}

/// Replay `case` and compare the whole committed file byte for byte.
///
/// This is the comparison the module docs promise. It is strictly stronger than
/// comparing only the response lines: the header, the declared count, the client
/// lines and the fixed instructions are all checked too.
pub fn check(case: &Case, text: &str) -> Result<(), String> {
    let recorded = parse_strict(case, text).map_err(|d| {
        format!(
            "transcript `{}` is not the text this case renders:\n  {d}",
            case.id
        )
    })?;
    assert_jsonrpc(&recorded.responses, &case.id);

    let live = capture(case).map_err(|e| format!("could not be replayed: {e}"))?;
    if recorded.responses != live {
        return Err(diff(&recorded.responses, &live, &case.id));
    }
    Ok(())
}

/// Directory holding the committed transcripts.
pub fn transcript_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(TRANSCRIPT_SUBDIR)
}

/// One transcript's path.
pub fn transcript_path(id: &str) -> PathBuf {
    transcript_dir().join(format!("{id}.txt"))
}

/// Every response is a JSON-RPC 2.0 message. Catches a truncated recording.
pub fn assert_jsonrpc(responses: &[String], case_id: &str) {
    for (i, r) in responses.iter().enumerate() {
        let v: Value = serde_json::from_str(r)
            .unwrap_or_else(|e| panic!("{case_id}: response {i} is not JSON: {r:?}: {e}"));
        assert_eq!(
            v.get("jsonrpc").and_then(Value::as_str),
            Some("2.0"),
            "{case_id}: response {i} lacks jsonrpc 2.0: {r}"
        );
    }
}

/// A readable diff of two response lists, for a failure message.
pub fn diff(expected: &[String], actual: &[String], case_id: &str) -> String {
    let mut msg = format!("transcript `{case_id}` does not match the live server\n");
    let n = expected.len().max(actual.len());
    for i in 0..n {
        match (expected.get(i), actual.get(i)) {
            (Some(e), Some(a)) if e == a => {}
            (e, a) => {
                msg.push_str(&format!("  line {}:\n", i + 1));
                msg.push_str(&format!("    recorded: {:?}\n", e));
                msg.push_str(&format!("    live    : {:?}\n", a));
            }
        }
    }
    msg
}
