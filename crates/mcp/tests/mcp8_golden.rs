//! MCP-08: golden transcripts for each tool, including every error code.
//!
//! The first half of REQ-MCP-SERVER's acceptance criterion (「協議一致性測試」) is
//! `mcp1_stdio_spec.rs`. This file is the second half (「黃金對話紀錄」): a recorded
//! request/response exchange per tool and per reachable error code, replayed
//! against the **real** `opencrayast-mcp` binary and compared byte for byte.
//!
//! # Regenerating
//!
//! ```sh
//! cargo test -p opencrayast-mcp --test mcp8_golden -- --ignored record
//! ```
//!
//! That overwrites `tests/golden/transcripts/`. Read the diff before committing it:
//! a change in those files is a change to the protocol an MCP client sees, and it
//! should be a deliberate one.
//!
//! # Scope, honestly
//!
//! MCP-08 says 「every error code」. The codes in `docs/TOOLS.md` §Error code
//! reference are the **handler-layer** vocabulary shared with the CLI. This server's
//! stdio surface exposes only the five read tools the dispatch layer routes
//! (`ast_info`, `ast_outline`, `ast_get`, `ast_search`, `ast_explain_pattern`), so
//! most of that vocabulary is unreachable from here. `coverage_claims_every_code_it_names`
//! and `unreachable_codes_are_documented_not_silently_skipped` below make that gap
//! **failing, not invisible**: adding a transcript for a new code is opt-in, and
//! dropping one turns the count assertion red.

mod golden;

// `parse` and `render` are used here directly by the self-tests, which rebuild
// temporary transcripts from their corruptions.
use golden::{
    Case, assert_jsonrpc, capture, case_ids, parse, render, transcript_dir, transcript_path,
};

/// Every recorded transcript replays byte for byte against the live server.
#[test]
fn every_transcript_replays_exactly() {
    let cases = golden::cases();
    let mut failures = Vec::new();
    let mut replayed = 0usize;

    for case in cases {
        let path = transcript_path(&case.id);
        let Ok(text) = std::fs::read_to_string(&path) else {
            failures.push(format!(
                "transcript `{}` is missing ({})\n  regenerate: cargo test -p \
                 opencrayast-mcp --test mcp8_golden -- --ignored record",
                case.id,
                path.display()
            ));
            continue;
        };
        // Full-file check: header, declared count, client lines, regeneration
        // instructions and server lines, all byte for byte.
        if let Err(e) = golden::check(case, &text) {
            failures.push(e);
            continue;
        }
        replayed += 1;
    }

    assert!(
        failures.is_empty(),
        "{} of {} transcripts disagree with the live server:\n\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n\n")
    );
    assert_eq!(replayed, cases.len(), "every case must have been replayed");
}

/// One test per transcript, so a failure names the case instead of a line number
/// in a 40-case dump. Cheap: each spawns a short-lived server.
macro_rules! per_case {
    ($($name:ident => $id:literal),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                let case = golden::cases()
                    .iter()
                    .find(|c| c.id == $id)
                    .unwrap_or_else(|| panic!("no case with id {}", $id));
                let text = std::fs::read_to_string(transcript_path($id))
                    .unwrap_or_else(|e| panic!("transcript {}: {e}", $id));
                // Strict parse (whole-file byte equality) then live replay.
                golden::check(case, &text).unwrap_or_else(|e| panic!("{e}"));
            }
        )*
    };
}

per_case! {
    golden_protocol_tools_list => "protocol_tools_list",
    golden_protocol_ping => "protocol_ping",
    golden_protocol_initialize_negotiation => "protocol_initialize_negotiation",
    golden_protocol_notification_no_reply => "protocol_notification_no_reply",
    golden_protocol_not_initialized => "protocol_not_initialized",
    golden_error_parse_error => "error_jsonrpc_parse_error",
    golden_error_invalid_request_batch => "error_jsonrpc_invalid_request_batch",
    golden_error_invalid_request_no_version => "error_jsonrpc_invalid_request_no_version",
    golden_error_invalid_request_null_id => "error_jsonrpc_invalid_request_null_id",
    golden_error_method_not_found => "error_jsonrpc_method_not_found",
    golden_error_invalid_params_params => "error_jsonrpc_invalid_params_params_not_object",
    golden_error_invalid_params_missing_name => "error_jsonrpc_invalid_params_missing_name",
    golden_error_invalid_params_arguments => "error_jsonrpc_invalid_params_arguments_not_object",
    golden_tool_ast_info => "tool_ast_info",
    golden_tool_ast_outline => "tool_ast_outline",
    golden_tool_ast_outline_directory => "tool_ast_outline_directory",
    golden_tool_ast_get => "tool_ast_get",
    golden_tool_ast_search => "tool_ast_search",
    golden_tool_ast_search_zero_matches => "tool_ast_search_zero_matches",
    golden_tool_ast_explain_pattern => "tool_ast_explain_pattern",
    golden_tool_write_refused => "tool_write_refused_in_read_mode",
    golden_error_invalid_args_missing_path => "error_tool_invalid_args_missing_path",
    golden_error_invalid_args_wrong_type => "error_tool_invalid_args_wrong_type",
    golden_error_invalid_args_limit => "error_tool_invalid_args_limit_out_of_range",
    golden_error_invalid_args_unknown_tool => "error_tool_invalid_args_unknown_tool",
    golden_error_invalid_args_unknown_language => "error_tool_invalid_args_unknown_language",
    golden_error_invalid_args_rule_rejected => "error_tool_invalid_args_search_rule_rejected",
    golden_error_not_found_path => "error_tool_not_found_path",
    golden_error_not_found_symbol => "error_tool_not_found_symbol",
    golden_error_outside_workspace => "error_tool_outside_workspace",
    golden_error_unsupported_language => "error_tool_unsupported_language",
    golden_error_not_utf8 => "error_tool_not_utf8",
    golden_error_file_too_large => "error_tool_file_too_large",
    golden_error_invalid_pattern => "error_tool_invalid_pattern",
    golden_error_ambiguous => "error_tool_ambiguous",
}

/// The committed transcripts and the case list agree, in both directions.
///
/// Without the reverse direction (no file on disk that no case claims) a deleted
/// transcript would silently shrink coverage instead of failing.
#[test]
fn committed_files_and_case_list_are_the_same_set() {
    let dir = transcript_dir();
    let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|e| {
            e.expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|n| n.ends_with(".txt"))
        .map(|n| n.trim_end_matches(".txt").to_string())
        .collect();
    on_disk.sort();

    let mut declared: Vec<String> = case_ids().iter().map(|s| (*s).to_string()).collect();
    declared.sort();

    assert_eq!(
        on_disk,
        declared,
        "the committed transcripts and the case list disagree.\n  \
         only on disk: {}\n  only declared: {}\n  \
         A transcript needs a case, and a case needs a recorded file.",
        on_disk
            .iter()
            .filter(|n| !declared.contains(n))
            .cloned()
            .collect::<Vec<_>>()
            .join(", "),
        declared
            .iter()
            .filter(|n| !on_disk.contains(n))
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    );
}

/// MCP-08 claims 「every error code」. This asserts the codes the transcripts
/// actually pin, against the codes `docs/TOOLS.md` documents, so the coverage claim
/// is checked rather than asserted in prose.
#[test]
fn coverage_claims_every_code_it_names() {
    /// Codes a transcript's own `covers:` header says it pins.
    fn covers_codes() -> Vec<String> {
        let mut out = Vec::new();
        for id in case_ids() {
            let text = std::fs::read_to_string(transcript_path(id)).expect("transcript");
            let t = parse(&text);
            // The `covers:` header names the code in `[snake_case]` or a JSON-RPC
            // number, e.g. "error code not_found (...)" / "error code -32601 (...)".
            let mut rest = t.covers.as_str();
            while let Some(i) = rest.find("error code ") {
                rest = &rest[i + "error code ".len()..];
                let token: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                    .collect();
                if !token.is_empty() {
                    out.push(token);
                }
            }
        }
        out
    }

    let pinned = covers_codes();

    // Every JSON-RPC / MCP code the transport can emit. These are the ones a
    // stdio client can actually see, so every one of them must be pinned.
    for code in ["-32002", "-32600", "-32601", "-32602", "-32700"] {
        assert!(
            pinned.iter().any(|c| c == code),
            "JSON-RPC code {code} is documented in docs/TOOLS.md and reachable, but no \
             transcript pins it; add one under tests/golden/transcripts/"
        );
    }

    // The handler-layer codes reachable from the five read tools this server routes.
    for code in [
        "invalid_args",
        "not_found",
        "outside_workspace",
        "unsupported_language",
        "not_utf8",
        "file_too_large",
        "invalid_pattern",
        "ambiguous",
    ] {
        assert!(
            pinned.iter().any(|c| c == code),
            "error code `{code}` is reachable from this server but no transcript pins it"
        );
    }

    // And the transcript must actually be producing that code, not merely claiming
    // to: the pinned list is cross-checked against the recorded bytes.
    for id in case_ids() {
        let case = golden::cases()
            .iter()
            .find(|c| c.id == id)
            .unwrap_or_else(|| panic!("no case with id {id}"));
        let text = std::fs::read_to_string(transcript_path(id)).expect("transcript");
        let t = golden::parse_strict(case, &text)
            .unwrap_or_else(|d| panic!("transcript {id} is not self-consistent:\n  {d}"));
        for line in &t.responses {
            if let Some(code) = line
                .split("error code ")
                .nth(1)
                .and_then(|s| s.split([' ', '(', ']']).next())
            {
                assert!(
                    pinned.iter().any(|c| c == code),
                    "transcript `{id}` contains code `{code}` but no `covers:` header \
                     names it; the coverage list would be lying"
                );
            }
        }
    }
}

/// The codes this surface genuinely cannot reach are written down, so "I covered
/// every code" cannot quietly mean "I covered the easy ones".
#[test]
fn unreachable_codes_are_documented_not_silently_skipped() {
    /// Every code in `docs/TOOLS.md` §Error code reference, parsed from the document.
    fn documented_codes() -> Vec<String> {
        let doc = include_str!("../../../docs/TOOLS.md");
        let mut out = Vec::new();
        let mut in_section = false;
        for line in doc.lines() {
            let line = line.trim();
            if line.starts_with("## ") {
                in_section = line.contains("Error code reference");
                continue;
            }
            if !in_section || !line.starts_with('|') {
                continue;
            }
            let cell = line
                .trim_matches('|')
                .split('|')
                .next()
                .unwrap_or("")
                .trim();
            // Skip the header and separator rows.
            if cell == "Code" || cell.starts_with("---") {
                continue;
            }
            // Rows like `plan_not_found` / `plan_expired` / ... share one cell.
            for part in cell.split('/') {
                let code = part.trim().trim_matches('`');
                if !code.is_empty() {
                    out.push(code.to_string());
                }
            }
        }
        out
    }

    /// Codes a transcript actually contains.
    fn recorded_codes() -> Vec<String> {
        let mut out = Vec::new();
        for id in case_ids() {
            let case = golden::cases()
                .iter()
                .find(|c| c.id == id)
                .unwrap_or_else(|| panic!("no case with id {id}"));
            let text = std::fs::read_to_string(transcript_path(id)).expect("transcript");
            let t = golden::parse_strict(case, &text)
                .unwrap_or_else(|d| panic!("transcript {id} is not self-consistent:\n  {d}"));
            for line in &t.responses {
                // `[code] message` inside a tool error, or a JSON-RPC "code": N.
                let mut rest = line.as_str();
                while let Some(i) = rest.find('[') {
                    // Step past the `[` itself; the code is what sits between the
                    // brackets, not one character further in.
                    rest = &rest[i + 1..];
                    if let Some(end) = rest.find(']') {
                        let code = &rest[..end];
                        // Snake_case, digits allowed (`not_utf8`, `plan_2`), and
                        // non-empty. `code` is the literal string a `covers:`
                        // header may contain; it is not an error code.
                        if !code.is_empty()
                            && code.starts_with(|c: char| c.is_ascii_lowercase())
                            && code
                                .chars()
                                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                        {
                            out.push(code.to_string());
                        }
                    }
                }
            }
        }
        out
    }

    let documented = documented_codes();
    let recorded = recorded_codes();

    // 30 codes are documented in `docs/TOOLS.md` §Error code reference; 9 are
    // pinned by a transcript above. This list is the other 21... plus the count is
    // checked by the loop below, so an entry that stops being true fails.
    //
    // Read this list as the honest answer to 「every error code」: these are the
    // codes this stdio surface cannot emit, each with why. Two kinds of reason:
    //
    // * **not routed** — the tool that would emit the code is not in
    //   `dispatch.rs`'s match arm, so the catalogue layer answers "unknown tool"
    //   (or `tools/list` advertises it, which is the defect recorded in the
    //   coverage note below).
    // * **not deterministic** — the code is reachable, but only by racing a
    //   machine-speed budget. A golden file may not depend on how fast the host
    //   is, so there is nothing stable to record.
    const UNREACHABLE: &[(&str, &str)] = &[
        // --- routed, but the code needs a clock or a real write -------------------
        //
        // These moved OFF this list when the six edit arms were wired into the dispatcher
        // (ISSUE-MCP-CATALOGUE). The reasons are rewritten from "not routed" to the real
        // obstacle, because a stale reason is worse than no reason: it tells the next reader
        // the tool cannot be called at all, when it can.
        (
            "already_applied",
            "routed (ast_edit_apply), but it needs a plan already applied once, and getting there \
             means a real write — which read-only mode refuses",
        ),
        (
            "busy",
            "routed (ast_edit_apply), but the lock is only held long enough to matter when a second \
             process contends; a transcript cannot create the race deterministically",
        ),
        (
            "comment_loss",
            "routed (ast_edit_preview), but needs a rewrite that drops a comment, which depends \
             on the fixture's exact comment placement rather than on the tool",
        ),
        (
            "diverged",
            "routed (ast_undo / ast_recover), but needs a real write followed by an out-of-band \
             edit — two operations the wire surface cannot stage in one recorded session",
        ),
        (
            "gate_failed",
            "routed (ast_edit_apply), but only the apply path runs gates, and apply is refused in \
             read-only mode",
        ),
        (
            "invalid_edit",
            "routed (ast_edit_apply), but only the apply path validates edit sets, and apply is \
             refused in read-only mode",
        ),
        (
            "journal_missing",
            "routed (ast_undo), but needs the retention check to have dropped a journal, which \
             depends on the clock",
        ),
        (
            "limit_exceeded",
            "routed (ast_plan_list / ast_plan_show), but reaching it needs a store already over \
             its plan cap; a fresh workspace has none",
        ),
        (
            "plan_corrupt",
            "routed (ast_plan_show), but needs a plan file corrupted on disk between preview and \
             show — the harness writes the fixture, not the store",
        ),
        (
            "plan_expired",
            "routed (ast_plan_show / ast_edit_preview), but needs the TTL to elapse, which is a \
             wall clock and so not byte-stable",
        ),
        (
            "replaced_not_durable",
            "routed (ast_edit_apply), but emitted only after a real write",
        ),
        (
            "rollback_incomplete",
            "routed (ast_edit_apply), but emitted only after a real write that then fails partway",
        ),
        (
            "stale_plan",
            "routed (ast_edit_apply), but needs the file to change after preview — the harness \
             cannot mutate the workspace mid-session",
        ),
        (
            "unsupported_target",
            "routed (the write path), but emitted only by a write, and write mode is refused here",
        ),
        (
            "wrong_workspace",
            "routed (the plan tools), but needs two workspaces in one session, and a session has \
             one --workspace",
        ),
        (
            "write_disabled",
            "not an MCP code: handler-layer only. The catalogue answers unknown-tool instead (docs/TOOLS.md)",
        ),
        (
            "protected_path",
            "routed, but the only tool that resolves a protected path for writing is refused in \
             read-only mode, and the read tools never resolve one",
        ),
        // --- reachable but not deterministic --------------------------------------
        (
            "timeout",
            "not deterministic: only by exceeding the parse time budget, which depends on host speed",
        ),
        (
            "budget_exceeded",
            "not deterministic: only by exhausting a tree/depth/step budget, which depends on host speed",
        ),
        // --- environment-dependent ------------------------------------------------
        (
            "io_error",
            "not deterministic: needs a real filesystem failure (permissions, EIO); the test workspace is always writable",
        ),
        (
            "internal",
            "not deterministic: by definition unexpected; there is no input that makes the server produce it",
        ),
        (
            "config_untrusted",
            "not reachable from this surface: the configuration file is read during startup, so \
             the server refuses to start with it rather than answering a request with it. The \
             shell's exit status for it is pinned in crates/mcp/tests/exit_code_parity_spec.rs.",
        ),
    ];

    // Every code that is both documented and NOT recorded must be on the
    // UNREACHABLE list, with a reason. A new unrecorded code fails here.
    for code in &documented {
        if recorded.contains(code) {
            continue;
        }
        assert!(
            UNREACHABLE.iter().any(|(c, _)| c == code),
            "docs/TOOLS.md documents error code `{code}` and no transcript pins it, but \
             it is not on the UNREACHABLE list in \
             crates/mcp/tests/mcp8_golden.rs with a reason. Either add a transcript \
             or record why it cannot be reached from this surface."
        );
    }

    // And the reverse: no UNREACHABLE entry may actually be recorded, or the note
    // is stale.
    for (code, _why) in UNREACHABLE {
        assert!(
            !recorded.contains(&code.to_string()),
            "error code `{code}` is on the UNREACHABLE list but a transcript pins it; \
             move it into the covered set and fix the coverage note"
        );
    }

    assert!(
        documented.len() >= 30,
        "the §Error code reference parsed to only {} codes - if it was \
         retitled or reflowed, this test is now checking an empty contract",
        documented.len()
    );
}

/// Record every transcript from live server output.
///
/// Ignored by default; run it to regenerate. Deliberately *not* `--nocapture`
/// dependent and it does no timing, so it is safe to run in CI by hand.
#[test]
#[ignore = "regenerates the golden transcripts; run on purpose"]
fn record() {
    let dir = transcript_dir();
    std::fs::create_dir_all(&dir).expect("create transcript dir");
    let mut written = 0;
    for case in golden::cases() {
        let responses =
            capture(case).unwrap_or_else(|e| panic!("cannot record `{}`: {e}", case.id));
        assert_jsonrpc(&responses, &case.id);
        std::fs::write(transcript_path(&case.id), render(case, &responses))
            .expect("write transcript");
        written += 1;
        eprintln!("recorded {} ({} responses)", case.id, responses.len());
    }
    eprintln!("wrote {written} transcripts to {}", dir.display());
    // `Case` is referenced only through the cases iterator; keep the import honest.
    let _: Option<&Case> = None;
}

/// The recorder writes exactly what the replayer reads back (a round trip through
/// the on-disk format), so a format change cannot leave stale files behind.
#[test]
fn render_and_parse_round_trip() {
    for case in golden::cases() {
        let sample: Vec<String> = case
            .sends
            .iter()
            .enumerate()
            .map(|(i, _)| format!(r#"{{"jsonrpc":"2.0","id":2,"result":{{"n":{i}}}}}"#))
            .collect();
        let text = render(case, &sample);
        let back = parse(&text);
        assert_eq!(back.id, case.id);
        assert_eq!(back.covers, case.covers);
        assert_eq!(
            back.sends,
            case.sends
                .iter()
                .map(|s| s.line.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(back.responses, sample);
    }
}

/// The one normalisation is idempotent and touches nothing else.
#[test]
fn normalisation_is_exactly_the_workspace_id() {
    assert_eq!(
        golden::normalise("workspace: . (id w-0123456789abcdef0123456789abcdef)"),
        "workspace: . (id <WORKSPACE_ID>)"
    );
    assert_eq!(
        golden::normalise("workspace: . (id <WORKSPACE_ID>)"),
        "workspace: . (id <WORKSPACE_ID>)"
    );
    assert_eq!(golden::normalise("no id here"), "no id here");
    // A path-shaped value that is not the workspace id must survive untouched.
    assert_eq!(
        golden::normalise("/root/udn/secret/path.rs"),
        "/root/udn/secret/path.rs"
    );
}

// ======================================================================================
// Self-tests: the harness must reject a corrupted transcript
// ======================================================================================

/// A closed set of ways to corrupt a real transcript, each paired with the case it
/// belongs to and the text it substitutes.
///
/// The header-only mutations come first on purpose. They are the ones a
/// response-only comparison cannot see: with the old harness, editing the very
/// error code in the `covers:` line of a transcript left the suite green.
const CORRUPTIONS: &[(&str, &str, &str)] = &[
    // --- header bytes: invisible to a response-only comparison -----------------------
    (
        "error_jsonrpc_invalid_params_arguments_not_object",
        "# covers: error code -32602",
        "# covers: error code -32699",
    ),
    (
        "error_jsonrpc_invalid_params_arguments_not_object",
        "# responses: 1",
        "# responses: 9",
    ),
    (
        "error_jsonrpc_invalid_params_arguments_not_object",
        "# case: error_jsonrpc_invalid_params_arguments_not_object",
        "# case: error_jsonrpc_invalid_params_params_not_object",
    ),
    (
        "error_jsonrpc_invalid_params_arguments_not_object",
        "# golden-transcript v1",
        "# golden-transcript v2",
    ),
    (
        "error_jsonrpc_invalid_params_arguments_not_object",
        "replayed byte for byte.",
        "the quick brown fox.",
    ),
    (
        "error_jsonrpc_invalid_params_arguments_not_object",
        "#   cargo test -p opencrayast-mcp --test mcp8_golden -- --ignored record",
        "#   do not regenerate",
    ),
    // --- client-line and response bytes ---------------------------------------------
    (
        "protocol_tools_list",
        "\"name\":\"ast_search\"",
        "\"name\":\"ast_searchx\"",
    ),
    (
        "error_jsonrpc_invalid_params_arguments_not_object",
        r#""code":-32602"#,
        r#""code":-32699"#,
    ),
];

/// Every corruption listed in [`CORRUPTIONS`] is reported by the replay check.
///
/// This is the test that would have caught the gap. It never touches the committed
/// files: every corruption is applied to an in-memory copy and must be rejected by
/// [`golden::check`], the function the real suite gates on.
#[test]
fn corrupting_a_transcript_makes_the_harness_report_it() {
    let mut missed = Vec::new();

    for (id, from, to) in CORRUPTIONS {
        let case = golden::cases()
            .iter()
            .find(|c| c.id == *id)
            .unwrap_or_else(|| panic!("no case with id {id}"));
        let original = std::fs::read_to_string(transcript_path(id))
            .unwrap_or_else(|e| panic!("transcript {id}: {e}"));

        // The needle must be present, or this test would pass for the wrong
        // reason — the failure mode it exists to prevent.
        assert!(
            original.contains(from),
            "corruption target {from:?} is absent from transcript {id}; update \
             CORRUPTIONS or this self-test proves nothing"
        );
        let corrupted = original.replacen(from, to, 1);
        assert_ne!(corrupted, original, "corruption of {id} changed nothing");

        if golden::check(case, &corrupted).is_ok() {
            missed.push(format!("{id}: {from:?} -> {to:?}"));
        }
    }

    assert!(
        missed.is_empty(),
        "{} of {} corruptions went unreported by `golden::check`; the transcripts \
         are not load-bearing:\n  {}",
        missed.len(),
        CORRUPTIONS.len(),
        missed.join("\n  ")
    );
}

/// The header mutations above are rejected by the strict parser specifically.
///
/// [`golden::check`] catches everything by replaying; [`golden::parse_strict`]
/// catches a corrupted header *before* the server is spawned, and without
/// replaying. That second capability is what the old harness lacked entirely, so it
/// is asserted separately — and only for the mutations that leave the responses
/// themselves untouched.
#[test]
fn corrupting_a_transcript_header_makes_the_strict_parser_report_it() {
    let mut checked = 0usize;
    let mut missed = Vec::new();

    for (id, from, to) in CORRUPTIONS {
        let case = golden::cases()
            .iter()
            .find(|c| c.id == *id)
            .unwrap_or_else(|| panic!("no case with id {id}"));
        let original = std::fs::read_to_string(transcript_path(id))
            .unwrap_or_else(|e| panic!("transcript {id}: {e}"));
        assert!(original.contains(from), "corruption target {from:?} absent");
        let corrupted = original.replacen(from, to, 1);

        // Only header edits change the file without changing a response line; for
        // those the parser alone must be enough.
        let touches_a_response = {
            let before = parse(&original).responses;
            let after = parse(&corrupted).responses;
            before != after
        };
        if touches_a_response {
            continue;
        }
        checked += 1;

        match golden::parse_strict(case, &corrupted) {
            Ok(_) => missed.push(format!("{id}: {from:?} -> {to:?}")),
            Err(d) => {
                // The report must name a line, so the failure is actionable.
                assert!(d.line >= 1, "drift for {id} reported no line number");
            }
        }
    }

    assert!(
        checked >= 5,
        "expected the header-only corruptions to be exercised, only saw {checked}"
    );
    assert!(
        missed.is_empty(),
        "{} header corruptions were accepted by `parse_strict`:\n  {}",
        missed.len(),
        missed.join("\n  ")
    );
}

/// The pristine corpus passes both gates unchanged, so the tests above are proving
/// the gate rejects corruptions rather than that it rejects everything.
#[test]
fn uncorrupted_transcripts_pass_both_gates() {
    for case in golden::cases() {
        let text = std::fs::read_to_string(transcript_path(&case.id))
            .unwrap_or_else(|e| panic!("transcript {}: {e}", case.id));
        golden::parse_strict(case, &text)
            .unwrap_or_else(|d| panic!("committed transcript {} drifted:\n  {d}", case.id));
        golden::check(case, &text).unwrap_or_else(|e| panic!("{e}"));
    }
}
