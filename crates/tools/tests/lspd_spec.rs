//! Spec for the LSPD integration document (ISSUE-LSPD-SPEC; LSPD1-01..08).
//!
//! `docs/LSPD-INTEGRATION.md` is a **specification another repository builds against**, so the
//! failure mode that matters is not "the document is wrong" but "the document is incomplete and
//! the other side guesses". These tests make the incompleteness loud instead:
//!
//! - every LSP method this document claims to cover has exactly one row, with no empty cell;
//! - every tool in the tool catalogue is named by at least one row;
//! - every row that says "no counterpart" names **no** tool, so an absence cannot hide a
//!   promise;
//! - every engine error code exists somewhere in the error mapping;
//! - the offset worked example is recomputed here, so the numbers in the document cannot drift.
//!
//! Both sides of those lists are **derived from the upstream documents**, not hard-coded twice:
//! the tool list comes from `TOOLS.md`'s own headings and the error list from the `ErrorCode`
//! enum. A test that carried its own copy of either would pass while the engine moved on, which
//! is the same class of bug this document exists to prevent.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;

use opencrayast_core::ALL_ERROR_CODES;

const SPEC: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/LSPD-INTEGRATION.md"
));
const TOOLS_DOC: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/TOOLS.md"));

/// Every LSP method the mapping table must cover, with the direction it travels.
///
/// A method that is not in this list and not in the table is invisible to the tests; that is
/// why the list is here, next to the assertions, and why adding a method to the document
/// without adding it here is a visible diff rather than a silent omission.
const LSP_METHODS: &[(&str, &str)] = &[
    // Base protocol.
    ("initialize", "c->s"),
    ("initialized", "c->s"),
    ("shutdown", "c->s"),
    ("exit", "c->s"),
    ("$/cancelRequest", "c->s"),
    ("$/setTrace", "c->s"),
    // Window.
    ("window/workDoneProgress/cancel", "c->s"),
    ("window/logMessage", "s->c"),
    ("window/showMessage", "s->c"),
    ("window/showMessageRequest", "s->c"),
    ("window/showDocument", "s->c"),
    // Text document synchronisation.
    ("textDocument/didOpen", "c->s"),
    ("textDocument/didChange", "c->s"),
    ("textDocument/didClose", "c->s"),
    ("textDocument/didSave", "c->s"),
    ("textDocument/willSave", "c->s"),
    ("textDocument/willSaveWaitUntil", "c->s"),
    // Text document requests.
    ("textDocument/hover", "c->s"),
    ("textDocument/definition", "c->s"),
    ("textDocument/declaration", "c->s"),
    ("textDocument/typeDefinition", "c->s"),
    ("textDocument/implementation", "c->s"),
    ("textDocument/references", "c->s"),
    ("textDocument/documentHighlight", "c->s"),
    ("textDocument/documentSymbol", "c->s"),
    ("textDocument/documentLink", "c->s"),
    ("textDocument/foldingRange", "c->s"),
    ("textDocument/selectionRange", "c->s"),
    ("textDocument/linkedEditingRange", "c->s"),
    ("textDocument/moniker", "c->s"),
    ("textDocument/inlayHint", "c->s"),
    ("textDocument/inlineValue", "c->s"),
    ("textDocument/codeAction", "c->s"),
    ("textDocument/codeLens", "c->s"),
    ("textDocument/completion", "c->s"),
    ("textDocument/signatureHelp", "c->s"),
    ("textDocument/formatting", "c->s"),
    ("textDocument/rangeFormatting", "c->s"),
    ("textDocument/onTypeFormatting", "c->s"),
    ("textDocument/prepareRename", "c->s"),
    ("textDocument/rename", "c->s"),
    ("textDocument/semanticTokens/full", "c->s"),
    ("textDocument/semanticTokens/range", "c->s"),
    ("textDocument/semanticTokens/full/delta", "c->s"),
    ("textDocument/diagnostic", "c->s"),
    ("textDocument/publishDiagnostics", "s->c"),
    // Workspace.
    ("workspace/symbol", "c->s"),
    ("workspace/executeCommand", "c->s"),
    ("workspace/didChangeConfiguration", "c->s"),
    ("workspace/didChangeWatchedFiles", "c->s"),
    ("workspace/didChangeWorkspaceFolders", "c->s"),
    ("workspace/willCreateFiles", "c->s"),
    ("workspace/didCreateFiles", "c->s"),
    ("workspace/willRenameFiles", "c->s"),
    ("workspace/didRenameFiles", "c->s"),
    ("workspace/willDeleteFiles", "c->s"),
    ("workspace/didDeleteFiles", "c->s"),
    ("workspace/semanticTokens/refresh", "s->c"),
    ("workspace/inlayHint/refresh", "s->c"),
    ("workspace/codeLens/refresh", "s->c"),
    ("workspace/diagnostic/refresh", "s->c"),
    // Telemetry.
    ("telemetry/event", "s->c"),
];

/// The status vocabulary. A row uses one of these and nothing else, so "partial" can never
/// quietly mean "we did not think about it".
const STATUSES: &[&str] = &["exact", "composition", "partial", "none"];

/// The engine tool names the mapping is allowed to name, taken from `TOOLS.md`'s own headings.
///
/// Not a copy: [`tool_names`] parses those headings out of the document, and
/// [`LSPD1-02_tools_and_the_catalogue_agree`] asserts this list equals it. A tool added to the
/// engine without adding it here fails that test instead of quietly going unmapped.
const TOOLS: &[&str] = &[
    "ast_info",
    "ast_outline",
    "ast_get",
    "ast_search",
    "ast_explain_pattern",
    "ast_edit_preview",
    "ast_plan_show",
    "ast_plan_list",
    "ast_edit_apply",
    "ast_undo",
    "ast_recover",
];

/// One cell of a markdown table row, unwrapped.
///
/// A cell may hold several backticked spans - `textDocument/hover`, `textDocument/definition`
/// is one cell with two - so unwrapping has to be per span, not per cell. Trimming only the
/// ends (which is what `trim_matches` does) leaves the trailing backtick of the last span
/// glued to its text, and the caller then compares `textDocument/hover\`` against a method
/// name. Strip every backtick and split the spans, keeping the cell's remaining words.
fn cell(raw: &str) -> String {
    raw.chars()
        .filter(|c| *c != '`')
        .collect::<String>()
        .trim()
        .to_string()
}

/// One row of the tool-indexed table of §3.2: tool, LSP counterpart, status, note.
struct ToolRow {
    tool: String,
    lsp: String,
    status: String,
    note: String,
}

/// Every row of the tool-indexed table, which is the other direction of the same completeness
/// requirement: the method-indexed table cannot show that a tool has no LSP surface at all.
fn tool_rows() -> Vec<ToolRow> {
    SPEC.lines()
        .filter_map(|line| {
            if !line.starts_with("| `ast_") {
                return None;
            }
            let cells: Vec<String> = line.trim_matches('|').split('|').map(cell).collect();
            if cells.len() != 4 {
                return None;
            }
            Some(ToolRow {
                tool: cells[0].clone(),
                lsp: cells[1].clone(),
                status: cells[2].clone(),
                note: cells[3].clone(),
            })
        })
        .collect()
}

/// One row of the capability mapping: method, direction, engine side, status, note.
struct Row {
    method: String,
    direction: String,
    engine: String,
    status: String,
    note: String,
}

/// Every mapping row in the document.
fn mapping_rows() -> Vec<Row> {
    SPEC.lines()
        .filter_map(|line| {
            if !line.starts_with("| `") {
                return None;
            }
            let cells: Vec<String> = line.trim_matches('|').split('|').map(cell).collect();
            if cells.len() != 5 {
                return None;
            }
            let method = cells[0].clone();
            if method.is_empty() || method.contains(' ') {
                return None;
            }
            // Only the mapping table has a direction cell; every other table in the document
            // (the status vocabulary, the two position systems) must not be read as rows.
            if cells[1] != "c->s" && cells[1] != "s->c" {
                return None;
            }
            Some(Row {
                method,
                direction: cells[1].clone(),
                engine: cells[2].clone(),
                status: cells[3].clone(),
                note: cells[4].clone(),
            })
        })
        .collect()
}

/// The tool names `TOOLS.md` defines, read out of its own `## \`ast_…\`` headings.
fn tool_names() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for line in TOOLS_DOC.lines().filter(|l| l.starts_with("## `")) {
        for token in line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            if token.starts_with("ast_") {
                names.insert(token.to_string());
            }
        }
    }
    names
}

/// The engine error codes, taken from the engine's own exported list.
///
/// [`ALL_ERROR_CODES`] is the constant the engine itself uses to assert that every
/// `ErrorCode` variant is listed, so this side of the mapping cannot drift the way a
/// hand-maintained copy would: adding a variant without adding it to the constant fails
/// in the core crate, and adding a code to the constant fails here.
fn error_codes() -> BTreeSet<String> {
    ALL_ERROR_CODES
        .iter()
        .map(|c| c.as_str().to_string())
        .collect()
}

/// LSPD1-01: every method has exactly one row, with the direction it travels and no empty cell.
#[test]
fn lspd1_01_every_lsp_method_has_exactly_one_row_with_no_empty_cell() {
    let rows = mapping_rows();
    for (method, direction) in LSP_METHODS {
        let matching: Vec<&Row> = rows.iter().filter(|r| r.method == *method).collect();
        assert_eq!(
            matching.len(),
            1,
            "{method} must have exactly one row in the capability mapping, found {}",
            matching.len()
        );
        let row = matching[0];
        assert_eq!(
            row.direction, *direction,
            "{method} travels {direction}, the table says {}",
            row.direction
        );
        for (name, cell) in [
            ("engine side", &row.engine),
            ("status", &row.status),
            ("note", &row.note),
        ] {
            assert!(
                !cell.trim().is_empty(),
                "{method}: the {name} cell is empty; a blank cell is an unfinished promise"
            );
        }
    }
}

/// LSPD1-02: the tool list this test checks against is the tool list `TOOLS.md` defines.
#[test]
fn lspd1_02_tools_and_the_catalogue_agree() {
    let from_catalogue = tool_names();
    let in_test: BTreeSet<String> = TOOLS.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(
        from_catalogue, in_test,
        "the tool list in this test has drifted from TOOLS.md; every tool needs a mapping row, \
         so the two lists have to be the same list"
    );
}

/// LSPD1-03: every tool is named by a method row **and** has its own row in the tool-indexed
/// table, with an LSP counterpart or an explicit `-`.
///
/// Both directions, because they fail differently: a tool missing from the method table is
/// unmapped, and a tool missing from the tool table is invisible to anyone reading the
/// document by tool - which is how an integrator ends up not knowing `ast_undo` exists.
#[test]
fn lspd1_03_every_tool_is_covered_in_both_directions() {
    let method_rows = mapping_rows();
    let own_rows = tool_rows();
    for tool in TOOLS {
        // A tool's own row is required of every tool, mapped or not: the `none` rows are how a
        // reader finds out the tool exists at all. The method-row check below is a separate
        // question, asked only of tools that are not `none`.
        let own = own_rows.iter().find(|r| r.tool == *tool);
        assert!(
            own.is_some(),
            "{tool} has no row in the tool table of §3.2; a tool with no LSP surface still needs \
             to be listed, or a reader browsing by tool never learns it exists"
        );
        let matching: Vec<&ToolRow> = own_rows.iter().filter(|r| r.tool == *tool).collect();
        assert_eq!(
            matching.len(),
            1,
            "{tool} must have exactly one row in the tool table, found {}",
            matching.len()
        );
        let row = matching[0];
        for (name, cell) in [
            ("LSP counterpart", &row.lsp),
            ("status", &row.status),
            ("note", &row.note),
        ] {
            assert!(!cell.is_empty(), "{tool}: the {name} cell is empty");
        }
        assert!(
            STATUSES.contains(&row.status.as_str()),
            "{tool}: status {:?} is not one of {STATUSES:?}",
            row.status
        );
        if row.status == "none" {
            assert_eq!(
                row.lsp, "-",
                "{tool}: a 'none' row must say `-` (no LSP counterpart), found {:?}",
                row.lsp
            );
        } else {
            // A tool the document claims an LSP surface for must actually be named by a row of
            // the method-indexed table - the two directions have to agree, or a reader following
            // either one alone reaches a different conclusion.
            assert!(
                method_rows
                    .iter()
                    .any(|r| r.engine.split_whitespace().any(|t| t == *tool)),
                "{tool} is mapped to {} in the tool table, but no row of the method table names it",
                row.lsp
            );
            // Anything it does name has to be a method the first table covers.
            for method in row.lsp.split(',').map(str::trim) {
                assert!(
                    LSP_METHODS.iter().any(|(m, _)| *m == method),
                    "{tool}: names the LSP method {method:?}, which is not in the method table"
                );
            }
        }
    }
}

/// LSPD1-04: a row that says "no counterpart" must not name a tool, and every status is one of
/// the four words the vocabulary defines.
///
/// This is the test that stops "no counterpart" from becoming a place to hide a promise.
#[test]
fn lspd1_04_no_counterpart_rows_claim_no_tool_and_every_status_is_known() {
    for row in mapping_rows() {
        assert!(
            STATUSES.contains(&row.status.as_str()),
            "{}: status {:?} is not one of {STATUSES:?}",
            row.method,
            row.status
        );
        let names_a_tool = TOOLS.iter().any(|t| row.engine.contains(*t));
        if row.status == "none" {
            assert!(
                !names_a_tool,
                "{}: the row says 'none' but its engine cell names {} - an absence that promises \
                 something is worse than a blank cell",
                row.method, row.engine
            );
            assert!(
                row.engine.trim() == "-",
                "{}: the engine cell of a 'none' row must be an explicit `-` (no counterpart), \
                 found {:?}; a blank or a dash character other than '-' is an unfinished cell",
                row.method,
                row.engine
            );
        } else {
            assert!(
                names_a_tool,
                "{}: status {} but the engine cell names no tool ({:?}); the row must say what \
                 answers the request",
                row.method, row.status, row.engine
            );
        }
    }
}

/// LSPD1-05: no row names a tool that does not exist. A capability the engine does not have
/// cannot be promised in a document other people build against.
#[test]
fn lspd1_05_no_row_invents_a_tool() {
    for row in mapping_rows() {
        for token in row
            .engine
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        {
            if token.starts_with("ast_") {
                assert!(
                    TOOLS.contains(&token),
                    "{}: the row names {token:?}, which is not in TOOLS.md",
                    row.method
                );
            }
        }
    }
}

/// LSPD1-06: every engine error code appears in the error mapping.
///
/// Derived from the enum, so a code added to the engine without a mapping fails here.
#[test]
fn lspd1_06_every_engine_error_code_is_mapped() {
    let unmapped: Vec<String> = error_codes()
        .into_iter()
        .filter(|code| !SPEC.contains(&format!("`{code}`")))
        .collect();
    assert!(
        unmapped.is_empty(),
        "these engine error codes are not in the error mapping: {unmapped:?}"
    );
}

/// LSPD1-07: the offset example is recomputed here, so the document's numbers cannot drift.
///
/// The source and the expected rows are the ones in §4.2. The conversion is done the way a
/// client must do it: walk UTF-16 code units from the start of the line and count UTF-8 bytes.
#[test]
fn lspd1_07_the_offset_worked_example_is_correct() {
    const SOURCE: &str = "let s = \"α😀\";\n";

    // LSP (0-based line, 0-based UTF-16 character) -> byte offset.
    let lsp_to_byte = |line: usize, character: usize| -> usize {
        // The byte offset at which 1-based `line` starts.
        let line_start = if line == 0 {
            0
        } else {
            SOURCE
                .match_indices('\n')
                .nth(line - 1)
                .map_or(0, |(i, b)| i + b.len())
        };
        let mut bytes = line_start;
        let mut units = 0usize;
        for c in SOURCE[line_start..].chars() {
            if units == character {
                return bytes;
            }
            if c == '\n' {
                break;
            }
            units += c.len_utf16();
            bytes += c.len_utf8();
        }
        bytes
    };

    // The properties the document states about `S` itself.
    assert_eq!(SOURCE.len(), 18, "`S` is 18 bytes");
    assert_eq!(
        SOURCE[..SOURCE.len() - 1].encode_utf16().count(),
        14,
        "line 0 has 14 UTF-16 code units"
    );

    // (lsp line, lsp character, byte offset, engine line:col)
    let expected = [
        (0usize, 8usize, 8usize, "1:9"),
        (0, 9, 9, "1:10"),
        (0, 10, 11, "1:12"),
        (0, 12, 15, "1:16"),
        (0, 14, 17, "1:18"),
    ];
    for (line, character, byte, engine) in expected {
        assert_eq!(
            lsp_to_byte(line, character),
            byte,
            "LSP {line}:{character} should be byte {byte}"
        );
        // The engine's column is 1-based, so it is the byte offset plus one.
        assert_eq!(
            format!("{}:{}", line + 1, byte + 1),
            engine,
            "engine line:col for LSP {line}:{character}"
        );
        let row = format!("| {line} | {character} | ");
        assert!(
            SPEC.contains(&row),
            "the document must contain the row starting {row:?} (engine {engine})"
        );
    }

    // The trap in words: character 10 is byte 11, not 10, and byte 10 is not a character
    // boundary - it is the second byte of `α`.
    assert_eq!(lsp_to_byte(0, 10), 11);
    assert!(
        !SOURCE.is_char_boundary(10),
        "byte 10 must be inside α, which is what makes the trap a trap"
    );

    // The reverse direction, also tabulated in §4.2.
    let byte_to_lsp = |byte: usize| -> (usize, usize) {
        let line = SOURCE[..byte].matches('\n').count();
        let line_start = if line == 0 {
            0
        } else {
            SOURCE
                .match_indices('\n')
                .nth(line - 1)
                .map_or(0, |(i, b)| i + b.len())
        };
        let character = SOURCE[line_start..byte].encode_utf16().count();
        (line, character)
    };
    for (engine, byte, line, character) in [("1:12", 11usize, 0usize, 10usize), ("1:16", 15, 0, 12)]
    {
        assert_eq!(byte_to_lsp(byte), (line, character), "engine {engine}");
        let row = format!("| {engine} | {byte} | {line} | {character} |");
        assert!(
            SPEC.contains(&row),
            "the document must contain the reverse row {row:?}"
        );
    }

    // The CRLF variant: one extra byte, and every later line start shifts by one.
    const CRLF: &str = "let s = \"α😀\";\r\n";
    assert_eq!(
        CRLF.len(),
        19,
        "`S2` is 19 bytes: `\\r\\n` is two where `\\n` was one"
    );
    assert_eq!(
        CRLF.match_indices('\n').map(|(i, b)| i + b.len()).next(),
        Some(19),
        "line 1 of `S2` starts at byte 19, one byte later than in `S`"
    );
    assert_eq!(
        SOURCE.match_indices('\n').map(|(i, b)| i + b.len()).next(),
        Some(18),
        "line 1 of `S` starts at byte 18 - the one-byte difference is the whole trap"
    );
}

/// LSPD1-08: the document states its own honesty rules, and the promises it does make are
/// grounded.
///
/// Two halves. First, the document has to carry the sections that make it a specification
/// rather than a description. Second, every tool it names must exist - checked here for the
/// prose as well as the table, because a sentence is exactly where an unearned promise hides.
#[test]
fn lspd1_08_the_document_is_a_specification_and_names_nothing_that_does_not_exist() {
    for heading in [
        "## 1. Startup, handshake, lifecycle",
        "## 2. What LSPD must not assume",
        "## 3. Capability mapping",
        "## 4. Position conversion",
        "## 5. Diagnostics and the syntax gate",
        "## 6. Error code mapping",
    ] {
        assert!(
            SPEC.contains(heading),
            "the document must contain the section {heading:?}"
        );
    }

    let tools = tool_names();
    for line in SPEC.lines() {
        for token in line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            if token.starts_with("ast_") && token.len() > 4 {
                assert!(
                    tools.contains(token),
                    "the document names {token:?}, which TOOLS.md does not define"
                );
            }
        }
    }
}
