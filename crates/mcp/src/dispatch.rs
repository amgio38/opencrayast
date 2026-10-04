//! `tools/call` → `opencrayast-tools` handlers (no tool logic here).

use opencrayast_core::error::{ErrorCode, ToolError};
use opencrayast_tools::{
    ApplyArgs, ExplainArgs, GetArgs, Mode, OutlineArgs, PlanListArgs, PlanShowArgs, PreviewArgs,
    SearchArgs, ToolContext, ToolEntry, UndoArgs, ast_edit_apply, ast_edit_preview,
    ast_explain_pattern, ast_get, ast_info, ast_outline, ast_plan_list, ast_plan_show, ast_recover,
    ast_search, ast_undo, find_tool, is_write_tool_name,
};
use serde_json::{Map, Value};

use crate::editstate::{edit_tools, stores};
use crate::rpc::{INVALID_PARAMS, error_response, tool_result};

/// The `paths` default `ast_search` documents: the workspace root (`docs/TOOLS.md`, `search.rs`).
const DEFAULT_SEARCH_PATH: &str = ".";

/// Whether a catalogue entry may be invoked under `server` mode.
///
/// Truth source is [`ToolEntry::mode`], never a parallel hand-written name list
/// (CR shape: a check narrower than the action it guards).
pub(crate) fn tool_callable(server: Mode, entry: &ToolEntry) -> bool {
    match server {
        Mode::Write => true,
        Mode::ReadOnly => entry.mode == Mode::ReadOnly,
    }
}

/// The `unknown tool` refusal envelope, with the next step chosen by what is actually true.
///
/// Two different situations produce the same *code*, and they must not produce the same advice:
/// a name this server never had, and a real write tool refused because the server is read-only.
/// Telling a caller to "call tools/list" for the second one teaches it that it hallucinated a
/// name that exists - it did not, the server simply cannot run it. So the advice names the
/// condition instead.
fn unknown_tool(id: Value, name: &str, server: Mode) -> String {
    let next = if server == Mode::Write {
        "Call tools/list for the tools this server exposes."
    } else if is_write_tool_name(name) {
        "This server is read-only and `ast_edit_apply`, `ast_undo` and `ast_recover` are write \
         tools. Call ast_info to see what this server allows; enabling write mode needs BOTH \
         --allow-write on the command line AND policy.allow_write = true in the server's \
         configuration file. Until then the write tools are not listed and cannot be called."
    } else {
        "Call tools/list for the tools this server exposes."
    };
    let err = ToolError::new(
        ErrorCode::InvalidArgs,
        format!("unknown tool `{name}`"),
        next,
    );
    tool_result(id, &err.to_string(), true)
}

/// Run one `tools/call` and return a JSON-RPC response line (no trailing newline).
///
/// `state_dir` is where the edit tools keep their plan and journal stores. It is a separate
/// argument rather than a field of [`ToolContext`] because that struct is a public cross-crate
/// interface: widening it would break every constructor outside this workspace, and the edit
/// handlers already solve the same problem with a borrowed [`EditTools`] on top of it.
pub fn call_tool(
    ctx: &ToolContext,
    state_dir: &std::path::Path,
    id: Value,
    params: &Value,
) -> String {
    let map = match crate::rpc::params_map(params) {
        Ok(m) => m,
        Err(()) => {
            return error_response(id, INVALID_PARAMS, "params must be an object");
        }
    };
    let name = match map.get("name").and_then(Value::as_str) {
        Some(n) => n,
        None => return error_response(id, INVALID_PARAMS, "missing params.name"),
    };
    let arguments = match map.get("arguments") {
        None | Some(Value::Null) => Value::Object(Map::new()),
        Some(v) if v.is_object() => v.clone(),
        Some(_) => {
            return error_response(id, INVALID_PARAMS, "`arguments` must be an object");
        }
    };

    // Catalogue layer (MCP): read-mode refusal of write tools is the *same*
    // unknown-tool error as any other missing name (TOOLS.md, Modes). No
    // hand-written name list. Handler-layer
    // `[write_disabled]` (CLI / in-process) is a different layer — not used here.
    let Some(entry) = find_tool(name) else {
        return unknown_tool(id, name, ctx.mode);
    };
    if !tool_callable(ctx.mode, entry) {
        return unknown_tool(id, name, ctx.mode);
    }

    match run(ctx, state_dir, name, &arguments) {
        Ok(text) => tool_result(id, &text, false),
        Err(e) => tool_result(id, &e.to_string(), true),
    }
}

fn run(
    ctx: &ToolContext,
    state_dir: &std::path::Path,
    name: &str,
    arguments: &Value,
) -> Result<String, ToolError> {
    let args = match arguments {
        Value::Null => Map::new(),
        Value::Object(m) => m.clone(),
        _ => {
            return Err(ToolError::new(
                ErrorCode::InvalidArgs,
                "arguments must be an object",
                "Pass a JSON object of tool arguments.",
            ));
        }
    };
    match name {
        "ast_info" => Ok(ast_info(ctx)),
        "ast_outline" => {
            let outline = OutlineArgs {
                path: require_string(&args, "path")?,
                depth: optional_u64(&args, "depth")?,
                kinds: optional_string_array(&args, "kinds")?,
                include_docs: optional_bool(&args, "include_docs")?,
                limit: optional_u64(&args, "limit")?,
            };
            ast_outline(ctx, &outline)
        }
        "ast_get" => {
            let get = GetArgs {
                symbol: require_string(&args, "symbol")?,
                path: optional_string(&args, "path")?,
                context_lines: optional_u64(&args, "context_lines")?,
                include_doc: optional_bool(&args, "include_doc")?,
            };
            ast_get(ctx, &get)
        }
        "ast_search" => {
            // `rule` must not be silently dropped (CR R2 ④): omit it or get invalid_args.
            match args.get("rule") {
                Some(v) if !v.is_null() => {
                    return Err(ToolError::new(
                        ErrorCode::InvalidArgs,
                        "`rule` is not accepted on the MCP tools/call path",
                        "Omit `rule` from this call; no other tool here takes it.",
                    ));
                }
                _ => {}
            }
            let search = SearchArgs {
                pattern: require_string(&args, "pattern")?,
                language: optional_string(&args, "language")?,
                // The documented default is `["."]`, and `search.rs` refuses an empty `paths`, so
                // omitting the argument has to mean the workspace root here rather than `[]`. The
                // minimal schema-valid call is `{"pattern": "..."}`, and that must work.
                paths: optional_string_array(&args, "paths")?
                    .unwrap_or_else(|| vec![DEFAULT_SEARCH_PATH.to_string()]),
                rule: None,
                context_lines: optional_u64(&args, "context_lines")?,
                limit: optional_u64(&args, "limit")?,
            };
            ast_search(ctx, &search)
        }
        "ast_explain_pattern" => {
            let explain = ExplainArgs {
                pattern: require_string(&args, "pattern")?,
                language: require_string(&args, "language")?,
            };
            ast_explain_pattern(ctx, &explain)
        }
        // The six edit tools. They need the plan and journal stores, which are opened lazily and
        // then reused for the process (see `crate::editstate`).
        //
        // **The arguments are built before `edit(...)`, in all six arms, and that order is the
        // point.** `edit` is what opens `PlanStore`/`JournalStore`, and opening *creates*
        // `<state>/ws-<id>/plans/` on disk. Parsing first means a request this server rejects
        // leaves the filesystem exactly as it was: a malformed call can no longer be used to
        // create directories inside the target repository, which is what a call that is answered
        // `invalid_args` must never be able to do. The ordering also keeps the reported fault
        // honest — bad arguments report `invalid_args`, not a filesystem error from a store that
        // should not have been touched at all.
        //
        // `ast_edit_preview` is in read mode and writes: it persists the plan
        // (`crates/tools/src/edit.rs`), which is why its `readOnlyHint` is false. What it never
        // writes is workspace *content* — the state dir is user-private data, not the
        // workspace. So a read-mode server's first *valid* `tools/call` of it creates
        // `state/ws-<id>/plans/`, and that is correct rather than a leak.
        "ast_plan_list" => {
            let list = PlanListArgs {
                limit: optional_usize(&args, "limit")?,
            };
            let edit = edit(ctx, state_dir)?;
            ast_plan_list(&edit, &list)
        }
        "ast_plan_show" => {
            let show = PlanShowArgs {
                plan_id: require_string(&args, "plan_id")?,
                file: optional_string(&args, "file")?,
                offset: optional_usize(&args, "offset")?,
                limit: optional_usize(&args, "limit")?,
            };
            let edit = edit(ctx, state_dir)?;
            ast_plan_show(&edit, &show)
        }
        "ast_edit_preview" => {
            // `rule` is published as a string but a `Rule` is a structured object with no string
            // form, so there is nothing to parse it from. It is refused rather than dropped —
            // silently ignoring an argument the caller supplied is the failure mode this tool set
            // exists to avoid (same reasoning as `ast_search`'s `rule`, `dispatch.rs` above).
            match args.get("rule") {
                Some(v) if !v.is_null() => {
                    return Err(ToolError::new(
                        ErrorCode::InvalidArgs,
                        "`rule` is not accepted on the MCP tools/call path",
                        "Omit `rule` from this call; express the constraint in `pattern` instead.",
                    ));
                }
                _ => {}
            }
            let preview = PreviewArgs {
                kind: require_string(&args, "kind")?,
                language: optional_string(&args, "language")?,
                paths: optional_string_array(&args, "paths")?,
                pattern: optional_string(&args, "pattern")?,
                replacement: optional_string(&args, "replacement")?,
                rule: None,
                operation: optional_string(&args, "operation")?,
                path: optional_string(&args, "path")?,
                symbol: optional_string(&args, "symbol")?,
                text: optional_string(&args, "text")?,
                note: optional_string(&args, "note")?,
            };
            let edit = edit(ctx, state_dir)?;
            ast_edit_preview(&edit, &preview)
        }
        "ast_edit_apply" => {
            let apply = ApplyArgs {
                plan_id: require_string(&args, "plan_id")?,
            };
            let edit = edit(ctx, state_dir)?;
            ast_edit_apply(&edit, &apply)
        }
        "ast_undo" => {
            let undo = UndoArgs {
                plan_id: require_string(&args, "plan_id")?,
            };
            let edit = edit(ctx, state_dir)?;
            ast_undo(&edit, &undo)
        }
        "ast_recover" => {
            // `ast_recover` takes no arguments, so there is nothing to parse before the open:
            // the ordering rule is satisfied by having nothing left to validate.
            let edit = edit(ctx, state_dir)?;
            ast_recover(&edit)
        }
        other => Err(ToolError::new(
            ErrorCode::InvalidArgs,
            format!("unknown tool `{other}`"),
            "Call tools/list for the tools this server exposes.",
        )),
    }
}

fn require_string(args: &Map<String, Value>, key: &str) -> Result<String, ToolError> {
    match args.get(key).and_then(Value::as_str) {
        Some(s) => Ok(s.to_string()),
        None => Err(ToolError::new(
            ErrorCode::InvalidArgs,
            format!("missing or mistyped `{key}`"),
            "Pass a string argument.",
        )),
    }
}

fn optional_string(args: &Map<String, Value>, key: &str) -> Result<Option<String>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(ToolError::new(
            ErrorCode::InvalidArgs,
            format!("`{key}` must be a string"),
            "Pass a string or omit the argument.",
        )),
    }
}

fn optional_u64(args: &Map<String, Value>, key: &str) -> Result<Option<u64>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().map(Some).ok_or_else(|| {
            ToolError::new(
                ErrorCode::InvalidArgs,
                format!("`{key}` must be an integer"),
                "Pass a non-negative integer.",
            )
        }),
    }
}

/// The edit-tool context for one call, or `io_error` when the stores cannot be opened.
///
/// The open error is a filesystem fact about the state directory, not about the tool's arguments,
/// so it is reported as `io_error` with the store's own next step.
fn edit<'a>(
    ctx: &'a ToolContext,
    state_dir: &'a std::path::Path,
) -> Result<opencrayast_tools::EditTools<'a>, ToolError> {
    match stores(ctx, state_dir) {
        Ok(s) => Ok(edit_tools(ctx, s)),
        Err(e) => Err(ToolError::new(
            ErrorCode::IoError,
            format!("the plan store could not be opened: {}", e.message),
            "Check that the state directory is writable and owned by this user.",
        )),
    }
}

/// A `usize` argument. Separate from [`optional_u64`] because the edit handlers' args use
/// `usize` and a u64 that large would not fit; the JSON layer still rejects it as not-an-integer.
fn optional_usize(args: &Map<String, Value>, key: &str) -> Result<Option<usize>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .map(Some)
            .ok_or_else(|| {
                ToolError::new(
                    ErrorCode::InvalidArgs,
                    format!("`{key}` must be a non-negative integer"),
                    "Pass a non-negative integer.",
                )
            }),
    }
}

fn optional_bool(args: &Map<String, Value>, key: &str) -> Result<Option<bool>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(ToolError::new(
            ErrorCode::InvalidArgs,
            format!("`{key}` must be a boolean"),
            "Pass true or false.",
        )),
    }
}

fn optional_string_array(
    args: &Map<String, Value>,
    key: &str,
) -> Result<Option<Vec<String>>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some(s) => out.push(s.to_string()),
                    None => {
                        return Err(ToolError::new(
                            ErrorCode::InvalidArgs,
                            format!("`{key}` must be an array of strings"),
                            "Pass only string elements.",
                        ));
                    }
                }
            }
            Ok(Some(out))
        }
        Some(_) => Err(ToolError::new(
            ErrorCode::InvalidArgs,
            format!("`{key}` must be an array of strings"),
            "Pass a JSON array of strings.",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencrayast_core::boundary::{Boundary, BoundaryConfig};
    use opencrayast_core::limits::Limits;
    use opencrayast_tools::{
        ToolAnnotations, WRITE_TOOL_NAMES, is_write_tool_name, tools_catalog, tools_for_mode,
    };
    use serde_json::{Value, json};
    use std::collections::BTreeSet;

    /// Tools the catalogue advertises that `run` has no arm for.
    ///
    /// **Empty, and that is the point.** It was six wide while the six edit tools were
    /// advertised by `tools/list` and then answered `unknown tool` — the false green this whole
    /// mechanism exists to make visible.
    ///
    /// The list and its count assertion are kept even though both are now empty. Deleting them
    /// would restore exactly the failure mode this project keeps getting burned by: a new
    /// advertised tool with no arm would again be invisible, because the test that noticed it
    /// would no longer exist. An empty list still makes a seventh gap red — the count assertion
    /// below compares the *live* unrouted set against `KNOWN_UNWIRED.len()`, so anything the
    /// catalogue advertises and `run` cannot reach fails immediately.
    ///
    /// Naming a tool here is a deliberate statement that it is advertised and unrouted. Adding an
    /// arm and leaving the name behind fails the count; removing an arm and not naming it fails
    /// direction 1.
    const KNOWN_UNWIRED: &[&str] = &[];

    /// The catalogue names in the production half of this file that `run` dispatches, recovered
    /// from the source rather than hand-copied.
    ///
    /// A hand-written copy of the arm list would be a second list to keep in step with the first
    /// — exactly the drift this test exists to catch. Reading the source means adding an arm is
    /// observed by the next run, not by the next careful edit.
    fn dispatch_arms() -> BTreeSet<String> {
        let source = include_str!("dispatch.rs");
        // Everything before the test module is production code; the test module deliberately
        // mentions probe names that must not be mistaken for arms.
        let production = source
            .split_once("#[cfg(test)]")
            .map_or(source, |(prod, _)| prod);
        production
            .lines()
            // An arm head is a quoted tool name at the start of the line. This excludes the other
            // way a name appears in the file — `name: "..."` inside a struct literal.
            .filter_map(|line| {
                let line = line.trim_start();
                let rest = line.strip_prefix('"')?;
                let end = rest.find('"')?;
                let name = &rest[..end];
                name.starts_with("ast_").then(|| name.to_string())
            })
            .collect()
    }

    /// A state directory for tests that reach the edit tools.
    ///
    /// Per-test and under `tempfile`, so a test that previews a plan cannot be seen by another
    /// one — and so the global store cache (keyed on state dir + workspace id) does not hand a
    /// test a store opened by a different test.
    ///
    /// The `0700` is load-bearing, not cosmetic. `PlanStore::open` re-verifies whatever directory
    /// it is given and refuses one that grants group or other access (SECURITY-MODEL T-21); a
    /// test directory with looser bits would make every routed edit arm answer `io_error` before
    /// reaching its handler. This chmod stands in for the state directory a real install creates.
    fn temp_state() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("a temp dir for the state directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
                .expect("temp dir is chmod-able");
        }
        dir
    }

    /// A well-formed, full-length plan id: `p-` plus 26 base32 characters (`a-z2-7`).
    ///
    /// The alphabet matters. `is_full_plan_id` rejects a body containing `0`, `1`, `8` or `9`, so
    /// a placeholder made of zeros is refused as a *prefix* — and on the write tools that
    /// rejection happens before the write gate, which would mean the test proved the id check
    /// instead of the refusal it was written for.
    const TEST_PLAN_ID: &str = "p-aaaaaaaaaaaaaaaaaaaaaaaaaa";

    /// One tool call, as the client sees it, against a throwaway state directory.
    ///
    /// The state dir is per call so a plan previewed by one test cannot be seen by another, and
    /// so the process-wide store cache (keyed on state dir + workspace id) never hands a test a
    /// store another test opened.
    ///
    /// A single call is safe on its own: the directory outlives the call, and the store it
    /// opened is never consulted again. A test making **several** calls must hold one
    /// [`temp_state`] for its whole body instead — see [`call_in`] — because the store verifies
    /// on every operation that its directories are still the ones it captured, and a directory
    /// removed and recreated under the same path is exactly the re-pointing that check refuses.
    fn call(ctx: &ToolContext, name: &str, arguments: Value) -> String {
        let state = temp_state();
        call_in(ctx, state.path(), name, arguments)
    }

    /// One tool call against a specific state directory, for a test that needs to look at what
    /// the call left behind.
    fn call_in(
        ctx: &ToolContext,
        state_dir: &std::path::Path,
        name: &str,
        arguments: Value,
    ) -> String {
        call_tool(
            ctx,
            state_dir,
            json!(1),
            &json!({ "name": name, "arguments": arguments }),
        )
    }

    /// A well-formed workspace id for the tests in this module: `w-` plus 32 lowercase hex.
    ///
    /// The edit tools open their stores against `workspace_id`, and `PlanStore::open` validates the
    /// shape before touching the filesystem (the id becomes a directory name). A malformed id would
    /// make a correctly-routed edit arm answer `invalid_args`, which is a different failure from
    /// "unrouted" and would mask the arms these tests exist to prove.
    const TEST_WORKSPACE_ID: &str = "w-0123456789abcdef0123456789abcdef";

    /// A file inside the boundary root holding one real **call site** as well as one definition.
    ///
    /// The definition alone is enough for `ast_get` and `ast_outline`, but `ast_search` matches
    /// syntactic *shape*, and a function definition is not a call — a pattern of
    /// `no_such_call_site($$$A)` matches nothing in it. The caller below is what makes the search
    /// assertion real: one match, one capture, one file.
    const CALL_SITE_FIXTURE: &str = "call_site_marker.rs";

    /// The source of [`CALL_SITE_FIXTURE`]: a definition and exactly one call to it.
    fn call_site_marker() -> &'static [u8] {
        b"pub fn no_such_call_site(_a: &str) {}\n\
          \n\
          pub fn uses_it() {\n    \
          no_such_call_site(\"marker\");\n}\n"
    }

    /// Put `content` at `root/name`, atomically.
    ///
    /// Atomic because the shared root is rewritten by every test in this module while other
    /// tests may be reading it, and the harness runs them in parallel: a plain `fs::write`
    /// truncates first, so a tool call in another thread can open the file between the truncate
    /// and the write and legitimately answer `0 lines` or "unreadable". A reader sees either the
    /// old bytes or the new ones, never an empty file.
    ///
    /// The temp name must be unique per *call*, not per process or per fixture: tests run in
    /// parallel and several of them call `ctx_for` at the same moment, so two threads writing
    /// the same fixture would share a temp path and race on the rename as well — one of them
    /// would find its temp file already renamed away.
    fn put(root: &std::path::Path, name: &str, content: &[u8]) {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let tmp = root.join(format!(
            ".{name}.{}.{}.tmp",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::write(&tmp, content).expect("the boundary root is writable");
        std::fs::rename(&tmp, root.join(name)).expect("the fixture lands atomically");
    }

    /// The shared boundary root these tests read fixtures out of.
    ///
    /// Shared on purpose — the fixture paths in the assertions are relative, and the store cache
    /// is keyed on the workspace id as well as the state directory — but a fixed path rather than
    /// a `tempfile`, because [`TEST_WORKSPACE_ID`] must stay a stable literal.
    fn shared_root() -> std::path::PathBuf {
        let root = std::env::temp_dir().join("opencrayast-mcp-invariant");
        let _ = std::fs::create_dir_all(&root);
        root
    }

    fn ctx_for(mode: Mode) -> ToolContext {
        ctx_in(mode, &shared_root())
    }

    /// A context whose boundary root is private to one test, so nothing rewrites its files.
    ///
    /// An assertion that reads files *it did not name* — an unqualified `ast_get`, a
    /// directory-level `ast_search` — must not race a parallel test rewriting the shared
    /// fixtures underneath it. This gives each such test its own root holding the same two
    /// files, and the directory outlives the context.
    fn private_ctx_for(mode: Mode) -> (ToolContext, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("a temp dir for a private boundary root");
        let root = dir.path().to_path_buf();
        let _ = std::fs::create_dir_all(&root);
        (ctx_in(mode, &root), dir)
    }

    fn ctx_in(mode: Mode, root: &std::path::Path) -> ToolContext {
        // Real files inside the boundary root, so a tool that walks the workspace
        // (`ast_edit_preview`) has something to walk instead of answering `not_found`. The
        // boundary is created from this root, so a relative `paths` argument resolves inside it.
        put(
            root,
            "boundary_root_marker.rs",
            b"pub fn no_such_call_site(_a: &str) {}\n",
        );
        put(root, CALL_SITE_FIXTURE, call_site_marker());
        let limits = Limits::default();
        let boundary = Boundary::new(BoundaryConfig::new(root.to_path_buf(), limits.clone()))
            .expect("temp root is a valid boundary root");
        ToolContext {
            boundary,
            limits,
            mode,
            // Deliberately no capability: minting a `WriteCap` is the shell's job (WCAP-1) and a
            // test must not hold one. A routed write tool answers `[write_disabled]` here, which
            // is still distinguishable from `unknown tool` — and is the correct proof that an arm
            // exists.
            write: None,
            version: "invariant-test".to_string(),
            // A **well-formed** workspace id — `w-` plus 32 lowercase hex. The edit tools open
            // a `PlanStore` against it, and the store refuses anything else (the id becomes a
            // path component), so the placeholder this test used to carry would have made every
            // routed edit arm answer `invalid_args` instead of reaching its handler. It is a
            // fixed literal rather than a derived id so the store directory is predictable here.
            workspace_id: TEST_WORKSPACE_ID.to_string(),
            respect_gitignore: true,
            extra_ignore: Vec::new(),
            config_source: Default::default(),
        }
    }

    /// The next step the `unknown tool` envelope carries, for `name` on a `server` in `mode`.
    ///
    /// The write tools are refused with the same code as a genuinely absent name, but they must
    /// not carry the same advice: telling a caller to consult `tools/list` about a tool that is
    /// real and merely unrunnable teaches it that it invented the name. So the two cases are
    /// spelled out here and the test below asserts the real bytes.
    fn unknown_tool_next(name: &str, mode: Mode) -> &'static str {
        if mode == Mode::ReadOnly && is_write_tool_name(name) {
            "This server is read-only and `ast_edit_apply`, `ast_undo` and `ast_recover` are \
             write tools. Call ast_info to see what this server allows; enabling write mode needs \
             BOTH --allow-write on the command line AND policy.allow_write = true in the \
             server's configuration file. Until then the write tools are not listed and cannot be \
             called."
        } else {
            "Call tools/list for the tools this server exposes."
        }
    }

    /// The exact envelope a refused or unrouted tool must produce: `invalid_args`, `isError`,
    /// and the `unknown tool` text. Used as a whole so no partial result can pass for a refusal.
    fn expected_unknown_tool(name: &str, mode: Mode) -> String {
        tool_result(
            json!(1),
            &format!(
                "[invalid_args] unknown tool `{name}` Next: {}",
                unknown_tool_next(name, mode)
            ),
            true,
        )
    }

    /// Whether `run` has an arm for `name`, judged the way a client observes it: an unrouted
    /// tool is the only one that answers `unknown tool`.
    fn is_routed(ctx: &ToolContext, name: &str) -> bool {
        !call(ctx, name, json!({})).contains("unknown tool")
    }

    /// The catalogue↔dispatch invariant, in both directions, per mode.
    ///
    /// - **advertised ⇒ routable.** Every tool `tools/list` publishes in this mode has a handler.
    ///   This is the false-green: the six tools in [`KNOWN_UNWIRED`] are published and then answer
    ///   `unknown tool`, which no test asserted before.
    /// - **routable ⇒ advertised.** Every arm in `run` names a tool the catalogue publishes.
    ///   An arm for a name the catalogue omits (a typo, a removed tool) is dead code that still
    ///   reads as coverage.
    ///
    ///   The check is against the **whole** catalogue rather than this mode's listing, and that
    ///   is deliberate. A write arm is unreachable in read mode — the mode gate answers
    ///   `unknown tool` before `run` is entered — so asserting per-mode would force either a
    ///   useless exemption for the three write arms or the removal of the arms entirely. The
    ///   reachability half of the invariant is already covered where it belongs: direction 1,
    ///   which calls every advertised tool in each mode, and
    ///   [`read_mode_refuses_every_write_tool_with_unknown_tool`], which proves the gate.
    ///
    /// Mutation self-proof, both directions:
    /// - delete the `"ast_get" =>` arm → the read-mode arm/routable assertion goes red;
    /// - add `"ast_gett" =>` → the routable ⇒ advertised assertion goes red as an orphan arm;
    /// - add a catalogue entry → advertised ⇒ routable goes red unless the name is in
    ///   [`KNOWN_UNWIRED`] and the count is updated.
    #[test]
    fn catalogue_and_dispatch_agree_in_both_directions_in_every_mode() {
        let arms = dispatch_arms();

        // The list stays honest about itself: exactly the catalogue's missing handlers.
        assert_eq!(
            KNOWN_UNWIRED.iter().collect::<BTreeSet<_>>().len(),
            KNOWN_UNWIRED.len(),
            "KNOWN_UNWIRED has a duplicate name"
        );

        for mode in [Mode::ReadOnly, Mode::Write] {
            let ctx = ctx_for(mode);
            let advertised: BTreeSet<&str> = tools_for_mode(mode).map(|t| t.name).collect();

            // Direction 1: advertised ⇒ routable.
            for name in &advertised {
                let routed = is_routed(&ctx, name);
                if !routed && !KNOWN_UNWIRED.contains(name) {
                    panic!(
                        "mode {}: `tools/list` advertises `{name}` but `run` has no arm for it, \
                         and it is not in KNOWN_UNWIRED — wire it, or record it there (and \
                         update the count).",
                        mode.as_str()
                    );
                }
            }

            // Direction 2: routable ⇒ advertised, checked against the whole catalogue. See the
            // method doc: a write arm is unreachable in read mode by design, so demanding
            // per-mode presence would be a false requirement, not a real one.
            for arm in &arms {
                assert!(
                    find_tool(arm).is_some(),
                    "`run` has an arm for `{arm}` but the catalogue does not advertise it at \
                     all — a dead arm"
                );
            }
        }

        // The count is the bound. A seventh gap has no name here, so direction 1 fails for it.
        let unwired_now: Vec<&str> = tools_catalog()
            .iter()
            .map(|t| t.name)
            .filter(|n| !arms.contains(*n))
            .collect();
        assert_eq!(
            unwired_now.len(),
            KNOWN_UNWIRED.len(),
            "the number of advertised-but-unrouted tools changed: {:?} are unrouted, \
             KNOWN_UNWIRED holds {:?}",
            unwired_now,
            KNOWN_UNWIRED
        );
        for name in &unwired_now {
            assert!(
                KNOWN_UNWIRED.contains(name),
                "`{name}` is advertised with no handler but is not named in KNOWN_UNWIRED"
            );
        }
    }

    /// The three read-mode plan tools are routed, each answering from its own handler rather
    /// than `unknown tool`.
    ///
    /// One assertion per arm, so removing any single arm makes exactly this test red rather
    /// than being absorbed by the count assertion. The arguments are the documented ones for
    /// each tool; the results are checked to be handler output, not merely "not unknown tool",
    /// because an arm that returns the right shape of refusal would still be a broken arm.
    #[test]
    fn the_three_read_mode_plan_tools_are_routed_in_read_mode() {
        // A private root, not the shared one: the shared fixture directory is a fixed path under
        // temp_dir, and CI runs several test jobs on one machine at once. Another job's fixture
        // can be mid-write while this one walks the directory, which is a race the test cannot
        // see and does not cause. This assertion reads the root's listing, so it needs a root it
        // owns.
        let (ctx, root) = private_ctx_for(Mode::ReadOnly);
        // The preview arm searches for a shape, and a private root has to contain something to
        // search for. Zero matches is the success this assertion wants, so one ordinary function
        // is enough and its content does not matter.
        std::fs::write(
            root.path().join("boundary_root_marker.rs"),
            "fn marker() {}\n",
        )
        .expect("the private root is writable");
        // One state dir for the whole test: the store re-verifies its directories on every
        // operation, so three calls against three different directories would each open a fresh
        // store, and one shared dir kept alive is what a server actually does.
        let state = temp_state();

        // `ast_plan_list` — an empty store answers a successful listing.
        let listed = call_in(&ctx, state.path(), "ast_plan_list", json!({}));
        assert!(
            !listed.contains("unknown tool"),
            "ast_plan_list must be routed: {listed}"
        );

        // `ast_plan_show` — a well-formed but absent id is the handler's own error, which
        // proves the arm reached the store rather than stopping at dispatch.
        let shown = call_in(
            &ctx,
            state.path(),
            "ast_plan_show",
            json!({ "plan_id": TEST_PLAN_ID }),
        );
        assert!(
            shown.contains("plan_not_found") || shown.contains("plan_expired"),
            "ast_plan_show must reach the store and answer about the plan: {shown}"
        );

        // `ast_edit_preview` — a rewrite request that matches nothing is a SUCCESSFUL result
        // (TOOLS.md: 0 matches is normal), so a non-error answer here proves the arm ran.
        let previewed = call_in(
            &ctx,
            state.path(),
            "ast_edit_preview",
            json!({
                "kind": "rewrite",
                "language": "rust",
                "pattern": "no_such_call_site_$$$A",
                "replacement": "x",
                "paths": ["boundary_root_marker.rs"],
            }),
        );
        assert!(
            !previewed.contains("unknown tool"),
            "ast_edit_preview must be routed: {previewed}"
        );
        assert!(
            !previewed.contains("isError\":true") && !previewed.contains("invalid_args"),
            "a 0-match rewrite is a normal result, not a refusal: {previewed}"
        );
    }

    /// The three write tools are routed in write mode, and **refused without a capability**.
    ///
    /// This is the half that matters. `ctx_for` deliberately carries no `WriteCap` — minting one
    /// is the shell's job (WCAP-1) — so with `--allow-write` absent there is no capability, and
    /// every write tool must answer `write_disabled` from L3. That is the same envelope a
    /// missing name would produce *except* the code and text name the write gate rather than
    /// `unknown tool`, which is how a caller learns the tool exists but the write is off. (The
    /// read-mode refusal is the *other* layer and is proved by
    /// [`read_mode_refuses_every_write_tool_with_unknown_tool`].)
    #[test]
    fn the_three_write_tools_refuse_without_a_capability_in_write_mode() {
        let ctx = ctx_for(Mode::Write);
        assert!(
            ctx.write.is_none(),
            "this test is about the no-capability path"
        );
        // One state dir for all three calls, kept alive for the whole test — see the note on
        // `call` about store directory identity.
        let state = temp_state();

        for (name, args) in [
            ("ast_edit_apply", json!({ "plan_id": TEST_PLAN_ID })),
            ("ast_undo", json!({ "plan_id": TEST_PLAN_ID })),
            ("ast_recover", json!({})),
        ] {
            let answered = call_in(&ctx, state.path(), name, args);
            assert!(
                !answered.contains("unknown tool"),
                "{name} must be routed in write mode (no arm ⇒ unknown tool): {answered}"
            );
            assert!(
                answered.contains("write_disabled"),
                "{name} without a WriteCap must answer write_disabled: {answered}"
            );
        }
    }

    /// `ast_edit_preview` creates the plan store — the read-mode tool that genuinely writes.
    ///
    /// This pins the behaviour the operator accepted: a read-mode server's first preview creates
    /// `state/ws-<id>/plans/`. It writes user-private state, never workspace content, so the
    /// creation is correct. The test asserts the store appears and that the workspace file list
    /// is unchanged by the preview (the boundary is what guarantees "never the workspace").
    #[test]
    fn preview_creates_the_state_store_but_not_workspace_content() {
        let state = temp_state();
        let ctx = ctx_for(Mode::ReadOnly);

        let plans_dir = state.path().join(format!("ws-{}/plans", ctx.workspace_id));
        assert!(
            !plans_dir.exists(),
            "no plan store before the first edit-tool call — the store is opened lazily"
        );

        let _ = call_in(
            &ctx,
            state.path(),
            "ast_edit_preview",
            json!({
                "kind": "rewrite",
                "language": "rust",
                "pattern": "no_such_call_site_$$$A",
                "replacement": "x",
                "paths": ["boundary_root_marker.rs"],
            }),
        );

        assert!(
            plans_dir.is_dir(),
            "the first preview must create the per-workspace plan store at {plans_dir:?}"
        );
    }

    /// In read mode every write tool answers exactly `unknown tool`, and the converse at the
    /// boundary layer: write mode does not refuse them for mode reasons.
    ///
    /// The whole envelope is compared, so a different code, a `[write_disabled]` handler result,
    /// or a partial success all fail — the refusal has to be indistinguishable from any other
    /// unrecognised name, which is what stops a read-mode server leaking that the write tools
    /// exist.
    ///
    /// Scoped to the mode boundary on purpose. "In write mode they *run*" is not asserted: the
    /// write handlers are unwired (see [`KNOWN_UNWIRED`]) and wiring them is blocked on the
    /// state-directory decision. What must hold regardless is that read mode refuses them and
    /// write mode's refusal is not the mode gate.
    #[test]
    fn read_mode_refuses_every_write_tool_with_unknown_tool() {
        let read_ctx = ctx_for(Mode::ReadOnly);

        for name in WRITE_TOOL_NAMES {
            assert!(
                is_write_tool_name(name),
                "{name} must be a write tool by name"
            );

            // Read mode: the refusal, byte for byte.
            assert_eq!(
                call(&read_ctx, name, json!({})),
                expected_unknown_tool(name, Mode::ReadOnly),
                "read mode must answer `unknown tool` for `{name}`, exactly"
            );

            // Converse, at the layer this crate owns: write mode does not gate it out, and the
            // entry is a write-mode one. Both are properties of the mode gate, not of whether a
            // handler is wired — which is what keeps this half true while wiring is blocked.
            let entry = find_tool(name).unwrap_or_else(|| panic!("{name} must be catalogued"));
            assert!(
                tool_callable(Mode::Write, entry),
                "write mode must not refuse `{name}` at the mode boundary"
            );
            assert_eq!(
                entry.mode,
                Mode::Write,
                "{name} must be a write-mode catalogue entry"
            );
            // And the refusal above is this gate's doing, in both directions: read mode refuses
            // the entry because it declares `Mode::Write`, and write mode does not. Both are
            // properties of the mode gate alone, which is what keeps them true while the
            // handlers remain unwired.
            assert!(
                !tool_callable(Mode::ReadOnly, entry),
                "read mode must refuse `{name}` at the mode boundary"
            );
        }

        // No read-mode tool may be refused: the boundary blocks writes only.
        for entry in tools_for_mode(Mode::ReadOnly) {
            assert!(
                is_routed(&read_ctx, entry.name) || KNOWN_UNWIRED.contains(&entry.name),
                "read-mode tool `{}` must not be refused in read mode",
                entry.name
            );
        }
    }

    /// Every routed arm must answer as **its own tool**, not merely as "not `unknown tool`".
    ///
    /// The bidirectional test above compares names, and a name is all a wrong arm can keep: an
    /// arm can reach the wrong backend, ignore its own arguments, or return another tool's shape
    /// and stay green there, because the only thing that test can see is which string is absent.
    /// So each arm gets one assertion here that could only pass for that arm's own handler —
    /// output that this backend alone produces.
    ///
    /// **This is the `ast_get` arm, asserted against `ast_outline`'s output on purpose.**
    /// `ast_get` returns the symbol's *source* in a fenced block; `ast_outline` returns a symbol
    /// *table* — indented `kind name L<n>` rows and a `<rel> <language> <N> lines` header. The two
    /// are read from the same file and share no output token, so pointing the `ast_get` arm at
    /// `ast_outline` cannot pass. `ast_plan_list` is the other direction worth having: its answer
    /// is a plan listing ("`N` plans for workspace …"), which no read tool and no other edit tool
    /// produces, so an arm wired to `ast_search` is caught there.
    ///
    /// Mutation self-proof: replace the `ast_get` arm's body with
    /// `ast_outline(ctx, &OutlineArgs { path: get.symbol, ..Default::default() })` and this goes
    /// red at `ast_get`'s source assertion.
    #[test]
    fn every_read_arm_answers_as_its_own_tool() {
        // A private boundary root: the `ast_get` and `ast_search` assertions below read every
        // file under the root rather than one it named, so a parallel test rewriting a shared
        // fixture must not be able to make them read a half-written file.
        let (ctx, _root) = private_ctx_for(Mode::ReadOnly);
        let state = temp_state();

        // `ast_info` — the five-line "what is running" report. Its own tokens: the version banner
        // and the limits line. No other tool prints either.
        let info = call_in(&ctx, state.path(), "ast_info", json!({}));
        assert!(
            info.contains("(mode: read-only)")
                && info.contains("limits: file")
                && info.contains("write: disabled"),
            "ast_info must answer with its own five-line report: {info}"
        );
        // The `write:` line must track the context's mode — a report that always says "disabled"
        // would satisfy the line above in write mode too, so both modes are checked.
        let info_write = call(&ctx_for(Mode::Write), "ast_info", json!({}));
        assert!(
            info_write.contains("(mode: write)") && info_write.contains("write: enabled"),
            "ast_info in write mode must report write mode: {info_write}"
        );

        // `ast_outline` — a symbol *table*: the file header line, then an indented kind/name row
        // with an `L<line>` position. `ast_get` on the same symbol returns source instead, and has
        // no `L<digits>` row.
        let outline = call_in(
            &ctx,
            state.path(),
            "ast_outline",
            json!({ "path": "boundary_root_marker.rs" }),
        );
        assert!(
            outline.contains("boundary_root_marker.rs")
                && outline.contains("rust")
                && outline.contains("1 line")
                && outline.contains("L1"),
            "ast_outline must answer with a symbol table for the file: {outline}"
        );

        // `ast_get` — the symbol's SOURCE, in a fenced block, with the location header
        // `<rel>:<first>-<last>  <kind> <qualified>  (<language>)`. Both halves are its own: the
        // outline output has no fence, and `ast_search` has no header of this shape.
        //
        // `path` is given because the boundary root holds two fixtures that both define this
        // symbol, and an unqualified call is then `ambiguous` — a *correct* answer this arm would
        // make, but not one that shows it returns source.
        let got = call_in(
            &ctx,
            state.path(),
            "ast_get",
            json!({ "symbol": "no_such_call_site", "path": CALL_SITE_FIXTURE }),
        );
        assert!(
            got.contains(&format!("{CALL_SITE_FIXTURE}:1"))
                && got.contains("fn no_such_call_site  (rust)")
                && got.contains("```rust")
                && got.contains("pub fn no_such_call_site"),
            "ast_get must answer with that symbol's source: {got}"
        );
        // The negative half, which is what makes this an assertion about `ast_get` rather than
        // about the fixture: an outline of the same file has no fence and no `fn` body.
        assert!(
            !got.contains("1 line") && !got.contains("L1"),
            "ast_get's answer must not be an outline table: {got}"
        );

        // `ast_search` — a match *count* header and a `file:line:col-col` position, neither of
        // which any other tool prints. The fixture is built to match, so the count is real.
        let searched = call_in(
            &ctx,
            state.path(),
            "ast_search",
            json!({
                "pattern": "no_such_call_site($$$A)",
                "language": "rust",
                "paths": [CALL_SITE_FIXTURE],
            }),
        );
        assert!(
            searched.contains("Found 1 matches in 1 files")
                && searched.contains(&format!("{CALL_SITE_FIXTURE}:4:5-4:32"))
                // A list capture prints under its own name, with three sigils. A single capture
                // would print `$A`, so this is the list shape this fixture actually has. The
                // capture text is JSON-escaped, because the text sits inside a JSON-RPC string.
                && searched.contains(r#"$$$A = \"marker\""#),
            "ast_search must answer with its match listing: {searched}"
        );

        // `ast_explain_pattern` — the parse tree in a ```text fence, under `metavariables:`.
        // Only this tool describes a pattern; nothing else prints either token.
        let explained = call_in(
            &ctx,
            state.path(),
            "ast_explain_pattern",
            json!({ "pattern": "no_such_call_site($$$A)", "language": "rust" }),
        );
        assert!(
            explained.contains("metavariables: $$$A (list)")
                && explained.contains("```text")
                // The node tree itself — the only thing this tool prints. `ast_search` prints
                // match *positions*, never a parse tree, so the two cannot be confused.
                && explained.contains("call_expression")
                && explained.contains("arguments"),
            "ast_explain_pattern must answer with the pattern's parse: {explained}"
        );
    }

    /// Every routed edit arm must answer as **its own edit tool** — the read-mode three here, the
    /// write-mode three in [`the_three_write_tools_refuse_without_a_capability_in_write_mode`]
    /// (where the handler's own gate, not its output, is what distinguishes it).
    ///
    /// The name-level test cannot see that `ast_plan_list`'s arm reached `ast_plan_show`: both
    /// answer about plans, both answer `ok`, and only one of them is a listing. The distinguishing
    /// token is the count line `N plan(s) for workspace <id> (state, expiry)`, which only
    /// `ast_plan_list` prints — `ast_plan_show` starts with `plan <id>`.
    ///
    /// The arguments are deliberately wrong in a way that has to reach *this* handler to be
    /// recognised: `ast_plan_show` is asked for a `file` no plan has, `ast_edit_preview` for a
    /// pattern matching nothing. A wrong backend cannot produce either message.
    #[test]
    fn every_plan_arm_answers_as_its_own_tool() {
        let (ctx, _root) = private_ctx_for(Mode::ReadOnly);
        let state = temp_state();

        // `ast_plan_list` — an empty store still prints the count line naming the workspace. A
        // listing is the only thing that prints "<N> plan(s) for workspace <id>".
        let listed = call_in(&ctx, state.path(), "ast_plan_list", json!({}));
        assert!(
            listed.contains(&format!("0 plans for workspace {}", ctx.workspace_id))
                && listed.contains("(state, expiry)"),
            "ast_plan_list must answer with a plan listing: {listed}"
        );

        // A limit out of range is `ast_plan_list`'s own validation (1..=200), which no other
        // handler enforces — so an arm wired to `ast_plan_show` cannot produce this message.
        let refused = call_in(&ctx, state.path(), "ast_plan_list", json!({ "limit": 0 }));
        assert!(
            refused.contains("limit 0 is outside 1..=200"),
            "ast_plan_list must enforce its own limit range: {refused}"
        );

        // `ast_plan_show` — an absent id is `plan_not_found` from the store. `ast_plan_list`
        // would answer `ok` with a listing, so the error code is what separates the two arms.
        let shown = call_in(
            &ctx,
            state.path(),
            "ast_plan_show",
            json!({ "plan_id": TEST_PLAN_ID }),
        );
        assert!(
            shown.contains("[plan_not_found]"),
            "ast_plan_show must answer from the store about this id: {shown}"
        );
        // And its own prefix rule (>= 10 characters), which is its validation and not the store's.
        let short = call_in(
            &ctx,
            state.path(),
            "ast_plan_show",
            json!({ "plan_id": "p-abc" }),
        );
        assert!(
            short.contains("too short to be unambiguous"),
            "ast_plan_show must enforce its own id-length rule: {short}"
        );

        // `ast_edit_preview` — a request that MATCHES stores a plan and prints its id back, which
        // no other handler does: no other tool hands a caller a plan id to apply. (A 0-match
        // rewrite is a normal result but stores nothing, so it would prove the arm ran without
        // proving this store, and the cross-arm check below needs the plan to exist.)
        let previewed = call_in(
            &ctx,
            state.path(),
            "ast_edit_preview",
            json!({
                "kind": "rewrite",
                "language": "rust",
                "pattern": "no_such_call_site($$$A)",
                "replacement": "replaced_call($$$A)",
                "paths": [CALL_SITE_FIXTURE],
            }),
        );
        assert!(
            previewed.contains("  (expires ") && previewed.contains("1 edits"),
            "ast_edit_preview must answer with a stored plan summary: {previewed}"
        );
        // And the plan it just stored must be the one the listing reports — the two arms reading
        // the same store, which is what makes them one server rather than two coincident handlers.
        let after = call_in(&ctx, state.path(), "ast_plan_list", json!({}));
        let stored_id = previewed
            .split_once("plan ")
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .unwrap_or_else(|| panic!("no plan id in the preview: {previewed}"));
        assert!(
            after.contains(&format!("1 plan for workspace {}", ctx.workspace_id)),
            "the previewed plan must appear in ast_plan_list: {after}"
        );
        assert!(
            after.contains(stored_id),
            "the plan {stored_id} the preview printed must be listed by ast_plan_list: {after}"
        );
    }

    /// The five edit arms that take arguments parse them **before** the stores are opened — the
    /// ordering that keeps a refused call from creating anything on disk.
    ///
    /// `edit(...)` opens `PlanStore`/`JournalStore`, and opening *creates*
    /// `<state>/ws-<id>/plans/`. An arm that opened first therefore turned "send a malformed
    /// request" into a filesystem write primitive keyed on nothing — and the spec-level test
    /// cannot prove it for the three write tools, because a read-mode server refuses them at the
    /// mode gate before dispatch ever sees them. A write-mode context reaches all three.
    ///
    /// The assertion is that **nothing** appears under the state directory, not that a particular
    /// tool refused: a `read_dir` on a path that was never created is the strongest statement
    /// available, and it still catches a store that was opened and then failed for another reason.
    ///
    /// `ast_recover` takes no arguments, so it has nothing to parse before the open and nothing
    /// this can catch. It is deliberately not listed rather than listed vacuously: it reaches the
    /// write gate and is refused there, and a refused write is a separate claim with its own test
    /// ([`the_three_write_tools_refuse_without_a_capability_in_write_mode`]).
    ///
    /// Mutation self-proof: move `edit(ctx, state_dir)?` back above the `Args` construction in any
    /// of these five arms and this goes red at that arm's `read_dir`.
    #[test]
    fn a_refused_edit_call_never_opens_the_state_store() {
        let (ctx, _root) = private_ctx_for(Mode::Write);

        for (name, arguments) in [
            ("ast_plan_list", json!({ "limit": "not a number" })),
            ("ast_plan_show", json!({})),
            ("ast_edit_preview", json!({})),
            (
                "ast_edit_preview",
                json!({ "kind": "rewrite", "rule": "r" }),
            ),
            ("ast_edit_apply", json!({})),
            ("ast_undo", json!({})),
        ] {
            let state = temp_state();
            let answered = call_in(&ctx, state.path(), name, arguments);
            assert!(
                answered.contains("invalid_args"),
                "{name} must refuse these arguments itself, not answer something else: {answered}"
            );
            assert!(
                std::fs::read_dir(state.path())
                    .expect("a refused call must not have created the state directory at all")
                    .next()
                    .is_none(),
                "{name} created state for a call it refused — the stores were opened before the \
                 arguments were parsed"
            );
        }
    }

    const READ_ANN: ToolAnnotations = ToolAnnotations {
        read_only_hint: true,
        destructive_hint: false,
        idempotent_hint: true,
        open_world_hint: false,
    };

    /// Mutation self-proof for ②: a `Mode::Write` entry whose *name* is absent from
    /// `WRITE_TOOL_NAMES` must still be blocked in read mode. The old hand-list gate
    /// would have let a catalogued call through; `tool_callable` must not.
    #[test]
    fn mode_gate_blocks_write_entry_even_when_name_is_not_on_the_hand_list() {
        let sneaky = ToolEntry {
            name: "ast_hypothetical_write",
            description: "not a real tool — mutation probe only",
            mode: Mode::Write,
            annotations: READ_ANN,
            input_schema: r#"{"type":"object","properties":{}}"#,
        };
        assert!(
            !is_write_tool_name(sneaky.name),
            "probe name must sit outside the hand-written list"
        );
        assert!(
            !tool_callable(Mode::ReadOnly, &sneaky),
            "read mode must refuse Mode::Write entries by mode, not by name list"
        );
        assert!(tool_callable(Mode::Write, &sneaky));
    }

    /// The minimal schema-valid `ast_search` call must work: `paths` omitted.
    ///
    /// This is the only call `SEARCH_SCHEMA` permits that the handler used to refuse. `paths` is
    /// not in `required`, `TOOLS.md` and `search.rs` both document its default as `["."]`, and
    /// `search.rs` refuses an empty `paths` — so dispatch turned a documented default into
    /// `paths has 0 entries, outside 1..=64` on a model's first attempt. A model cannot discover
    /// that by retrying: the schema says the call is well-formed.
    ///
    /// Mutation self-proof: restore `.unwrap_or_default()` on the `paths` line in the `ast_search`
    /// arm — the test goes red on `paths has 0 entries`, which is exactly the old failure.
    #[test]
    fn ast_search_applies_the_documented_paths_default_when_the_argument_is_omitted() {
        let ctx = ctx_for(Mode::ReadOnly);
        // The pattern matches the boundary marker file, so a successful default call names it in
        // its output rather than merely reporting "0 matches in N files" - which would also be
        // what a search of the wrong directory produces.
        let answered = call(
            &ctx,
            "ast_search",
            json!({ "pattern": "pub fn no_such_call_site($$$PARAMS) { $$$BODY }" }),
        );

        assert!(
            !answered.contains("paths has 0 entries"),
            "omitting `paths` must apply the documented default of the workspace root, not \
             refuse: {answered}"
        );
        assert!(
            !answered.contains("isError\":true"),
            "the default call must succeed, not error: {answered}"
        );
        assert!(
            answered.contains("no_such_call_site"),
            "the default must search the workspace root, so the boundary marker file's \
             contents are found: {answered}"
        );
    }

    /// The default is `["."]`, and an explicit empty `paths` is still refused.
    ///
    /// Two separate claims that a single assertion would conflate: *omitting* the argument means
    /// the workspace root (handled by dispatch), while *passing* `[]` means the caller asked for
    /// nothing and is refused by the handler. The second must survive, or the default would have
    /// been implemented by ignoring the argument entirely.
    #[test]
    fn ast_search_still_refuses_an_explicitly_empty_paths_array() {
        let ctx = ctx_for(Mode::ReadOnly);
        let answered = call(
            &ctx,
            "ast_search",
            json!({ "pattern": "fn $N() { $B }", "paths": [] }),
        );
        assert!(
            answered.contains("paths has 0 entries"),
            "an explicit empty `paths` is the caller's error and must stay refused: {answered}"
        );
    }

    /// A write tool refused on a read-only server must be told *why*, in terms it can act on.
    ///
    /// The code stays `invalid_args` / `unknown tool` — that indeterminacy is the mode boundary,
    /// and it is deliberate. But the `Next:` is not part of that boundary, and pointing a model at
    /// `tools/list` for a tool that genuinely exists teaches it that it hallucinated the name.
    /// The refusal has to name the actual condition instead: read-only server, write tool,
    /// `ast_info` reports it, and enabling write needs the flag **and** the config key.
    ///
    /// Mutation self-proof: restore the single unconditional
    /// `"Call tools/list for the tools this server exposes."` in `unknown_tool` — the first half
    /// of this assertion goes red on the missing `--allow-write`.
    #[test]
    fn the_read_mode_refusal_of_a_write_tool_names_the_real_condition() {
        let ctx = ctx_for(Mode::ReadOnly);
        for name in WRITE_TOOL_NAMES {
            let answered = call(&ctx, name, json!({}));
            assert!(
                !answered.contains("isError\":false"),
                "{name} must still be refused as an error"
            );
            assert!(
                !answered.contains("Call tools/list"),
                "{name} exists and is merely unrunnable on a read-only server; sending the \
                 caller to tools/list teaches it that it invented the name: {answered}"
            );
            for expected in [
                "read-only",
                "ast_info",
                "--allow-write",
                "policy.allow_write = true",
            ] {
                assert!(
                    answered.contains(expected),
                    "{name} refusal must mention `{expected}`: {answered}"
                );
            }
        }
    }

    /// The converse half: a name that is genuinely not a tool still gets the `tools/list` advice.
    ///
    /// Without this the previous test could be satisfied by a blanket rewrite of every unknown-tool
    /// message into write-mode prose, which would be its own kind of lie.
    #[test]
    fn a_genuinely_unknown_name_still_points_at_tools_list() {
        let ctx = ctx_for(Mode::ReadOnly);
        assert_eq!(
            call(&ctx, "ast_no_such_tool", json!({})),
            expected_unknown_tool("ast_no_such_tool", Mode::ReadOnly)
        );
        assert!(
            call(&ctx, "ast_no_such_tool", json!({})).contains("Call tools/list"),
            "an unknown name is what tools/list is the answer for"
        );
    }

    /// The `rule` refusals must not send a model to a binary it was never told about.
    ///
    /// Both arms said "use the CLI" — a program no `tools/call` caller was ever introduced to,
    /// from either mode. What the caller can actually do is omit the field, and for preview there
    /// is a concrete alternative worth naming.
    #[test]
    fn no_refusal_points_the_model_at_a_binary_it_was_never_told_about() {
        let ctx = ctx_for(Mode::ReadOnly);
        let cases: [(&str, serde_json::Value); 2] = [
            (
                "ast_search",
                json!({ "pattern": "fn $N() { $B }", "paths": ["boundary_root_marker.rs"],
                        "rule": { "kind": "fn" } }),
            ),
            (
                "ast_edit_preview",
                json!({ "kind": "rewrite", "language": "rust", "pattern": "x",
                        "replacement": "y", "paths": ["boundary_root_marker.rs"],
                        "rule": { "kind": "fn" } }),
            ),
        ];
        for (name, arguments) in cases {
            let answered = call(&ctx, name, arguments);
            assert!(
                answered.contains("`rule` is not accepted"),
                "{name} must still refuse `rule`, loudly: {answered}"
            );
            assert!(
                !answered.contains("CLI") && !answered.contains("opencrayast "),
                "{name} must not point a tools/call caller at a binary it was never told about: \
                 {answered}"
            );
            assert!(
                answered.contains("Omit `rule`"),
                "{name} must say the actionable thing — omit the field: {answered}"
            );
        }
    }
}
