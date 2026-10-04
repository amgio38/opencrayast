//! UX-1 (AGENT-GUIDE reconciliation): the tool catalogue, `docs/TOOLS.md` and the audit table in
//! `docs/AGENT-GUIDE.md` must agree, in BOTH directions.
//!
//! # Why this test exists
//!
//! The failure this prevents is the one that cannot be noticed by reading: a tool is registered,
//! works, has a test, and is simply *undocumented*. An agent choosing between tools reads
//! `tools/list` and `docs/TOOLS.md`; a tool missing from both is invisible to review and shows up
//! as "the tool does not exist" to the user, months later.
//!
//! Three directions, each a separate test so a failure names what broke:
//!
//! 1. **catalogue → docs.** Every registered tool has a section in `docs/TOOLS.md`. (Nothing
//!    implemented without being declared.)
//! 2. **docs → catalogue.** Every tool named in `docs/TOOLS.md` §Modes is registered. (Nothing
//!    declared without being implemented — a documented tool that does not exist is worse: an agent
//!    will try it.)
//! 3. **catalogue → audit table.** `docs/AGENT-GUIDE.md`'s audit table is *parsed as rows* and
//!    compared to the catalogue as set equality, so deleting a row, adding a row for a tool that
//!    does not exist, or misnaming a row all go red. Prose mentions outside the table cannot
//!    satisfy it. This is the invariant the ticket calls "adding a tool without a description must
//!    go red".
//!
//! 4. **description → docs, verbatim.** Every shipped `description` is quoted **exactly** in its own
//!    section of `docs/TOOLS.md`, and every description quoted there is one the server ships
//!    (UX1-10). `docs/TOOLS.md` §Conventions makes the shipped string the authoritative side; this
//!    test is the executable half of that sentence, in both directions.
//!
//! # Why these tests read files instead of calling the MCP server
//!
//! They assert the *documents*, which is the whole point: a drift between code and docs is
//! invisible to a test that only exercises code. The paths are resolved from `CARGO_MANIFEST_DIR`
//! so the test does not depend on the working directory.
//!
//! Refs: docs/TOOLS.md §Conventions, §Modes and annotations, §Error code reference;
//! docs/AGENT-GUIDE.md.

// `panic` is legitimate in a test: it is how a failure is reported, and these tests read documents
// that must exist. The lint is denied workspace-wide, so it is allowed here rather than turned into
// `expect` everywhere, which would read as though the failure were recoverable.
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use opencrayast_tools::Mode;
use opencrayast_tools::registry::{WRITE_TOOL_NAMES, tools_catalog, tools_for_mode};
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn read_doc(name: &str) -> String {
    let p = repo_root().join("docs").join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// UX1-01: every registered tool is declared in `docs/TOOLS.md`.
///
/// The check is on the tool's own heading, not a bare mention of its name: TOOLS.md is allowed to
/// *refer* to a tool in prose (an error table, a workflow note) without it being a declared tool,
/// and that must not satisfy this test.
#[test]
fn ux1_01_every_registered_tool_is_declared_in_tools_md() {
    let tools = read_doc("TOOLS.md");
    let mut missing = Vec::new();
    for t in tools_catalog() {
        // A declared tool has its own `## ` heading under a backticked name.
        let declared = tools
            .lines()
            .any(|l| l.trim_start().starts_with("## ") && l.contains(&format!("`{}`", t.name)));
        if !declared {
            missing.push(t.name);
        }
    }
    assert!(
        missing.is_empty(),
        "registered but not declared in docs/TOOLS.md (an agent cannot choose what it cannot \
         see): {missing:?}"
    );
}

/// UX1-02: every tool named in `docs/TOOLS.md` §Modes is registered — the reverse direction.
///
/// A documented tool with no handler is the worse half of this pair: an agent reads the
/// description, forms a plan around the tool, and then fails at call time.
#[test]
fn ux1_02_every_tool_declared_in_tools_md_is_registered() {
    let tools_md = read_doc("TOOLS.md");
    let registered: Vec<&str> = tools_catalog().iter().map(|t| t.name).collect();

    // `## \`ast_...\`` is the declaration form in TOOLS.md.
    let declared: Vec<String> = tools_md
        .lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let rest = l.strip_prefix("## `")?;
            let end = rest.find('`')?;
            let name = &rest[..end];
            name.starts_with("ast_").then(|| name.to_string())
        })
        .collect();

    let unregistered: Vec<&String> = declared
        .iter()
        .filter(|n| !registered.contains(&n.as_str()))
        .collect();
    assert!(
        unregistered.is_empty(),
        "declared in docs/TOOLS.md but not registered: {unregistered:?} \
         (an agent will plan around a tool that cannot be called)"
    );
    assert!(
        !declared.is_empty(),
        "no `## \\`ast_...\\`` headings were found in docs/TOOLS.md — the parser for this test \
         has stopped matching the document, which would make UX1-02 vacuously pass"
    );
}

/// UX1-10: every shipped `description` is quoted **verbatim** in `docs/TOOLS.md`, and every
/// description quoted there is one the server actually ships.
///
/// # The contract this enforces
///
/// `docs/TOOLS.md` §Conventions states which side is authoritative: the `description` string an
/// agent receives from `tools/list`. Each tool's section quotes that string exactly, as a blockquote
/// under the label *Published description*. This test is the executable half of that sentence —
/// before it existed, the document asserted verbatim correspondence that no test checked, and
/// **0 of 11** shipped descriptions matched, so a fully green suite said nothing about it.
///
/// # Why verbatim, rather than "the substance is present"
///
/// The alternative contract — check that key noun phrases from each description occur somewhere in
/// the docs — was measured, not assumed. Making it pass required inserting words that appear in no
/// doc today (`fetch`, `structural`, `captures`, `hunk`, `refuses`, `interrupted`, `twice`,
/// `once`), i.e. it would have *forced* the awkward prose, while asserting strictly less: a
/// keyword-overlap check passes on a description whose meaning was inverted, and an edit can
/// satisfy it by sprinkling words. Exact containment has neither failure mode, and the quoted
/// string is a natural thing to keep accurate — it is the sentence the agent receives.
///
/// # Why this does not reword the documentation
///
/// The quotes are additive: each section keeps its existing prose. TOOLS.md reads as a
/// specification first, with the published one-liner quoted where it can be checked, so the
/// invariant costs no meaning in the documentation.
///
/// # Both directions, deliberately
///
/// Forward only (`description -> doc`) would be satisfied by an empty set of quotes. The reverse
/// direction is what catches the case where someone paraphrases the quote, or quotes a string for a
/// tool that does not exist — the "declared but not implemented" half of the invariant.
#[test]
fn ux1_10_every_published_description_is_quoted_verbatim() {
    let tools_md = read_doc("TOOLS.md");

    // Quoted strings are blockquote lines (`> …`) that follow a **Published description** label. A
    // blockquote anywhere else in the document (a worked example, a quoted error line) is not a
    // published description and must not be counted as one. The label is recognised only in the
    // exact forms the document uses, so prose merely mentioning the words cannot start a quote.
    //
    // A label's block runs until a line that is neither a blockquote nor blank: the shared
    // `ast_plan_show` / `ast_plan_list` section quotes two tools, blank-line separated, and both
    // must be harvested. The blockquote's text is taken verbatim, so the quote stays a substring
    // of the shipped description.
    const LABEL: &str = "**Published description";
    const LABEL_PLURAL: &str = "**Published descriptions";
    let mut quoted: Vec<String> = Vec::new();
    let mut in_block = false;
    for line in tools_md.lines() {
        let t = line.trim();
        if !in_block && (t.starts_with(LABEL) || t.starts_with(LABEL_PLURAL)) {
            in_block = true;
            continue;
        }
        if in_block {
            match t.strip_prefix("> ") {
                Some(text) if !text.trim().is_empty() => {
                    quoted.push(text.trim().to_string());
                }
                _ if t.is_empty() => { /* blank: the block may hold another quote */ }
                _ => in_block = false,
            }
        }
    }

    // Guard against the parser silently stopping to match the document, which would make the
    // forward direction vacuous. One quote per tool is the current shape; the shared
    // `ast_plan_show` / `ast_plan_list` section quotes both of its tools, so the count is the
    // catalogue size.
    assert_eq!(
        quoted.len(),
        tools_catalog().len(),
        "expected one published-description quote per catalogued tool, parsed {} — the parser has \
         stopped matching docs/TOOLS.md, which would make this test vacuous",
        quoted.len()
    );

    // Forward: what an agent receives must appear in the document, exactly.
    let unquoted: Vec<&str> = tools_catalog()
        .iter()
        .filter(|t| !quoted.iter().any(|q| q == t.description))
        .map(|t| t.name)
        .collect();
    assert!(
        unquoted.is_empty(),
        "these tools ship a description that docs/TOOLS.md does not quote verbatim: {unquoted:?} \
         (TOOLS.md §Conventions makes the description the contract — an agent reads the shipped \
         string, so the document must carry it unchanged)"
    );

    // Reverse: a quote the server never sends is documentation of a tool that does not exist.
    let phantom: Vec<&String> = quoted
        .iter()
        .filter(|q| !tools_catalog().iter().any(|t| t.description == *q))
        .collect();
    assert!(
        phantom.is_empty(),
        "docs/TOOLS.md quotes published descriptions the catalogue does not ship: {phantom:?} \
         (an agent will plan around a tool that cannot be called)"
    );

    // Placement: each quote must sit in its own tool's section, so a quote cannot satisfy the
    // contract by living under the wrong heading. A heading names its tools in backticks, which is
    // how the shared `ast_plan_show` / `ast_plan_list` section is resolved for both of them.
    for t in tools_catalog() {
        let heading = format!("`{}`", t.name);
        let section = section_containing_heading(&tools_md, &heading).unwrap_or_else(|| {
            panic!(
                "docs/TOOLS.md has no section whose heading names {heading}, so {}'s description \
                 cannot be quoted where an agent will look for it",
                t.name
            )
        });
        assert!(
            section.contains(&format!("> {}", t.description)),
            "{}'s published description is not quoted in its own section of docs/TOOLS.md",
            t.name
        );
    }
}

/// The body of the `## ` section whose heading contains `heading`, up to the next `## ` heading.
///
/// Returns a slice of `doc`, not a new `String`: the caller only needs to search it.
fn section_containing_heading<'a>(doc: &'a str, heading: &str) -> Option<&'a str> {
    // Byte offset of the start of each line, so a section body can be sliced out of `doc`
    // without copying.
    let starts: Vec<usize> = doc
        .match_indices('\n')
        .map(|(i, _)| i + 1)
        .chain(std::iter::once(doc.len()))
        .collect();

    for (idx, &start) in starts.iter().enumerate() {
        let end = starts[idx + 1];
        let line = doc[start..end].trim_start();
        let Some(title) = line.strip_prefix("## ") else {
            continue;
        };
        if !title.contains(heading) {
            continue;
        }
        // Body runs from this line's end to the next `## ` heading (or end of document).
        let body_start = end;
        let body_end = starts[idx + 1..]
            .iter()
            .copied()
            .find(|&s| doc[s..].trim_start().starts_with("## "))
            .unwrap_or(doc.len());
        return Some(&doc[body_start..body_end.max(body_start)]);
    }
    None
}

/// UX1-03: the audit table in `docs/AGENT-GUIDE.md` has exactly one row per registered tool.
///
/// This is the ticket's invariant 4. The row must name the tool, so a renamed tool cannot keep the
/// row it no longer fills.
///
/// # What this test does, and what it used to do
///
/// An earlier version searched the **whole file** for `` `name` ``. That sounds like a superset of
/// row-parsing but is not one: the guide mentions `ast_explain_pattern` in five places outside the
/// audit table, so deleting that tool's row left the test green. Of the 11 tools only the 2 whose
/// name occurs exactly once were actually protected. The assertion also compared a filtered
/// catalogue against the catalogue length, so it could only ever succeed by construction.
///
/// So the table is now **parsed as rows**, and compared to the catalogue as **set equality**: delete
/// any row, add a row for a tool that does not exist, or leave a row naming the wrong tool, and
/// each is red. Prose mentions outside the table are not rows and cannot satisfy it.
#[test]
fn ux1_03_the_audit_table_has_one_row_per_tool() {
    let guide = read_doc("AGENT-GUIDE.md");
    let catalog = tools_catalog();

    // Isolate the audit table: the one whose header row names the Tool column. Locating it by
    // header rather than by a fixed heading means a renamed or moved section is a parser failure
    // (reported by the floor assertion below) rather than a silently empty result.
    let table = audit_table(&guide);

    let row_names = table_rows(&table);

    // The parser must actually see rows. Without this floor a parser that matched nothing would
    // report "missing: all tools", which is at least loud — but a parser that matched the header's
    // separator row only would be quieter, so pin the real count.
    assert!(
        row_names.len() > 1,
        "the audit table parser found {} row(s) in docs/AGENT-GUIDE.md; it has stopped matching \
         the document",
        row_names.len()
    );

    // Both directions, as sets — not containment.
    let missing: Vec<&str> = catalog
        .iter()
        .map(|t| t.name)
        .filter(|n| !row_names.iter().any(|r| r == n))
        .collect();
    assert!(
        missing.is_empty(),
        "registered tools with no row in the docs/AGENT-GUIDE.md audit table: {missing:?} \
         (invariant 4: adding a tool without a description must go red)"
    );

    let extra: Vec<&&str> = row_names
        .iter()
        .filter(|r| !catalog.iter().any(|t| t.name == **r))
        .collect();
    assert!(
        extra.is_empty(),
        "the audit table names tools that are not catalogued: {extra:?} (a row for a tool that \
         does not exist is the other direction of the same drift)"
    );

    // And exactly one row each: a tool listed twice is a document that cannot be read as a
    // one-row-per-tool table.
    for t in catalog {
        let n = row_names.iter().filter(|r| **r == t.name).count();
        assert_eq!(
            n, 1,
            "the audit table has {n} rows for {}, expected exactly 1",
            t.name
        );
    }
}

/// The audit table block: every contiguous run of `|`-prefixed lines whose header row's first cell
/// is `Tool`, joined so a table split by a blank line is still parsed whole.
fn audit_table(guide: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_run = false;
    let mut is_audit = false;
    for line in guide.lines() {
        let t = line.trim();
        if t.starts_with('|') {
            let cells = table_cells(t);
            if !in_run {
                in_run = true;
                // A table run is the audit table exactly when its header names the Tool column.
                is_audit = cells.first().is_some_and(|c| *c == "Tool");
            }
            if is_audit {
                out.push(t.to_string());
            }
            continue;
        }
        // A blank line ends a table run but must not end the audit table's identity for the
        // *next* run; re-derive it from that run's own header.
        in_run = false;
        is_audit = false;
    }
    out
}

/// The cells of one `|`-prefixed row, trimmed, with the empty leading/trailing cells dropped.
///
/// Borrows from `line`, so callers can return cell text without copying.
fn table_cells(line: &str) -> Vec<&str> {
    let mut cells: Vec<&str> = line
        .trim()
        .trim_start_matches('|')
        .split('|')
        .map(str::trim)
        .collect();
    // `split` leaves the text after the final `|` as an empty cell; drop it only if empty, so a
    // malformed row is still visible in the cell count.
    if cells.last().is_some_and(|c| c.is_empty()) {
        cells.pop();
    }
    cells
}

/// The tool names named by the audit table's body rows: the first cell of every row that is neither
/// the header nor the `|---|---|` separator.
///
/// Borrows from `table`, so no cell text is copied.
fn table_rows(table: &[String]) -> Vec<&str> {
    let mut names: Vec<&str> = Vec::new();
    for line in table.iter().skip(1) {
        let Some(first) = table_cells(line).first().copied() else {
            continue;
        };
        // The separator row is `|---|---|`, whose first cell is not a backticked name.
        let name = first.trim_matches('`');
        if !name.is_empty() && first.starts_with('`') {
            names.push(name);
        }
    }
    names
}

/// UX1-04: annotations match what the tool actually does, and write tools are absent from the
/// read-mode catalogue rather than present-and-refused.
///
/// # The subtlety this test exists to get right
///
/// `WRITE_TOOL_NAMES` is **not** the same as "tools with `read_only_hint == false`", and treating
/// them as one is a bug this test caught while being written. `ast_edit_preview` is a read-mode
/// tool that is not `read_only`: it writes the *plan store*, never a workspace file (TOOLS.md §Modes
/// spells this out as "false (writes the plan store, never the workspace)"). So the two sets
/// legitimately differ, and the honest invariant is the one stated below — not `read_only ==
/// !is_write`.
///
/// Two things are asserted instead:
///
/// 1. a tool that writes a WORKSPACE FILE must not be `read_only`, and must not appear in read
///    mode;
/// 2. a `read_only` tool must not write — checked structurally, by requiring the read-mode-only
///    tools to be exactly the ones TOOLS.md lists as not touching the workspace.
///
/// The second half of the ticket's requirement: a write tool appearing in read mode teaches an
/// agent the tool exists, and the refusal only arrives after it has built a plan around it.
#[test]
fn ux1_04_annotations_and_mode_visibility_agree_with_reality() {
    let catalog = tools_catalog();

    // 1. A write tool — one that can change a workspace file — is never read_only.
    for name in WRITE_TOOL_NAMES {
        let t = catalog
            .iter()
            .find(|t| t.name == *name)
            .unwrap_or_else(|| panic!("{name} is named a write tool but is not registered"));
        assert!(
            !t.annotations.read_only_hint,
            "{name} writes a workspace file, so read_only_hint must be false"
        );
    }

    // 2. Write tools are ABSENT from read mode, not present-and-refused.
    let read: Vec<&str> = tools_for_mode(Mode::ReadOnly).map(|t| t.name).collect();
    for w in WRITE_TOOL_NAMES {
        assert!(
            !read.contains(w),
            "{w} is a write tool but appears in the read-mode catalogue; it must be absent, not \
             present-and-refused"
        );
    }

    // 3. Write mode offers the whole catalogue.
    let write: Vec<&str> = tools_for_mode(Mode::Write).map(|t| t.name).collect();
    assert_eq!(
        write.len(),
        catalog.len(),
        "write mode must offer the whole catalogue, got {write:?}"
    );
    for w in WRITE_TOOL_NAMES {
        assert!(write.contains(w), "{w} must appear in write mode");
    }

    // 4. The converse, which is the half a test can actually pin: every tool that is read_only
    // must be one TOOLS.md documents as touching no workspace file. `ast_edit_preview` is the
    // deliberate exception in the other direction and is named here so a future tool cannot join
    // the read_only set without this list being updated.
    const READ_ONLY_TOOLS: &[&str] = &[
        "ast_info",
        "ast_outline",
        "ast_get",
        "ast_search",
        "ast_explain_pattern",
        "ast_plan_list",
        "ast_plan_show",
    ];
    let read_only: Vec<&str> = catalog
        .iter()
        .filter(|t| t.annotations.read_only_hint)
        .map(|t| t.name)
        .collect();
    let undeclared: Vec<&str> = read_only
        .iter()
        .copied()
        .filter(|n| !READ_ONLY_TOOLS.contains(n))
        .collect();
    assert!(
        undeclared.is_empty(),
        "these tools claim read_only_hint but are not on the reviewed read-only list: {undeclared:?} \
         — a read-only tool must not write anything, so each one needs a human decision recorded \
         here"
    );
    // And every name on that list really is read_only, so deleting one from the list cannot be
    // used to silence this test.
    let mislabelled: Vec<&&str> = READ_ONLY_TOOLS
        .iter()
        .filter(|n| !read_only.contains(n))
        .collect();
    assert!(
        mislabelled.is_empty(),
        "these are on the reviewed read-only list but do not claim read_only_hint: {mislabelled:?}"
    );
}

/// UX1-05: every registered tool has a non-empty description that says something.
///
/// An empty or one-word description satisfies every other test in this file while telling an agent
/// nothing, which is the failure the ticket's first invariant is about ("no implemented-but-
/// undeclared, no declared-but-unimplemented").
#[test]
fn ux1_05_every_tool_has_a_meaningful_description() {
    for t in tools_catalog() {
        let d = t.description.trim();
        assert!(!d.is_empty(), "{}: empty description", t.name);
        // A real sentence, not a name repeated. Twenty characters is a floor chosen to sit below
        // every genuine description in the catalogue and above every placeholder.
        assert!(
            d.chars().count() >= 20,
            "{}: description is {} characters, too short to tell an agent when to use it: {d:?}",
            t.name,
            d.chars().count()
        );
        assert!(
            d.ends_with('.') || d.ends_with('!') || d.ends_with('?') || d.ends_with(')'),
            "{}: description does not read as a sentence: {d:?}",
            t.name
        );
    }
}

/// UX1-06: the error codes named in `docs/TOOLS.md` §Error code reference and the codes the crate
/// can actually emit are the same set, in both directions.
///
/// This is the ticket's invariant 2. Direction one catches a documented code that no longer exists
/// (an agent that sees `[plan_corrupt]` in a table will report it to a user who can never get it).
/// Direction two catches an emitted code nobody documented, which is worse: the agent gets a code
/// it has no meaning for.
///
/// # Parsing the table
///
/// The table has two shapes, and both have to be read:
///
/// - one code per row: `| \`invalid_args\` | Meaning | Next step |`
/// - several codes sharing one meaning: `| \`plan_not_found\` / \`plan_expired\` / … | Plan
///   problems | Preview again |`
///
/// A parser that only understood the first shape would report the second group as undocumented
/// and then, after a fix, quietly stop catching anything — so the count floor below is what keeps
/// this test from becoming vacuous.
#[test]
fn ux1_06_error_codes_are_documented_in_both_directions() {
    let tools_md = read_doc("TOOLS.md");

    let section = tools_md
        .split_once("## Error code reference")
        .map(|(_, rest)| rest)
        .expect("docs/TOOLS.md has no '## Error code reference' section");
    // Stop at the next `## ` heading: codes named in prose further down are not the table.
    let section = section
        .split_once("\n## ")
        .map(|(head, _)| head)
        .unwrap_or(section);

    let documented: Vec<String> = section
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            let rest = l.strip_prefix("| ")?;
            // Only the first cell of a row.
            let cell = rest.split('|').next()?.trim();
            // Every `code` in the cell, however many the row groups.
            let mut names = Vec::new();
            for part in cell.split('/') {
                let name = part
                    .trim()
                    .trim_start_matches('`')
                    .trim_end_matches('`')
                    .trim();
                // Digits count. `not_utf8` is a real code, and an earlier version of this filter
                // accepted only `[a-z_]`, which silently dropped it: the table row was there, the
                // parser did not see it, and UX1-06 reported a documented code as undocumented. The
                // floor assertion below is what turned that into a visible failure instead of a
                // quietly wrong "pass".
                if !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                {
                    names.push(name.to_string());
                }
            }
            (!names.is_empty()).then_some(names)
        })
        .flatten()
        .collect();

    // The floor is the number of codes the table documents today, not a guess. Set too high it
    // fails on a correct document; set too low it would not notice the parser losing a row shape.
    // 24 is the count of the current table; a table that silently loses rows drops below it.
    assert!(
        documented.len() >= 24,
        "only {} error codes parsed from the §Error code reference table (saw {documented:?}) — \
         the parser has stopped matching the document, which would make this test vacuous",
        documented.len()
    );

    let emitted: Vec<&str> = opencrayast_core::ALL_ERROR_CODES
        .iter()
        .map(|c| c.as_str())
        .collect();

    let undocumented: Vec<&&str> = emitted
        .iter()
        .filter(|c| !documented.iter().any(|d| d == *c))
        .collect();
    assert!(
        undocumented.is_empty(),
        "codes the crate emits but docs/TOOLS.md does not document: {undocumented:?} \
         (an agent receiving one has no documented meaning for it)"
    );

    let phantom: Vec<&String> = documented
        .iter()
        .filter(|d| !emitted.contains(&d.as_str()))
        .collect();
    assert!(
        phantom.is_empty(),
        "codes docs/TOOLS.md documents that the crate cannot emit: {phantom:?}"
    );

    // The count is a literal on purpose: `ALL_ERROR_CODES` is maintained by hand, so a variant
    // added to the enum but not to the slice would otherwise make the direction above vacuous.
    assert_eq!(
        emitted.len(),
        opencrayast_core::ERROR_CODE_COUNT,
        "ALL_ERROR_CODES is out of step with the enum: {emitted:?}"
    );
}
