//! Spec for ISSUE-EDIT-10: the six edit tool handlers (`docs/TOOLS.md`; EDIT10-01..10).
//!
//! Unix only, like every spec that opens a file through the boundary: the store and the journal
//! check owner and mode bits, and `Boundary::open_read` answers `unsupported_target` elsewhere.
//! Nothing in `edit.rs` is unix-specific - it deserializes, calls L3 and renders - so this gate
//! is about the reader, not the logic.
//!
//! ## These tests call the shipping path
//!
//! Every assertion here goes through the public handler - `ast_edit_preview(ctx, &args)` and
//! friends - and compares the **string the handler returns**. There is no second renderer to
//! compare against and no formatter under test that the client never sees: the sanitiser, the
//! byte cap, the truncation notice and the escaped-character report are all in `edit.rs`, and
//! these are the bytes a client receives. That is deliberate: a golden test of a *reimplementation*
//! of the layout would pass while the real output rotted, which is exactly how the output
//! sanitising got left in a test double on another ticket.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_core::boundary::{Boundary, BoundaryConfig};
use opencrayast_core::limits::Limits;
use opencrayast_edit::{Clock, JournalStore, PlanStore};
use opencrayast_tools::context::{Mode, ToolContext};
use opencrayast_tools::registry::{WRITE_TOOL_NAMES, tools_for_mode};
use opencrayast_tools::{
    ApplyArgs, EditTools, PlanListArgs, PlanShowArgs, PreviewArgs, UndoArgs, ast_edit_apply,
    ast_edit_preview, ast_plan_list, ast_plan_show, ast_recover, ast_undo,
};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const WS: &str = "w-00112233445566778899aabbccddeeff";

/// A clock that does not move, so `expires HH:MM` is a golden value rather than a race.
struct FakeClock(AtomicU64);
impl Clock for FakeClock {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// A workspace with a boundary, both stores and a read or write [`EditTools`].
struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
    state: PathBuf,
    tools: ToolContext,
    plans: PlanStore,
    journals: JournalStore,
}

impl Fx {
    fn new(mode: Mode) -> Fx {
        Fx::with_limits(mode, |_| {})
    }

    /// A fixture whose limits are tweaked, for the cases that are about the output cap itself.
    fn with_limits(mode: Mode, tweak: impl FnOnce(&mut Limits)) -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        fs::create_dir_all(root.join("src")).unwrap();
        let state = dir.path().join("state");
        let mut limits = Limits::default();
        tweak(&mut limits);
        let clock: Arc<dyn Clock> = Arc::new(FakeClock(AtomicU64::new(1_000_000)));
        let plans = PlanStore::open(&state, WS, limits.clone(), clock.clone()).unwrap();
        let journals = JournalStore::open(&state, WS, limits.clone(), clock).unwrap();
        // A write fixture needs a capability, and the only way to get one is the way production
        // does: parse configuration that says `allow_write = true` and mint from the permission it
        // hands out. There is no test-only bypass, by design (WCAP-1).
        let write = match mode {
            Mode::ReadOnly => None,
            Mode::Write => {
                opencrayast_core::config::Settings::parse("[policy]\nallow_write = true\n")
                    .expect("the fixture's own configuration parses")
                    .write_permission()
                    .map(opencrayast_tools::WriteCap::mint)
            }
        };
        Fx {
            tools: ToolContext {
                boundary: Boundary::new(BoundaryConfig::new(root.clone(), limits.clone())).unwrap(),
                limits,
                mode,
                write,
                version: "0.20261002.1".into(),
                workspace_id: WS.into(),
                respect_gitignore: true,
                extra_ignore: Vec::new(),
                config_source: Default::default(),
            },
            plans,
            journals,
            _dir: dir,
            root,
            state,
        }
    }

    fn write(&self, rel: &str, content: &str) {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn edit(&self) -> EditTools<'_> {
        EditTools {
            tools: &self.tools,
            plans: &self.plans,
            journals: &self.journals,
            state_dir: &self.state,
            lock_timeout: std::time::Duration::from_millis(200),
        }
    }

    /// Preview a rewrite and return the stored plan id.
    fn preview_log_rewrite(&self) -> String {
        let source = "log(1);\n";
        self.write("src/a.ts", source);
        let out = ast_edit_preview(
            &self.edit(),
            &PreviewArgs {
                kind: "rewrite".into(),
                language: Some("typescript".into()),
                paths: Some(vec!["src/a.ts".into()]),
                pattern: Some("log($$$ARGS)".into()),
                replacement: Some("log2($$$ARGS)".into()),
                note: None,
                ..PreviewArgs::default()
            },
        )
        .unwrap();
        // The id is the first token of the first line.
        out.split_whitespace().nth(1).unwrap().to_string()
    }
}

fn preview_args(kind: &str) -> PreviewArgs {
    match kind {
        "rewrite" => PreviewArgs {
            kind: "rewrite".into(),
            language: Some("typescript".into()),
            paths: Some(vec!["src/a.ts".into()]),
            pattern: Some("log($$$ARGS)".into()),
            replacement: Some("log2($$$ARGS)".into()),
            ..PreviewArgs::default()
        },
        other => PreviewArgs {
            kind: other.into(),
            ..PreviewArgs::default()
        },
    }
}

/// EDIT10-01: `ast_edit_preview`'s output is byte-for-byte the documented shape.
///
/// Every structural element of `TOOLS.md` §`ast_edit_preview` is here: the header with the plan
/// id, the expiry, the file and edit counts and the byte totals, one line per file with the
/// singular/plural of "edit", the diff, and the two `Next:` lines.
///
/// Two places where this document and `TOOLS.md` §`ast_edit_preview` **disagree**, and what the
/// test pins:
///
/// - The worked example shows the diff **unfenced**; §Output sanitising says "Diffs and
///   `ast_search` match lines are always inside fences (never next to tool prose)". The
///   normative rule wins, so the diff is fenced - a source line starting with `-` or `+` can
///   never be read as tool prose.
/// - The `@@` hunk header sits **outside** the fence. §Output sanitising requires "diff lines"
///   to be fenced, and the header is generated here from numbers, not read from the file - so it
///   is tool prose, and fencing it would only make the output harder to read.
/// - The `+A −B` totals: L3's `RiskSummary::bytes_removed` is `changed_bytes`, which counts
///   insertions as well as removals, so the handler computes both numbers from the edits. A
///   golden test that printed `−26` for two six-byte lines would have been a wrong number
///   faithfully rendered.
#[test]
fn edit10_01_preview_output_is_the_documented_shape_byte_for_byte() {
    let fx = Fx::new(Mode::ReadOnly);
    fx.write("src/a.ts", "log(1);\nlog(2);\n");
    let out = ast_edit_preview(
        &fx.edit(),
        &PreviewArgs {
            kind: "rewrite".into(),
            language: Some("typescript".into()),
            paths: Some(vec!["src/a.ts".into()]),
            pattern: Some("log($$$ARGS)".into()),
            replacement: Some("log2($$$ARGS)".into()),
            note: None,
            ..PreviewArgs::default()
        },
    )
    .unwrap();

    let id = out.split_whitespace().nth(1).unwrap();
    assert_eq!(id.len(), 28, "a full plan id is 28 characters: {id}");
    // `expires` is `created_at + plan_ttl_minutes` rendered in UTC: 1 000 000 s is 11d 13:46:40,
    // and the plan lives 15 minutes, so it expires at 14:01:40 UTC - a constant here, not a race.
    let expected = format!(
        "plan {id}  (expires 14:01 UTC)  — 1 files, 2 edits, +14 −12 bytes\n\
         \x20 src/a.ts   2 edits   syntax errors 0 → 0\n\
         \n\
         --- a/src/a.ts\n\
         +++ b/src/a.ts\n\
         @@ -1,2 +1,2 @@\n\
         ```\n\
         -log(1);\n\
         -log(2);\n\
         +log2(1);\n\
         +log2(2);\n\
         ```\n\
         Next: review the diff, then apply with ast_edit_apply plan_id={id}\n\
         \x20     (write mode) or with `opencrayast edit apply {id}` (CLI).\n"
    );
    assert_eq!(
        out, expected,
        "preview output drifted from the documented shape"
    );
}

/// EDIT10-02: the mode gate. The three write tools are not exposed in read-only mode (MCP-02),
/// and a handler reached anyway refuses with `write_disabled`.
#[test]
fn edit10_02_write_tools_are_not_exposed_in_read_only_and_refuse_when_called() {
    // Layer 1, the catalogue: these three are not listed in read mode. Asserted through
    // `registry`, which is the single source for both the names and the listing - the tools crate
    // keeps no second copy of either.
    for tool in WRITE_TOOL_NAMES {
        assert!(
            !tools_for_mode(Mode::ReadOnly).any(|e| e.name == *tool),
            "{tool} must not be listed in read-only mode"
        );
    }
    // The read tools are listed in both modes - `ast_edit_preview` writes only the plan store.
    for tool in [
        "ast_info",
        "ast_outline",
        "ast_get",
        "ast_search",
        "ast_explain_pattern",
        "ast_plan_list",
        "ast_plan_show",
        "ast_edit_preview",
    ] {
        assert!(
            tools_for_mode(Mode::ReadOnly).any(|e| e.name == tool),
            "{tool} must be listed in read-only mode"
        );
        assert!(
            tools_for_mode(Mode::Write).any(|e| e.name == tool),
            "{tool} must be listed in write mode"
        );
    }

    // Layer 2, the handler: an in-process or CLI caller that reaches a write tool while writes
    // are off gets `write_disabled` and a next step. A CLI caller knows the tool exists, so
    // answering "unknown tool" there would be nonsense - and the MCP layer never gets this far,
    // because layer 1 already refuses the name. Two layers, two contracts, neither a second
    // source of truth for the other.
    let fx = Fx::new(Mode::ReadOnly);
    fx.write("src/a.ts", "log(1);\n");
    let id = fx.preview_log_rewrite();
    for result in [
        ast_edit_apply(
            &fx.edit(),
            &ApplyArgs {
                plan_id: id.clone(),
            },
        )
        .map(|_| ()),
        ast_undo(
            &fx.edit(),
            &UndoArgs {
                plan_id: id.clone(),
            },
        )
        .map(|_| ()),
        ast_recover(&fx.edit()).map(|_| ()),
    ] {
        let err = result.unwrap_err();
        assert_eq!(
            err.code,
            opencrayast_core::ErrorCode::WriteDisabled,
            "a write tool in read-only mode must be write_disabled"
        );
        assert!(
            !err.message.contains(&fx.root.display().to_string()),
            "the message must not leak an absolute path: {:?}",
            err.message
        );
    }
}

/// EDIT10-03 (E-15): apply takes the **full** id only. A prefix is refused, and so is anything
/// that merely looks like one - the write path never resolves an abbreviation.
#[test]
fn edit10_03_apply_refuses_a_prefix_id() {
    let fx = Fx::new(Mode::Write);
    let id = fx.preview_log_rewrite();
    assert_eq!(id.len(), 28);

    let err = ast_edit_apply(
        &fx.edit(),
        &ApplyArgs {
            plan_id: id[..12].to_string(),
        },
    )
    .unwrap_err();
    assert_eq!(
        err.code,
        opencrayast_core::ErrorCode::InvalidArgs,
        "a prefix must not be enough to write"
    );
    assert!(
        err.message.contains("full plan id"),
        "the message must say why: {:?}",
        err.message
    );
    // **Which layer refused**, pinned by the wording, because the code cannot tell the two gates
    // apart: L3 refuses a prefix as well - `PlanStore::get_for_write` answers "write paths
    // require a full plan id" - so a test that checked only the code would stay green with this
    // module's gate deleted, and the tools layer would have stopped being the thing that says
    // "abbreviations are for reading".
    assert!(
        err.message.contains("prefix"),
        "the tools layer must be the one that refused, and it must say it was a prefix: {:?}",
        err.message
    );
    assert_eq!(
        fx.plans.get_for_write(&id[..12]).unwrap_err().message,
        "write paths require a full plan id",
        "L3 refuses prefixes independently - recorded here so the two gates stay distinguishable"
    );

    // The read-only tool accepts the same prefix, which is the asymmetry E-15 describes.
    let read_back = ast_plan_show(
        &fx.edit(),
        &PlanShowArgs {
            plan_id: id[..12].to_string(),
            ..PlanShowArgs::default()
        },
    )
    .unwrap();
    assert!(
        read_back.contains(&id),
        "ast_plan_show accepts a prefix and prints the full id"
    );

    // Too short to be a prefix at all.
    let err = ast_plan_show(
        &fx.edit(),
        &PlanShowArgs {
            plan_id: "p-abc".into(),
            ..PlanShowArgs::default()
        },
    )
    .unwrap_err();
    assert_eq!(err.code, opencrayast_core::ErrorCode::InvalidArgs);
}

/// EDIT10-04: apply output is the documented shape, byte for byte, including the two `Next:`
/// lines and the undo hint.
#[test]
fn edit10_04_apply_output_is_the_documented_shape_byte_for_byte() {
    let fx = Fx::new(Mode::Write);
    fx.write("src/a.ts", "log(1);\n");
    let out = ast_edit_preview(
        &fx.edit(),
        &PreviewArgs {
            kind: "rewrite".into(),
            language: Some("typescript".into()),
            paths: Some(vec!["src/a.ts".into()]),
            pattern: Some("log($$$ARGS)".into()),
            replacement: Some("log2($$$ARGS)".into()),
            ..PreviewArgs::default()
        },
    )
    .unwrap();
    let id = out.split_whitespace().nth(1).unwrap().to_string();

    let applied = ast_edit_apply(
        &fx.edit(),
        &ApplyArgs {
            plan_id: id.clone(),
        },
    )
    .unwrap();
    let expected = format!(
        "Applied {id} — 1 file changed.\n\
         \x20 src/a.ts   syntax errors 0 → 0\n\
         Undo with ast_undo plan_id={id} (kept 7 days).\n\
         Next: verify the semantics — for example Run your project's type checker or \
         language-server diagnostics on the changed files.\n\
         \x20     (lsp_diagnostics) on the changed files.\n"
    );
    assert_eq!(
        applied, expected,
        "apply output drifted from the documented shape"
    );
    assert_eq!(
        fs::read_to_string(fx.root.join("src/a.ts")).unwrap(),
        "log2(1);\n",
        "and the workspace really changed"
    );
}

/// EDIT10-05: undo restores the bytes exactly and prints what it restored.
#[test]
fn edit10_05_undo_restores_the_bytes_and_says_so() {
    let fx = Fx::new(Mode::Write);
    let original = "log(1);\n";
    fx.write("src/a.ts", original);
    let id = ast_edit_preview(
        &fx.edit(),
        &PreviewArgs {
            kind: "rewrite".into(),
            language: Some("typescript".into()),
            paths: Some(vec!["src/a.ts".into()]),
            pattern: Some("log($$$ARGS)".into()),
            replacement: Some("log2($$$ARGS)".into()),
            ..PreviewArgs::default()
        },
    )
    .unwrap()
    .split_whitespace()
    .nth(1)
    .unwrap()
    .to_string();
    ast_edit_apply(
        &fx.edit(),
        &ApplyArgs {
            plan_id: id.clone(),
        },
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(fx.root.join("src/a.ts")).unwrap(),
        "log2(1);\n"
    );

    let undone = ast_undo(
        &fx.edit(),
        &UndoArgs {
            plan_id: id.clone(),
        },
    )
    .unwrap();
    assert_eq!(
        undone,
        format!(
            "Undone {id} — 1 file restored.\n  \
             src/a.ts\n\
             The journal for {id} is kept 7 days; after that undo is no longer possible.\n"
        )
    );
    assert_eq!(
        fs::read_to_string(fx.root.join("src/a.ts")).unwrap(),
        original,
        "undo restores the bytes exactly"
    );
}

/// EDIT10-06: a file edited after apply refuses the undo with `diverged`, names the file, and
/// prints **nothing** from the file's content.
#[test]
fn edit10_06_diverged_names_the_file_and_leaks_no_content() {
    let fx = Fx::new(Mode::Write);
    fx.write("src/a.ts", "log(1);\n");
    let id = ast_edit_preview(
        &fx.edit(),
        &PreviewArgs {
            kind: "rewrite".into(),
            language: Some("typescript".into()),
            paths: Some(vec!["src/a.ts".into()]),
            pattern: Some("log($$$ARGS)".into()),
            replacement: Some("log2($$$ARGS)".into()),
            ..PreviewArgs::default()
        },
    )
    .unwrap()
    .split_whitespace()
    .nth(1)
    .unwrap()
    .to_string();
    ast_edit_apply(
        &fx.edit(),
        &ApplyArgs {
            plan_id: id.clone(),
        },
    )
    .unwrap();

    // A person edits the file afterwards, with a recognisable string in it.
    fx.write("src/a.ts", "SECRET_TOKEN_ABC\n");
    let err = ast_undo(
        &fx.edit(),
        &UndoArgs {
            plan_id: id.clone(),
        },
    )
    .unwrap_err();
    assert_eq!(err.code, opencrayast_core::ErrorCode::Diverged);
    assert!(
        err.message.contains("src/a.ts"),
        "the diverged files must be named: {:?}",
        err.message
    );
    assert!(
        !err.message.contains("SECRET_TOKEN_ABC"),
        "the refusal must not quote the file's content: {:?}",
        err.message
    );
    assert_eq!(
        fs::read_to_string(fx.root.join("src/a.ts")).unwrap(),
        "SECRET_TOKEN_ABC\n",
        "and nothing was reverted"
    );
}

/// EDIT10-07: the diff is **bounded**, and truncation is announced with a pointer to
/// `ast_plan_show` rather than silently dropped.
///
/// The cap is exercised through the shipping path by lowering `max_output_bytes` on the context,
/// so the string compared here is the one that would be returned to a client at that limit.
#[test]
fn edit10_07_the_diff_is_bounded_and_truncation_says_where_the_rest_is() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ws");
    fs::create_dir_all(root.join("src")).unwrap();
    let state = dir.path().join("state");
    // A body big enough that the diff cannot fit in the output cap.
    let mut source = String::new();
    for i in 0..400 {
        source.push_str(&format!("log({i});\n"));
    }
    fs::write(root.join("src/a.ts"), &source).unwrap();

    let limits = Limits {
        max_output_bytes: 2048,
        ..Limits::default()
    };
    let clock: Arc<dyn Clock> = Arc::new(FakeClock(AtomicU64::new(1_000_000)));
    let plans = PlanStore::open(&state, WS, limits.clone(), clock.clone()).unwrap();
    let journals = JournalStore::open(&state, WS, limits.clone(), clock).unwrap();
    let tools = ToolContext {
        boundary: Boundary::new(BoundaryConfig::new(root.clone(), limits.clone())).unwrap(),
        limits,
        mode: Mode::ReadOnly,
        write: None,
        version: "0.20261002.1".into(),
        workspace_id: WS.into(),
        respect_gitignore: true,
        extra_ignore: Vec::new(),
        config_source: Default::default(),
    };
    let ctx = EditTools {
        tools: &tools,
        plans: &plans,
        journals: &journals,
        state_dir: &state,
        lock_timeout: std::time::Duration::from_millis(200),
    };

    let out = ast_edit_preview(
        &ctx,
        &PreviewArgs {
            kind: "rewrite".into(),
            language: Some("typescript".into()),
            paths: Some(vec!["src/a.ts".into()]),
            pattern: Some("log($$$ARGS)".into()),
            replacement: Some("log9($$$ARGS)".into()),
            ..PreviewArgs::default()
        },
    )
    .unwrap();

    assert!(
        out.len() <= 2048,
        "the output must respect max_output_bytes, got {}",
        out.len()
    );
    assert!(
        out.contains("[truncated"),
        "a truncated preview must say so: {:?}",
        out
    );
    assert!(
        out.contains("ast_plan_show"),
        "and must point at ast_plan_show: {:?}",
        out
    );
    // What it did show is real diff, not a stub - and it is cut between lines, so the `@@`
    // header is never left pointing at a body that was dropped whole.
    assert!(out.contains("--- a/src/a.ts"), "{}", out);
    assert!(out.contains("@@ -1,400 +1,400 @@"), "{}", out);
    assert!(
        out.contains("-log(0);"),
        "the first lines are shown: {}",
        out
    );
    // F6, the other direction: when the **header** does not fit, no fence may follow it either.
    // A plan whose per-file summary alone fills the budget is the case - the diff then starts
    // with no room at all, and an unguarded `out.line("```")` would leave a stray code block
    // with no `--- a/` above it.
    let wide = Fx::with_limits(Mode::ReadOnly, |limits| limits.max_output_bytes = 260);
    for i in 0..6 {
        wide.write(&format!("src/f{i}.ts"), &format!("log({i});\n"));
    }
    let wide_id = ast_edit_preview(
        &wide.edit(),
        &PreviewArgs {
            kind: "rewrite".into(),
            language: Some("typescript".into()),
            paths: Some(vec!["src/".into()]),
            pattern: Some("log($$$ARGS)".into()),
            replacement: Some("log2($$$ARGS)".into()),
            ..PreviewArgs::default()
        },
    )
    .unwrap()
    .split_whitespace()
    .nth(1)
    .unwrap()
    .to_string();
    let narrow = ast_plan_show(
        &wide.edit(),
        &PlanShowArgs {
            plan_id: wide_id,
            limit: Some(2),
            ..PlanShowArgs::default()
        },
    )
    .unwrap();
    assert!(
        !narrow.contains("```"),
        "a header that does not fit must not be followed by an orphan fence: {narrow}"
    );
    assert!(
        !narrow.contains("--- a/"),
        "and no file header either: {narrow}"
    );
    assert!(
        narrow.contains("[truncated") || narrow.contains("Next: review the diff"),
        "the output ends with a tail line rather than running out mid-sentence: {narrow}"
    );

    // The knife edge is where the `@@` header fits and the fence's opening line does not, and a
    // cap sweep is the only way to land on it: a single fixed cap proves nothing about the byte
    // either side of it. For every cap, **no fenced block may be empty** - an opener immediately
    // followed by a closer, a truncation notice, or the end of the output is an orphan.
    for cap in (300..=760).step_by(4) {
        let swept = Fx::with_limits(Mode::ReadOnly, |limits| {
            limits.max_output_bytes = cap;
        });
        for i in 0..6 {
            swept.write(&format!("src/f{i}.ts"), &format!("log({i});\n"));
        }
        let swept_id = ast_edit_preview(
            &swept.edit(),
            &PreviewArgs {
                kind: "rewrite".into(),
                language: Some("typescript".into()),
                paths: Some(vec!["src/".into()]),
                pattern: Some("log($$$ARGS)".into()),
                replacement: Some("log2($$$ARGS)".into()),
                ..PreviewArgs::default()
            },
        )
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_string();
        let shown = ast_plan_show(
            &swept.edit(),
            &PlanShowArgs {
                plan_id: swept_id,
                limit: Some(3),
                ..PlanShowArgs::default()
            },
        )
        .unwrap();
        // Fence parity: a fence line seen while outside a block opens one, and it must have a
        // body line after it. A closer may legitimately be followed by the truncation notice -
        // that is a block that was cut, not an orphan.
        let lines: Vec<&str> = shown.lines().collect();
        let mut in_block = false;
        for (i, line) in lines.iter().enumerate() {
            if !line.trim_start().starts_with("```") {
                continue;
            }
            if in_block {
                in_block = false;
                continue;
            }
            in_block = true;
            let body = lines.get(i + 1).map(|l| l.trim_start());
            assert!(
                body.is_some_and(|b| {
                    !(b.starts_with("```") || b.starts_with('[') || b.is_empty())
                }),
                "cap {cap}: a fenced block opened at line {i} with no body:\n{shown}"
            );
        }
        assert!(
            !in_block,
            "cap {cap}: the output ends inside an unclosed fence, which would render everything \
             after it as code:\n{shown}"
        );
    }

    assert!(
        out.contains("```\nNext: review the diff"),
        "the fence must be closed even when the diff was cut, or the rest of the output renders \
         as code: {out}"
    );
}

/// EDIT10-08: `ast_plan_list` shows what `TOOLS.md` says it shows - id, note, files, edits,
/// state, expiry - and its `limit` is honoured with a notice when it cuts.
#[test]
fn edit10_08_plan_list_shows_state_and_expiry_and_bounds_its_limit() {
    let fx = Fx::new(Mode::ReadOnly);
    fx.write("src/a.ts", "log(1);\n");
    let first = fx.preview_log_rewrite();
    fx.write("src/b.ts", "log(3);\n");
    let second = fx.preview_log_rewrite();

    let all = ast_plan_list(&fx.edit(), &PlanListArgs::default()).unwrap();
    assert!(
        all.contains(&first) && all.contains(&second),
        "both plans listed: {all}"
    );
    // The note column shows `-` for these two plans (they have none); the marked form is
    // asserted in `edit10_13`, where a plan actually carries a note.
    for field in ["ready", "files", "edits", "expires"] {
        assert!(all.contains(field), "the row must carry {field}: {all}");
    }

    // A plan that has been applied reports `applied`, from the journal rather than the store.
    let write = Fx::new(Mode::Write);
    write.write("src/a.ts", "log(1);\n");
    let id = write.preview_log_rewrite();
    ast_edit_apply(
        &write.edit(),
        &ApplyArgs {
            plan_id: id.clone(),
        },
    )
    .unwrap();
    let listed = ast_plan_list(&write.edit(), &PlanListArgs::default()).unwrap();
    assert!(
        listed.contains("applied"),
        "an applied plan says applied: {listed}"
    );
    assert!(
        !listed.contains("note: -"),
        "F4: the note column is the caller's words or a dash, never a bare label: {listed}"
    );

    // The limit is honoured, and cutting is announced.
    let fx2 = Fx::new(Mode::ReadOnly);
    fx2.write("src/a.ts", "log(1);\n");
    fx2.preview_log_rewrite();
    // A different note is a different plan (it is hashed); previewing the same request twice is
    // the same plan, which is the determinism invariant and not a second row.
    let second = ast_edit_preview(
        &fx2.edit(),
        &PreviewArgs {
            note: Some("a second request".into()),
            ..preview_args("rewrite")
        },
    )
    .unwrap();
    assert!(second.contains("plan p-"), "{second}");
    let one = ast_plan_list(&fx2.edit(), &PlanListArgs { limit: Some(1) }).unwrap();
    assert!(
        one.contains("[truncated: showing 1 of"),
        "a cut list must say so: {one}"
    );
    for bad in [Some(0), Some(201)] {
        let err = ast_plan_list(&fx2.edit(), &PlanListArgs { limit: bad }).unwrap_err();
        assert_eq!(err.code, opencrayast_core::ErrorCode::InvalidArgs);
    }
}

/// EDIT10-09: argument validation. Every refusal is `invalid_args` naming the argument and its
/// legal form, and a `rewrite` argument passed to `kind=symbol` (or the reverse) is refused
/// rather than ignored.
#[test]
fn edit10_09_arguments_are_validated_and_named() {
    let fx = Fx::new(Mode::ReadOnly);
    fx.write("src/a.ts", "log(1);\n");

    // A missing required argument.
    let err = ast_edit_preview(&fx.edit(), &preview_args("rewrite_missing_pattern")).unwrap_err();
    assert_eq!(err.code, opencrayast_core::ErrorCode::InvalidArgs);
    assert!(err.message.contains("pattern"), "{}", err.message);

    // An unknown kind.
    let err = ast_edit_preview(&fx.edit(), &preview_args("delete_everything")).unwrap_err();
    assert_eq!(err.code, opencrayast_core::ErrorCode::InvalidArgs);
    assert!(
        err.message.contains("not an edit kind"),
        "the message states what is true: {:?}",
        err.message
    );
    assert!(
        err.next.contains("rewrite") && err.next.contains("symbol"),
        "and `next` says what to do: {:?}",
        err.next
    );

    // A rewrite argument on a symbol request.
    let err = ast_edit_preview(
        &fx.edit(),
        &PreviewArgs {
            kind: "symbol".into(),
            operation: Some("delete".into()),
            path: Some("src/a.ts".into()),
            symbol: Some("log".into()),
            pattern: Some("log($$$ARGS)".into()),
            ..PreviewArgs::default()
        },
    )
    .unwrap_err();
    assert_eq!(err.code, opencrayast_core::ErrorCode::InvalidArgs);
    assert!(
        err.message.contains("pattern"),
        "the message names the offending argument: {:?}",
        err.message
    );
    assert!(
        err.next.contains("rewrite"),
        "and `next` says which kind takes it: {:?}",
        err.next
    );

    // An unknown operation, naming the five legal ones.
    let err = ast_edit_preview(
        &fx.edit(),
        &PreviewArgs {
            kind: "symbol".into(),
            operation: Some("rename".into()),
            path: Some("src/a.ts".into()),
            symbol: Some("log".into()),
            ..PreviewArgs::default()
        },
    )
    .unwrap_err();
    assert_eq!(err.code, opencrayast_core::ErrorCode::InvalidArgs);
    assert!(
        err.message.contains("not one of the five operations"),
        "the message states what is true: {:?}",
        err.message
    );
    assert!(
        err.next.contains("replace_body") && err.next.contains("insert_after"),
        "and `next` lists the five legal operations: {:?}",
        err.next
    );
}

/// EDIT10-10: the 0-match preview is the shape `TOOLS.md` documents - a success, not an error.
#[test]
fn edit10_10_a_preview_that_matches_nothing_is_a_success_with_the_documented_shape() {
    let fx = Fx::new(Mode::ReadOnly);
    fx.write("src/a.ts", "log(1);\n");
    let out = ast_edit_preview(
        &fx.edit(),
        &PreviewArgs {
            kind: "rewrite".into(),
            language: Some("typescript".into()),
            paths: Some(vec!["src/a.ts".into()]),
            pattern: Some("zzz_no_such_call($$$ARGS)".into()),
            replacement: Some("x".into()),
            ..PreviewArgs::default()
        },
    )
    .unwrap();
    assert_eq!(
        out,
        "0 matches — nothing to change in src/a.ts\n\
         Next: widen the pattern, or preview a directory instead of one file.\n",
        "the 0-match shape is printed exactly as TOOLS.md writes it, **including what was \
         searched**"
    );
}

/// `ast_info`'s write banner must name the tools that are actually write tools - the list in
/// `info.rs` and the one this module gates on are the same three names.
#[test]
fn edit10_11_the_write_banner_names_exactly_the_write_tools() {
    let read_only = Fx::new(Mode::ReadOnly);
    let info = opencrayast_tools::ast_info(&read_only.tools);
    assert!(info.contains("write: disabled"), "{info}");
    let write = Fx::new(Mode::Write);
    let info = opencrayast_tools::ast_info(&write.tools);
    assert!(info.contains("write: enabled"), "{info}");
    for tool in WRITE_TOOL_NAMES {
        assert!(
            info.contains(tool),
            "{tool} must be named in the banner: {info}"
        );
    }
    // And nothing else is presented as a write tool.
    let banner = info.lines().next_back().unwrap();
    for tool in ["ast_edit_preview", "ast_plan_show", "ast_plan_list"] {
        assert!(
            !banner.contains(tool),
            "{tool} is a read tool and must not be in the write banner: {banner}"
        );
    }
}

/// The read-only banner must not recommend a step that does not, by itself, reach write mode.
///
/// `--allow-write` is **necessary and not sufficient**: the server also requires
/// `policy.allow_write = true` in its configuration file, and with the flag alone it stays
/// read-only. The old banner said "start the server with --allow-write to enable …", so a model
/// that had already passed the flag was told to pass the flag again — an instruction that cannot
/// work.
///
/// Mutation self-proof: restore the old read-only line in `info.rs` — red on the missing
/// `policy.allow_write`.
#[test]
fn info_names_both_conditions_for_write_mode_in_read_mode() {
    let read_only = Fx::new(Mode::ReadOnly);
    let info = opencrayast_tools::ast_info(&read_only.tools);
    let banner = info.lines().next_back().unwrap();
    assert!(
        banner.contains("--allow-write"),
        "the command-line half of the condition must still be named: {banner}"
    );
    assert!(
        banner.contains("policy.allow_write = true"),
        "the configuration half must be named too, or the advice is a loop: {banner}"
    );
}

/// `ast_recover` has no arguments, and its args type exists so the MCP layer has one type per
/// tool. The call is made with no argument at all, which is the shape the contract describes.
#[test]
fn edit10_14_recover_takes_no_arguments() {
    let fx = Fx::new(Mode::Write);
    fx.write("src/a.ts", "log(1);\n");
    // `RecoverArgs` is a unit struct: there is nothing to pass, which is what "Arguments: none"
    // means. The handler takes no argument parameter at all.
    let args: opencrayast_tools::RecoverArgs = opencrayast_tools::RecoverArgs;
    let _ = args;
    assert_eq!(ast_recover(&fx.edit()).unwrap(), "Nothing to recover.\n");
}

/// `ast_recover` with nothing to recover says so, in one line.
#[test]
fn edit10_12_recover_with_nothing_to_recover_says_so() {
    let fx = Fx::new(Mode::Write);
    fx.write("src/a.ts", "log(1);\n");
    assert_eq!(ast_recover(&fx.edit()).unwrap(), "Nothing to recover.\n");

    // After an apply there is still nothing to recover: apply recovered as it went.
    let id = fx.preview_log_rewrite();
    ast_edit_apply(&fx.edit(), &ApplyArgs { plan_id: id }).unwrap();
    assert_eq!(
        ast_recover(&fx.edit()).unwrap(),
        "Nothing to recover.\n",
        "a clean apply leaves no half-applied journal"
    );
}

/// A note with a hostile character is escaped in the output, and the escape is counted.
///
/// The string compared is the handler's return value, so this is the sanitising a client gets -
/// not a helper called directly.
#[test]
fn edit10_13_a_hostile_note_is_escaped_and_the_count_reported() {
    let fx = Fx::new(Mode::ReadOnly);
    fx.write("src/a.ts", "log(1);\n");
    let out = ast_edit_preview(
        &fx.edit(),
        &PreviewArgs {
            kind: "rewrite".into(),
            language: Some("typescript".into()),
            paths: Some(vec!["src/a.ts".into()]),
            pattern: Some("log($$$ARGS)".into()),
            replacement: Some("log2($$$ARGS)".into()),
            note: Some("issue 42\u{1b}[31m".into()),
            ..PreviewArgs::default()
        },
    )
    .unwrap();
    // The note is not shown by preview's own layout, but the plan list shows it - through the
    // same escape helper the preview output uses.
    let listed = ast_plan_list(&fx.edit(), &PlanListArgs::default()).unwrap();
    assert!(
        !listed.contains('\u{1b}'),
        "no raw ESC may reach the output: {listed:?}"
    );
    assert!(
        listed.contains("\\u{1b}"),
        "the ESC is shown as an escape: {listed:?}"
    );
    assert!(
        listed.contains("[escaped: 1 control"),
        "and the count is reported: {listed:?}"
    );
    // F4: preview shows the note too, marked as the caller's words - a reviewer must never read
    // a note as something the tool concluded.
    assert!(
        out.contains("note (written by the caller): issue 42\\u{1b}[31m"),
        "preview must show the note, marked as caller-written: {out:?}"
    );
    assert!(!out.contains('\u{1b}'), "and no raw ESC: {out:?}");
}

/// F7, first half: `ast_plan_show` returns **the diff**, not just a summary.
///
/// This is the test whose absence let F1 through. A truncated preview points at this tool for
/// "the rest", so an answer without a diff leaves that pointer aimed at nothing - and the
/// reviewer's own probe for it was three `contains` checks: a hunk header, a `-` line, and a
/// piece of the file's content.
#[test]
fn edit10_15_plan_show_returns_the_diff_a_truncated_preview_points_at() {
    let fx = Fx::new(Mode::ReadOnly);
    fx.write("src/a.ts", "log(1);\nlog(2);\n");
    fx.write("src/b.ts", "log(3);\n");
    let id = ast_edit_preview(
        &fx.edit(),
        &PreviewArgs {
            kind: "rewrite".into(),
            language: Some("typescript".into()),
            paths: Some(vec!["src/".into()]),
            pattern: Some("log($$$ARGS)".into()),
            replacement: Some("log2($$$ARGS)".into()),
            ..PreviewArgs::default()
        },
    )
    .unwrap()
    .split_whitespace()
    .nth(1)
    .unwrap()
    .to_string();

    let shown = ast_plan_show(
        &fx.edit(),
        &PlanShowArgs {
            plan_id: id.clone(),
            ..PlanShowArgs::default()
        },
    )
    .unwrap();

    assert!(shown.contains("@@ "), "a hunk header: {shown}");
    assert!(shown.contains("\n-log(1);"), "a removed line: {shown}");
    assert!(shown.contains("\n+log2(1);"), "an added line: {shown}");
    assert!(shown.contains("--- a/src/a.ts"), "the file header: {shown}");
    assert!(shown.contains("expires "), "and the expiry: {shown}");
    assert!(shown.contains("UTC"), "with the zone it is in: {shown}");
    // Both files, because the plan covers both.
    assert!(shown.contains("src/b.ts"), "{shown}");
}

/// F7, second half: `file`, `offset` and `limit` really do something.
///
/// Their absence is F2: all three were accepted, ignored, and produced output identical to a
/// plain call. Each one is now either obeyed or refused - never silently dropped.
#[test]
fn edit10_16_plan_show_file_offset_and_limit_are_obeyed_or_refused() {
    let fx = Fx::new(Mode::ReadOnly);
    fx.write("src/a.ts", "log(1);\nlog(2);\nlog(4);\nlog(5);\n");
    fx.write("src/b.ts", "log(3);\n");
    let id = ast_edit_preview(
        &fx.edit(),
        &PreviewArgs {
            kind: "rewrite".into(),
            language: Some("typescript".into()),
            paths: Some(vec!["src/".into()]),
            pattern: Some("log($$$ARGS)".into()),
            replacement: Some("log2($$$ARGS)".into()),
            ..PreviewArgs::default()
        },
    )
    .unwrap()
    .split_whitespace()
    .nth(1)
    .unwrap()
    .to_string();
    let show = |file: Option<&str>, offset: Option<usize>, limit: Option<usize>| {
        ast_plan_show(
            &fx.edit(),
            &PlanShowArgs {
                plan_id: id.clone(),
                file: file.map(str::to_string),
                offset,
                limit,
            },
        )
    };

    let all = show(None, None, None).unwrap();
    let hunk_count = all.matches("@@ ").count();
    assert!(hunk_count >= 2, "the fixture needs several hunks: {all}");

    // `file` narrows to one file.
    let only_a = show(Some("src/a.ts"), None, None).unwrap();
    assert!(only_a.contains("src/a.ts"), "{only_a}");
    assert!(
        !only_a.contains("--- a/src/b.ts"),
        "b.ts was filtered out: {only_a}"
    );

    // A file that is not in the plan is refused rather than ignored.
    let err = show(Some("no/such/file.rs"), None, None).unwrap_err();
    assert_eq!(err.code, opencrayast_core::ErrorCode::InvalidArgs);
    assert!(err.message.contains("no/such/file.rs"), "{:?}", err.message);

    // `limit` bounds the hunks, and says how many there were.
    let one = show(None, None, Some(1)).unwrap();
    assert_eq!(
        one.matches("@@ ").count(),
        1,
        "limit 1 shows one hunk: {one}"
    );
    assert!(one.contains("[showing 1 of"), "and says so: {one}");

    // `offset` skips.
    let second = show(None, Some(1), Some(1)).unwrap();
    assert_eq!(second.matches("@@ ").count(), 1, "{second}");
    assert_ne!(
        second, one,
        "offset 1 must show a different hunk than offset 0"
    );

    // `limit=0` is the same refusal `ast_plan_list` gives, not a silent empty answer.
    let err = show(None, None, Some(0)).unwrap_err();
    assert_eq!(err.code, opencrayast_core::ErrorCode::InvalidArgs);

    // An offset past the end is empty and says so, rather than pretending it showed hunks.
    let past = show(None, Some(99), None).unwrap();
    assert!(past.contains("[showing 0 of"), "{past}");
}

/// F3: the `state` column is the journal's own state, never rounded up to `applied`.
///
/// A `prepared` journal means originals are saved and **no target has been touched**, and a
/// `rolled_back` one means every target is back to its original; calling either of those
/// `applied` is how an agent decides the wrong thing about a plan. The row is read through the
/// public tool surface at three points in the plan's life, which is enough to catch the defect
/// this had: a mapping that answered `applied` for any journal at all.
#[test]
fn edit10_17_plan_list_state_tracks_the_journal() {
    let fx = Fx::new(Mode::Write);
    fx.write("src/a.ts", "log(1);\n");
    let id = fx.preview_log_rewrite();

    // No journal yet: nothing has ever been applied to this plan.
    assert!(
        ast_plan_list(&fx.edit(), &PlanListArgs::default())
            .unwrap()
            .contains("ready"),
        "a plan with no journal is ready"
    );

    ast_edit_apply(
        &fx.edit(),
        &ApplyArgs {
            plan_id: id.clone(),
        },
    )
    .unwrap();
    let after_apply = ast_plan_list(&fx.edit(), &PlanListArgs::default()).unwrap();
    assert!(after_apply.contains("applied"), "{after_apply}");

    // Undoing moves the journal to `undone`. A column that cannot report this is a column that
    // was reporting a constant.
    ast_undo(&fx.edit(), &UndoArgs { plan_id: id }).unwrap();
    let after_undo = ast_plan_list(&fx.edit(), &PlanListArgs::default()).unwrap();
    assert!(after_undo.contains("undone"), "{after_undo}");
    assert!(
        !after_undo.contains("applied"),
        "and it stops saying applied once undone: {after_undo}"
    );
}

/// The output cap is a **hard** limit, at every value a configuration can produce.
///
/// `Limits::validate` rejects only a zero `max_output_bytes`, so `max_output_bytes = 1` is a legal
/// configuration and the caller is entitled to a one-byte answer. The truncation notice and the
/// escape count used to be appended after the body had already spent the budget, which made every
/// truncated output about a notice longer than the cap it was given.
///
/// Mutation self-proof: append the notice with `push_str` outside the budget check in
/// `Bounded::finish` - restoring the old shape - and this goes red at the first cap whose notice
/// does not fit.
#[test]
fn edit10_18_output_never_exceeds_the_configured_cap() {
    // One fixture, one plan: the cap is then varied on the context so every measurement is of the
    // same plan. (The plan id is content-derived, so the repeated previews below all name it.)
    let mut fx = Fx::new(Mode::ReadOnly);
    for i in 0..4 {
        fx.write(&format!("src/f{i}.ts"), &format!("log({i});\nlog({i}b);\n"));
    }
    let args = || PreviewArgs {
        kind: "rewrite".into(),
        language: Some("typescript".into()),
        paths: Some(vec!["src/".into()]),
        pattern: Some("log($$$ARGS)".into()),
        replacement: Some("log2($$$ARGS)".into()),
        ..PreviewArgs::default()
    };
    let id = ast_edit_preview(&fx.edit(), &args())
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_string();

    for cap in 1..=200usize {
        fx.tools.limits.max_output_bytes = cap as u64;
        let preview = ast_edit_preview(&fx.edit(), &args()).unwrap();
        assert!(
            preview.len() <= cap,
            "cap {cap}: preview returned {} bytes\n{preview}",
            preview.len()
        );
        let shown = ast_plan_show(
            &fx.edit(),
            &PlanShowArgs {
                plan_id: id.clone(),
                ..PlanShowArgs::default()
            },
        )
        .unwrap();
        assert!(
            shown.len() <= cap,
            "cap {cap}: plan_show returned {} bytes\n{shown}",
            shown.len()
        );
        let listed = ast_plan_list(&fx.edit(), &PlanListArgs { limit: Some(20) }).unwrap();
        assert!(
            listed.len() <= cap,
            "cap {cap}: plan_list returned {} bytes\n{listed}",
            listed.len()
        );
    }
}
