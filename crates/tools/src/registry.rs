//! Tool catalogue for the MCP / CLI shells (docs/TOOLS.md §Modes and annotations).
//!
//! Every tool in `docs/TOOLS.md` §Modes has a handler in this crate and is registered here.
//! The three write tools are registered with `mode: Mode::Write`, so they are absent from a
//! read-only listing and present in a write-mode one; reaching them still needs a
//! [`opencrayast_edit::WriteCap`], which only a configuration-issued `WritePermission` can mint.

use crate::context::Mode;

/// MCP-facing annotations (TOOLS.md "Modes and annotations").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolAnnotations {
    /// Hint that the tool does not modify the workspace.
    pub read_only_hint: bool,
    /// Hint that the tool may destroy or overwrite user data.
    pub destructive_hint: bool,
    /// Hint that repeating the call with the same arguments is safe.
    pub idempotent_hint: bool,
    /// Hint that the tool may reach outside the workspace (network, …).
    pub open_world_hint: bool,
}

/// One registered tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolEntry {
    /// Stable tool name (`ast_info`, …).
    pub name: &'static str,
    /// One-line description for `tools/list`.
    pub description: &'static str,
    /// Whether the tool requires write mode to appear / run.
    pub mode: Mode,
    /// Published annotations.
    pub annotations: ToolAnnotations,
    /// JSON Schema object for `tools/list` `inputSchema` (raw JSON text).
    pub input_schema: &'static str,
}

/// Names of the write tools from TOOLS.md §Modes. Kept as a separate list only so the
/// dispatch layer can recognise a write *name* on a read-only server; the catalogue itself
/// decides visibility from [`ToolEntry::mode`], never from this list.
pub const WRITE_TOOL_NAMES: &[&str] = &["ast_edit_apply", "ast_undo", "ast_recover"];

/// Whether `name` is a write tool named in TOOLS.md (even if not yet registered).
pub fn is_write_tool_name(name: &str) -> bool {
    WRITE_TOOL_NAMES.contains(&name)
}

/// Every tool that currently has a handler, in catalogue order.
pub fn tools_catalog() -> &'static [ToolEntry] {
    &CATALOG
}

/// Tools visible under `mode` (write tools omitted in [`Mode::ReadOnly`]).
pub fn tools_for_mode(mode: Mode) -> impl Iterator<Item = &'static ToolEntry> {
    tools_catalog().iter().filter(move |t| match mode {
        Mode::Write => true,
        Mode::ReadOnly => t.mode == Mode::ReadOnly,
    })
}

/// Look up a registered tool by name.
pub fn find_tool(name: &str) -> Option<&'static ToolEntry> {
    tools_catalog().iter().find(|t| t.name == name)
}

const READ: ToolAnnotations = ToolAnnotations {
    read_only_hint: true,
    destructive_hint: false,
    idempotent_hint: true,
    open_world_hint: false,
};

/// Read mode, `readOnlyHint: false`: the tool writes, but only the plan store - never the
/// workspace. `docs/TOOLS.md` §Modes and annotations spells this out, and it is the row that
/// proves `readOnlyHint` and `mode` are two different questions.
const READ_PLAN_STORE_WRITE: ToolAnnotations = ToolAnnotations {
    read_only_hint: false,
    destructive_hint: false,
    idempotent_hint: true,
    open_world_hint: false,
};

/// Write mode, destructive and not idempotent: `ast_edit_apply`, `ast_undo`.
const WRITE_DESTRUCTIVE: ToolAnnotations = ToolAnnotations {
    read_only_hint: false,
    destructive_hint: true,
    idempotent_hint: false,
    open_world_hint: false,
};

/// Write mode, destructive but idempotent: `ast_recover` (TOOLS.md §Modes and annotations).
const WRITE_RECOVER: ToolAnnotations = ToolAnnotations {
    read_only_hint: false,
    destructive_hint: true,
    idempotent_hint: true,
    open_world_hint: false,
};

const EMPTY_OBJECT: &str = r#"{"type":"object","properties":{}}"#;

/// A full plan id is `p-` plus 26 base32 characters (EDIT-MODEL E-15): the write side does
/// not accept a prefix, so the schema says exactly that.
const PLAN_ID_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "plan_id": { "type": "string", "minLength": 28, "maxLength": 28 }
  },
  "required": ["plan_id"]
}"#;

const PREVIEW_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "kind": { "type": "string", "enum": ["rewrite", "symbol"] },
    "language": { "type": "string" },
    "paths": { "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": 64 },
    "pattern": { "type": "string" },
    "replacement": { "type": "string" },
    "operation": {
      "type": "string",
      "enum": ["replace", "replace_body", "delete", "insert_before", "insert_after"]
    },
    "path": { "type": "string" },
    "symbol": { "type": "string" },
    "text": { "type": "string" },
    "note": { "type": "string" }
  },
  "required": ["kind"]
}"#;

const PLAN_SHOW_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "plan_id": { "type": "string", "minLength": 10 },
    "file": { "type": "string" },
    "offset": { "type": "integer", "minimum": 0 },
    "limit": { "type": "integer", "minimum": 1 }
  },
  "required": ["plan_id"]
}"#;

const PLAN_LIST_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "limit": { "type": "integer", "minimum": 1, "maximum": 200 }
  }
}"#;

const OUTLINE_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "path": { "type": "string" },
    "depth": { "type": "integer", "minimum": 1, "maximum": 6 },
    "kinds": { "type": "array", "items": { "type": "string" } },
    "include_docs": { "type": "boolean" },
    "limit": { "type": "integer", "minimum": 1 }
  },
  "required": ["path"]
}"#;

const GET_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "symbol": { "type": "string" },
    "path": { "type": "string" },
    "context_lines": { "type": "integer", "minimum": 0, "maximum": 20 },
    "include_doc": { "type": "boolean" }
  },
  "required": ["symbol"]
}"#;

const SEARCH_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "pattern": { "type": "string" },
    "language": { "type": "string" },
    "paths": { "type": "array", "items": { "type": "string" } },
    "context_lines": { "type": "integer", "minimum": 0, "maximum": 5 },
    "limit": { "type": "integer", "minimum": 1 }
  },
  "required": ["pattern"]
}"#;

const EXPLAIN_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "pattern": { "type": "string" },
    "language": { "type": "string" }
  },
  "required": ["pattern", "language"]
}"#;

const CATALOG: [ToolEntry; 11] = [
    ToolEntry {
        name: "ast_info",
        description: "What is running and what it will allow (mode, workspace, languages, limits).",
        mode: Mode::ReadOnly,
        annotations: READ,
        input_schema: EMPTY_OBJECT,
    },
    ToolEntry {
        name: "ast_outline",
        description: "The skeleton of a file or directory — symbols with kinds, line ranges and \
                      signatures. Lists names only, no source; to read a symbol's text use \
                      ast_get, by symbol name. `limit` is 1..=`limits.max_results` \
                      (`results` in ast_info's limits line; 200 by default).",
        mode: Mode::ReadOnly,
        annotations: READ,
        input_schema: OUTLINE_SCHEMA,
    },
    ToolEntry {
        name: "ast_get",
        description: "One symbol's source, by symbol name (not by path), as fenced data. \
                      Exactly one name per call; for several, call it once per name, or list \
                      candidates with ast_outline first.",
        mode: Mode::ReadOnly,
        annotations: READ,
        input_schema: GET_SCHEMA,
    },
    ToolEntry {
        name: "ast_search",
        description: "Structural search: matches for a pattern in files under `paths`. `paths` \
                      defaults to the workspace root when omitted. `limit` is \
                      1..=`limits.max_results` (`results` in ast_info's limits line; 200 by \
                      default). If the pattern is rejected as unparseable, use \
                      ast_explain_pattern, which reads no files.",
        mode: Mode::ReadOnly,
        annotations: READ,
        input_schema: SEARCH_SCHEMA,
    },
    ToolEntry {
        name: "ast_explain_pattern",
        description: "Explain a pattern: parse it, list captures, and say what would match. Reads \
                      no files — use it when ast_search rejects a pattern as unparseable, since \
                      both fail with the same invalid_pattern code.",
        mode: Mode::ReadOnly,
        annotations: READ,
        input_schema: EXPLAIN_SCHEMA,
    },
    ToolEntry {
        name: "ast_plan_list",
        description: "Stored edit plans for this workspace: id, state, expiry, files, edits.",
        mode: Mode::ReadOnly,
        annotations: READ,
        input_schema: PLAN_LIST_SCHEMA,
    },
    ToolEntry {
        name: "ast_plan_show",
        description: "One plan's summary and diff, paged by hunk. This is the plan **as stored \
                      before it was applied**, not the current content of the files — to read \
                      current code use ast_get or ast_outline. A plan that has already been \
                      applied still renders its original diff. Omit `limit` for every hunk; \
                      `limit: 0` is refused.",
        mode: Mode::ReadOnly,
        annotations: READ,
        input_schema: PLAN_SHOW_SCHEMA,
    },
    ToolEntry {
        // Read mode, `readOnlyHint: false`: it writes the plan store and never the workspace.
        name: "ast_edit_preview",
        description: "Preview an edit as a plan: the diff, the plan id, and what applying it \
                      would change. THIS CALL PERSISTS the plan under \
                      `<workspace>/.opencrayast` — the workspace's files are never modified, but \
                      the returned plan_id only exists because a file was written, which is why \
                      readOnlyHint is false. kind=rewrite needs language, pattern, replacement \
                      and paths; kind=symbol needs path, symbol and operation, plus text for \
                      replace, replace_body, insert_before and insert_after (text must include \
                      the surrounding braces for replace_body, and is unused by delete).",
        mode: Mode::ReadOnly,
        annotations: READ_PLAN_STORE_WRITE,
        input_schema: PREVIEW_SCHEMA,
    },
    ToolEntry {
        name: "ast_edit_apply",
        description: "Apply a stored plan to the workspace. Needs the full plan id; \
                      takes a lock and is not idempotent.",
        mode: Mode::Write,
        annotations: WRITE_DESTRUCTIVE,
        input_schema: PLAN_ID_SCHEMA,
    },
    ToolEntry {
        name: "ast_undo",
        description: "Revert a plan that was applied, from its journal. Needs the full plan id; \
                      refuses when a file has changed since the apply.",
        mode: Mode::Write,
        annotations: WRITE_DESTRUCTIVE,
        input_schema: PLAN_ID_SCHEMA,
    },
    ToolEntry {
        name: "ast_recover",
        description: "Finish or roll back an interrupted apply. Takes no arguments and is \
                      idempotent: running it twice is the same as once.",
        mode: Mode::Write,
        annotations: WRITE_RECOVER,
        input_schema: EMPTY_OBJECT,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The invariant a catalogue actually has to hold: **a listing never hands out more than the
    /// entry declares.** Read mode lists exactly the entries that declare read mode, and write
    /// mode lists everything.
    ///
    /// This is one-directional on purpose. Under write mode every entry is listed, read-mode tools
    /// included - write mode is a superset, not a different set - so "listed under write" says
    /// nothing about what an entry declares. What must never happen is the reverse: a write tool
    /// appearing in a read-mode listing, or a read-mode entry vanishing from one.
    ///
    /// This replaces an older assertion that every catalogue entry had `read_only_hint == true`.
    /// That was wrong, not merely incomplete: `ast_edit_preview` is listed in read mode **and**
    /// has `readOnlyHint: false`, because it writes the plan store and never the workspace
    /// (`docs/TOOLS.md` §Modes and annotations). The old assertion could only be satisfied by
    /// either dropping `ast_edit_preview` from the catalogue or lying about its annotations.
    ///
    /// Mutation self-proof: change `tools_for_mode`'s read arm to filter on
    /// `t.annotations.read_only_hint` instead of `t.mode` - the plausible "simplification" that
    /// the old assertion was quietly built on. `ast_edit_preview` then disappears from read mode,
    /// and both halves of this test go red.
    #[test]
    fn a_listing_never_hands_out_more_than_the_entry_declares() {
        for entry in tools_for_mode(Mode::ReadOnly) {
            assert_eq!(
                entry.mode,
                Mode::ReadOnly,
                "{} declares {} but is listed in read mode",
                entry.name,
                entry.mode.as_str()
            );
        }
        for entry in tools_catalog() {
            let listed_read = tools_for_mode(Mode::ReadOnly).any(|t| t.name == entry.name);
            assert_eq!(
                listed_read,
                entry.mode == Mode::ReadOnly,
                "{} declares {} but its read-mode presence is {listed_read}",
                entry.name,
                entry.mode.as_str()
            );
        }
        assert_eq!(
            tools_for_mode(Mode::Write).count(),
            tools_catalog().len(),
            "write mode is a superset: it lists every entry"
        );
    }

    /// The three edit rows pinned against the contract table in `docs/TOOLS.md` §Modes and
    /// annotations, field by field.
    ///
    /// This is the test that says the catalogue was filled in **from the table** rather than from
    /// memory. Mutation self-proof: change `ast_edit_preview`'s `mode` to `Mode::Write`, or its
    /// `read_only_hint` to `true` because "it is only a read tool" - either one word turns this
    /// red, and neither would be caught by a test that only compared modes to each other.
    #[test]
    fn the_edit_rows_match_the_tools_md_table() {
        // (name, mode, readOnlyHint, destructiveHint, idempotentHint, openWorldHint)
        let expected: [(&str, Mode, bool, bool, bool, bool); 3] = [
            ("ast_plan_list", Mode::ReadOnly, true, false, true, false),
            ("ast_plan_show", Mode::ReadOnly, true, false, true, false),
            // Read mode, but it writes the plan store - the row that proves `mode` and
            // `readOnlyHint` are different questions.
            (
                "ast_edit_preview",
                Mode::ReadOnly,
                false,
                false,
                true,
                false,
            ),
        ];
        for (name, mode, read_only, destructive, idempotent, open_world) in expected {
            let entry = find_tool(name).unwrap_or_else(|| panic!("{name} is not catalogued"));
            assert_eq!(entry.mode, mode, "{name} mode");
            assert_eq!(
                entry.annotations.read_only_hint, read_only,
                "{name} readOnlyHint"
            );
            assert_eq!(
                entry.annotations.destructive_hint, destructive,
                "{name} destructiveHint"
            );
            assert_eq!(
                entry.annotations.idempotent_hint, idempotent,
                "{name} idempotentHint"
            );
            assert_eq!(
                entry.annotations.open_world_hint, open_world,
                "{name} openWorldHint"
            );
        }
    }

    /// `readOnlyHint` is a claim about the **workspace**, not about the mode. A tool may be
    /// listed in read mode and still write something - `ast_edit_preview` writes the plan store.
    /// What no catalogue entry may do is claim to destroy user data.
    #[test]
    fn annotations_describe_the_workspace_not_the_mode() {
        for entry in tools_catalog() {
            // A tool that touches the workspace must be a write-mode entry AND say it destroys;
            // a tool that does not must be read-mode and say it is safe. The two fields answer
            // different questions (`ast_edit_preview` is read-mode with `readOnlyHint: false`),
            // so neither is derived from the other.
            if entry.annotations.destructive_hint {
                assert_eq!(
                    entry.mode,
                    Mode::Write,
                    "{} claims to destroy data but is not a write tool",
                    entry.name
                );
            }
            if entry.annotations.read_only_hint {
                assert_eq!(
                    entry.mode,
                    Mode::ReadOnly,
                    "{} claims read-only but is a write tool",
                    entry.name
                );
            }
        }
        // The row that proves the two fields are independent, pinned so it cannot be "fixed" by
        // making the annotation match the mode.
        let preview = find_tool("ast_edit_preview").expect("ast_edit_preview is catalogued");
        assert_eq!(preview.mode, Mode::ReadOnly);
        assert!(
            !preview.annotations.read_only_hint,
            "ast_edit_preview writes the plan store, so readOnlyHint is false"
        );
    }

    #[test]
    fn write_tool_names_are_catalogued_as_write_mode() {
        for name in WRITE_TOOL_NAMES {
            assert!(is_write_tool_name(name));
            let entry = find_tool(name)
                .unwrap_or_else(|| panic!("{name} has a handler and must be catalogued"));
            assert_eq!(
                entry.mode,
                Mode::Write,
                "{name} must be a write-mode entry, or a write tool would be listed read-only"
            );
            assert!(
                entry.annotations.destructive_hint,
                "{name} changes the workspace and must say so"
            );
        }
    }

    #[test]
    fn every_entry_has_object_shaped_input_schema_text() {
        for t in tools_catalog() {
            assert!(
                t.input_schema.contains(r#""type":"object""#)
                    || t.input_schema.contains(r#""type": "object""#),
                "{} schema must declare type object",
                t.name
            );
        }
    }

    /// `ast_edit_preview` must not publish a `rule` property.
    ///
    /// A published property is a promise the model is entitled to build on, and this one could
    /// never be kept: `PREVIEW_SCHEMA` typed `rule` as a **string** while a `Rule` is a structured
    /// object with no string form, and the `tools/call` path hard-refuses any non-null `rule`
    /// anyway. So a model that read the schema, sent a string, and was refused would have no way
    /// to tell that the schema — not its guess — was wrong.
    ///
    /// It is the same defect as on `ast_search`, where the refusal is kept (silently dropping an
    /// argument is worse) and the promise is deleted. Here the promise is deleted and the refusal
    /// stays for a caller that sends the field from memory.
    ///
    /// Mutation self-proof: re-add `"rule": { "type": "string" }` to `PREVIEW_SCHEMA` — red.
    #[test]
    fn ast_edit_preview_does_not_publish_a_rule_property() {
        let schema = find_tool("ast_edit_preview")
            .expect("ast_edit_preview is catalogued")
            .input_schema;
        assert!(
            !schema.contains("\"rule\""),
            "PREVIEW_SCHEMA publishes `rule` as a string, which has no parse target: {schema}"
        );
    }

    /// No schema may publish a `maxLength` on `note`.
    ///
    /// The bound it published (200) was wrong by five times — the real one is
    /// `limits.note_max_bytes`, 1024 by default and operator-configurable — so it both refused
    /// notes that are fine and disagreed with the enforcement. The honest move for a value the
    /// operator can change is to publish no bound: `ast_info`'s limits line is where a configured
    /// limit becomes visible at runtime.
    ///
    /// Mutation self-proof: put `"maxLength": 200` back on `note` — red.
    #[test]
    fn ast_edit_preview_publishes_no_hardcoded_note_bound() {
        let schema = find_tool("ast_edit_preview")
            .expect("ast_edit_preview is catalogued")
            .input_schema;
        assert!(
            !schema.contains("maxLength"),
            "`note` is bounded by the operator-configurable limits.note_max_bytes, so no \
             hardcoded bound may appear in the schema: {schema}"
        );
    }

    /// The description of `ast_edit_preview` must say that the call persists a plan, and where.
    ///
    /// `readOnlyHint: false` is the annotation that says it; nothing said *what* is written or
    /// *where*. The path — the state directory — appeared in no description, in no `Next:`, and
    /// (until this change) was even described in `docs/TOOLS.md` as "never the workspace", which is
    /// true of workspace *content* and false of the tree: the store lands inside the user's tree. A
    /// client that honours `destructiveHint: false` to skip confirmation deserves to know the
    /// call creates a file before it makes one.
    #[test]
    fn ast_edit_preview_discloses_that_it_persists_a_plan_and_where() {
        let entry = find_tool("ast_edit_preview").expect("ast_edit_preview is catalogued");
        assert!(
            entry.description.contains(".opencrayast"),
            "the description must name where the plan is written: {}",
            entry.description
        );
        assert!(
            entry.description.contains("PERSIST"),
            "the description must say the call persists rather than implying a dry run: {}",
            entry.description
        );
    }

    /// The descriptions of the four tools that overlap must say which one to reach for.
    ///
    /// Each pair failed in a way no retry could reveal: `ast_search` and `ast_explain_pattern`
    /// take the same two arguments and answer a byte-identical `invalid_pattern`; `ast_get` and
    /// `ast_outline` are both "give me this file's symbols"; `ast_plan_show` returns content that
    /// is stored, pre-apply, and will hand a model yesterday's source without saying so. A
    /// disambiguating clause in the description is the only place the model sees before it calls.
    #[test]
    fn overlapping_descriptions_disambiguate_themselves() {
        let d = |name: &str| {
            find_tool(name)
                .unwrap_or_else(|| panic!("{name} is catalogued"))
                .description
                .to_string()
        };
        // search vs explain: which one reads files.
        assert!(d("ast_search").contains("in files"));
        assert!(d("ast_explain_pattern").contains("Reads no files"));
        // get vs outline: name vs path, and whether source comes back.
        assert!(d("ast_get").contains("symbol name (not by path)"));
        assert!(d("ast_outline").contains("no source"));
        // plan_show vs get: stored content, not the current file.
        assert!(d("ast_plan_show").contains("not the current content"));
        // ast_search has no rewrite path; the old one-liner claimed one and sent models looking
        // for a second entry point that does not exist.
        assert!(
            !d("ast_search").contains("rewrite"),
            "ast_search has no rewrite path: {}",
            d("ast_search")
        );
    }
}
