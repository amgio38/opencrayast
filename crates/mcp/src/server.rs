//! Request loop: read → validate → dispatch → write. Logging stays on stderr.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use opencrayast_tools::{Mode, ToolContext, tools_for_mode};
use serde_json::{Value, json};

use crate::dispatch::call_tool;
use crate::rpc::{
    Fault, INVALID_REQUEST, Incoming, METHOD_NOT_FOUND, PARSE_ERROR, error_response,
    parse_incoming, result_response,
};
use crate::transport::{ReadOutcome, WriteOutcome, read_message, write_message};
use crate::{PROTOCOL_VERSION, SERVER_NAME};

/// MCP revision codes we will echo when the client asks for one of them.
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// JSON-RPC application error: tools used before `notifications/initialized`.
const NOT_INITIALIZED: i64 = -32002;

/// Model-facing guidance in `initialize.instructions` (kept short).
pub const SERVER_INSTRUCTIONS: &str = "\
Use ast_info first to see mode and limits. Prefer ast_outline / ast_get / \
ast_search over reading whole files. Write tools (ast_edit_apply, ast_undo, \
ast_recover) appear only when the server is started with --allow-write. \
Tool failures return isError with a [code] and a Next: step.";

/// What [`serve`] needs besides the stdio streams.
pub struct ServerConfig {
    /// Shared tool environment (boundary, limits, mode).
    pub ctx: ToolContext,
    /// `serverInfo.version` (usually `CARGO_PKG_VERSION`).
    pub version: String,
    /// The state directory the plan store, journal store and apply lock live under.
    ///
    /// Carried here rather than on [`ToolContext`]: `ToolContext` is a public cross-crate
    /// interface, and adding a required field would break every constructor outside this
    /// workspace. `EditTools` borrows the read context and adds the three store handles, so
    /// nothing has to widen `ToolContext` for the edit tools to work.
    pub state_dir: PathBuf,
}

/// Serve until stdin EOF or stdout closes. Returns `Ok(())` on a clean stop
/// (invariant 7 / EPIPE failure-table row).
pub fn serve<R: BufRead, W: Write>(
    cfg: &ServerConfig,
    input: &mut R,
    output: &mut W,
) -> io::Result<()> {
    let mut initialized = false;
    loop {
        match read_message(input)? {
            ReadOutcome::Eof => return Ok(()),
            ReadOutcome::TooLarge => {
                // Cap exceeded: answer with a JSON-RPC error, never panic / OOM.
                if !emit(
                    output,
                    &error_response(Value::Null, INVALID_REQUEST, "message too large"),
                )? {
                    return Ok(());
                }
            }
            ReadOutcome::Incomplete => {
                // Half-frame at EOF: refuse, then the next read yields Eof.
                if !emit(
                    output,
                    &error_response(
                        Value::Null,
                        INVALID_REQUEST,
                        "incomplete message (EOF before newline)",
                    ),
                )? {
                    return Ok(());
                }
            }
            ReadOutcome::Message(bytes) => {
                if bytes.is_empty() {
                    continue;
                }
                match parse_incoming(&bytes) {
                    Err(Fault::Parse) => {
                        if !emit(
                            output,
                            &error_response(Value::Null, PARSE_ERROR, "parse error"),
                        )? {
                            return Ok(());
                        }
                    }
                    Err(Fault::Invalid(id)) => {
                        if !emit(
                            output,
                            &error_response(
                                id.unwrap_or(Value::Null),
                                INVALID_REQUEST,
                                "invalid request",
                            ),
                        )? {
                            return Ok(());
                        }
                    }
                    Ok(Incoming::Notification { method, params }) => {
                        handle_notification(&method, &params, &mut initialized);
                    }
                    Ok(Incoming::Request { id, method, params }) => {
                        let line = handle_request(cfg, &mut initialized, id, &method, &params);
                        if !emit(output, &line)? {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
}

/// Write one response line. `Ok(false)` means stdout closed — stop without error.
fn emit<W: Write>(out: &mut W, line: &str) -> io::Result<bool> {
    match write_message(out, line)? {
        WriteOutcome::Written => Ok(true),
        WriteOutcome::Closed => Ok(false),
    }
}

fn handle_notification(method: &str, params: &Value, initialized: &mut bool) {
    match method {
        "notifications/initialized" => {
            *initialized = true;
        }
        // Cancellation is silent when we add concurrency; for now ignore unknowns.
        "notifications/cancelled" => {
            let _request_id = params.get("requestId");
        }
        other => {
            // Protocol: notifications never get a reply. Log to stderr only.
            eprintln!("opencrayast-mcp: ignoring notification `{other}`");
        }
    }
}

fn handle_request(
    cfg: &ServerConfig,
    initialized: &mut bool,
    id: Value,
    method: &str,
    params: &Value,
) -> String {
    match method {
        "initialize" => initialize_response(cfg, id, params),
        "ping" => result_response(id, json!({})),
        "tools/list" | "tools/call" if !*initialized => {
            error_response(id, NOT_INITIALIZED, "server not initialized")
        }
        "tools/list" => list_tools(cfg, id),
        "tools/call" => call_tool(&cfg.ctx, &cfg.state_dir, id, params),
        _ => error_response(id, METHOD_NOT_FOUND, &format!("method not found: {method}")),
    }
}

fn initialize_response(cfg: &ServerConfig, id: Value, params: &Value) -> String {
    let requested = params
        .as_object()
        .and_then(|m| m.get("protocolVersion"))
        .and_then(Value::as_str);
    let negotiated = negotiate(requested);
    result_response(
        id,
        json!({
            "protocolVersion": negotiated,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": SERVER_NAME, "version": cfg.version },
            "instructions": SERVER_INSTRUCTIONS,
        }),
    )
}

fn negotiate(requested: Option<&str>) -> &'static str {
    for &v in SUPPORTED_PROTOCOL_VERSIONS {
        if requested == Some(v) {
            return v;
        }
    }
    PROTOCOL_VERSION
}

fn list_tools(cfg: &ServerConfig, id: Value) -> String {
    let tools: Vec<Value> = tools_for_mode(cfg.ctx.mode)
        .map(|t| {
            let schema: Value = serde_json::from_str(t.input_schema).unwrap_or_else(|_| {
                // Schema constants are compile-time; a bad one must not panic the server.
                eprintln!(
                    "opencrayast-mcp: bad input_schema for `{}`; using empty object",
                    t.name
                );
                json!({"type": "object", "properties": {}})
            });
            json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": schema,
                "annotations": {
                    "readOnlyHint": t.annotations.read_only_hint,
                    "destructiveHint": t.annotations.destructive_hint,
                    "idempotentHint": t.annotations.idempotent_hint,
                    "openWorldHint": t.annotations.open_world_hint,
                }
            })
        })
        .collect();
    // Listed tools come from tools_for_mode: each entry's ToolEntry.mode must be
    // compatible with the server mode. Do **not** assert readOnlyHint==true —
    // TOOLS.md: ast_edit_preview is read-mode with readOnlyHint=false.
    debug_assert!(tools_for_mode(cfg.ctx.mode).all(|t| match cfg.ctx.mode {
        Mode::Write => true,
        Mode::ReadOnly => t.mode == Mode::ReadOnly,
    }));
    result_response(id, json!({ "tools": tools }))
}

/// Build a catalogue entry for tests that inspect list shape without stdio.
#[cfg(test)]
pub(crate) fn list_tool_names(mode: Mode) -> Vec<&'static str> {
    tools_for_mode(mode).map(|t| t.name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_list_only_exposes_read_mode_entries() {
        let names = list_tool_names(Mode::ReadOnly);
        assert!(!names.is_empty());
        for name in &names {
            let entry = opencrayast_tools::find_tool(name).expect("listed tool must be registered");
            assert_eq!(
                entry.mode,
                Mode::ReadOnly,
                "`{name}` listed in read mode must have ToolEntry.mode == ReadOnly"
            );
        }
    }
}
