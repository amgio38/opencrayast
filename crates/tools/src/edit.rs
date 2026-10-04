//! The six edit tools: `ast_edit_preview`, `ast_plan_show`, `ast_plan_list`,
//! `ast_edit_apply`, `ast_undo`, `ast_recover` (docs/TOOLS.md).
//!
//! # What a handler here is allowed to do
//!
//! Four things, in this order: turn deserialized arguments into validated ones, call L3, render
//! the result, and cap it. It does **not** implement edit logic and it does **not** touch the
//! filesystem - every path goes through [`opencrayast_core::boundary::Boundary`] and every
//! decision about what to write belongs to `opencrayast-edit` (`ARCHITECTURE.md` §Layers:
//! L4 renders, L3 plans, L2/L1 are pure). A handler that computed an edit itself would be a
//! second implementation of something the model already specifies, and the two would drift.
//!
//! # Why the context is [`EditTools`] and not [`ToolContext`]
//!
//! The write handlers need the plan and journal stores, and the apply lock's state directory.
//! `ToolContext` carries none of them, and adding required fields would force every existing
//! constructor - thirteen of them, across six test files this ticket does not own - to change.
//! [`EditTools`] borrows the read context and adds the three handles, so this module adds no
//! field to anyone else's struct.
//!
//! # The rendering contract
//!
//! Output is capped at `limits.max_output_bytes` and **says what was cut and where to get the
//! rest** ([`ast_plan_show`](TOOLS.md#ast_edit_preview)). A silent truncation is a lie about
//! what the caller is looking at. Every name, path and note goes through
//! [`escape_inline`](opencrayast_core::render::escape_inline), every diff line through a fence,
//! and the count of escaped characters is reported rather than hidden.
//!
//! # Read-only mode
//!
//! [`docs/TOOLS.md`](TOOLS.md#modes-and-annotations) and the catalogue entry MCP-02 say a write
//! tool called in read-only mode is an **unknown tool** - the name is not in the catalogue at
//! all. The failure table for this ticket says `[write_disabled]`, which is also a code in
//! `TOOLS.md`'s own error table ("Write tool used while writing is off", next step "Use the CLI
//! to apply, or enable write mode"). Both are implemented, each where it belongs:
//!
//! - [`crate::registry::tools_for_mode`] is the catalogue layer: in read-only mode the three
//!   write tools are **not**
//!   exposed, so a dispatch layer answers unknown-tool for them (MCP-02).
//! - the handlers themselves refuse with `write_disabled` if they are reached anyway - the
//!   path a CLI or an in-process caller takes, and the code `TOOLS.md` documents for it.

use crate::context::ToolContext;
use opencrayast_core::error::{ErrorCode, ToolError};
use opencrayast_core::hash::is_full_plan_id;
use opencrayast_core::render::{EscapeCounts, escape_inline, fenced_block};
use opencrayast_edit::apply::ApplyContext;
use opencrayast_edit::preview::{EditRequest, PreviewContext, SymbolOp, preview};
use opencrayast_edit::store::{PlanMeta, PlanStore, PlanSummary};
use opencrayast_edit::{
    ApplyResult, DiffLine, DiffLineKind, FileDiff, Hunk, JournalStore, Plan, PlanFile,
    PreviewOutcome, Recovered, SkippedFile, UndoResult,
};
use opencrayast_query::pattern::Rule;
use std::path::Path;
use std::time::Duration;

/// Everything the edit handlers need: the read context plus the stores L3 writes through.
pub struct EditTools<'a> {
    /// The shared read context: boundary, limits, mode, workspace id.
    pub tools: &'a ToolContext,
    /// Where plans are stored.
    pub plans: &'a PlanStore,
    /// Where journals are stored.
    pub journals: &'a JournalStore,
    /// The state directory, for the apply lock.
    pub state_dir: &'a Path,
    /// How long a write waits for the apply lock.
    pub lock_timeout: Duration,
}

/// `ast_edit_preview` arguments (docs/TOOLS.md §`ast_edit_preview`).
///
/// Every row of the documented argument table has a field, so the MCP layer can deserialize the
/// published schema into this without inventing anything. Fields that only apply to one `kind`
/// are `Option` and are refused when used with the other.
#[derive(Debug, Clone, Default)]
pub struct PreviewArgs {
    /// `rewrite` or `symbol`.
    pub kind: String,
    /// For `rewrite`: the language id the pattern is compiled for.
    pub language: Option<String>,
    /// For `rewrite`: files or directories.
    pub paths: Option<Vec<String>>,
    /// For `rewrite`: the pattern.
    pub pattern: Option<String>,
    /// For `rewrite`: the replacement template.
    pub replacement: Option<String>,
    /// For `rewrite`: the constraint rule.
    pub rule: Option<Rule>,
    /// For `symbol`: `replace`, `replace_body`, `delete`, `insert_before`, `insert_after`.
    pub operation: Option<String>,
    /// For `symbol`: the file.
    pub path: Option<String>,
    /// For `symbol`: the name or qualified name.
    pub symbol: Option<String>,
    /// For `symbol`: replacement text; required unless the operation is `delete`.
    pub text: Option<String>,
    /// Optional caller note, stored with the plan and shown to reviewers.
    pub note: Option<String>,
}

/// `ast_plan_show` arguments.
#[derive(Debug, Clone, Default)]
pub struct PlanShowArgs {
    /// A plan id or an unambiguous prefix of at least ten characters (EDIT-MODEL §Plan format).
    pub plan_id: String,
    /// Show only this file's diff.
    pub file: Option<String>,
    /// First hunk to show (0-based).
    pub offset: Option<usize>,
    /// Maximum hunks to show.
    pub limit: Option<usize>,
}

/// `ast_plan_list` arguments.
#[derive(Debug, Clone, Default)]
pub struct PlanListArgs {
    /// Maximum plans to list; default 20, hard maximum 200.
    pub limit: Option<usize>,
}

/// `ast_edit_apply` arguments.
#[derive(Debug, Clone, Default)]
pub struct ApplyArgs {
    /// The **full** plan id. A prefix is refused here and accepted only by the read-only plan
    /// tools (EDIT-MODEL E-15).
    pub plan_id: String,
}

/// `ast_undo` arguments.
#[derive(Debug, Clone, Default)]
pub struct UndoArgs {
    /// The applied plan's **full** id.
    pub plan_id: String,
}

/// `ast_recover` has no arguments; the struct exists so the MCP layer has one type per tool and
/// the signature stays uniform.
#[derive(Debug, Clone, Default)]
pub struct RecoverArgs;

// -------------------------------------------------------------------------------------------
// ast_edit_preview
// -------------------------------------------------------------------------------------------

/// Produce a plan. Never modifies the workspace (EDIT-MODEL E-12): the only write is into the
/// plan store, which is why this tool is available in read-only mode.
///
/// Output, byte for byte (`docs/TOOLS.md` §`ast_edit_preview`):
///
/// ```text
/// plan p-7k2m9xq4ab  (expires 09:45)  — 2 files, 3 edits, +41 −39 bytes
///   src/a.ts   2 edits   syntax errors 0 → 0
///   src/b.ts   1 edit    syntax errors 0 → 0
/// skipped: 1 file too large (docs/generated.ts)
///
/// --- a/src/a.ts
/// +++ b/src/a.ts
/// @@ -12,1 +12,1 @@
/// -  console.log("start", id)
/// +  logger.debug("start", id)
/// Next: review the diff, then apply with ast_edit_apply plan_id=p-7k2m9x4ab
///       (write mode) or with `opencrayast edit apply p-7k2m9xq4ab` (CLI).
/// ```
///
/// A request that matches nothing is a **successful** result with no plan id, no files and a
/// line saying so; the 0-match shape is documented in `TOOLS.md` and printed exactly as written
/// there.
pub fn ast_edit_preview(ctx: &EditTools<'_>, args: &PreviewArgs) -> Result<String, ToolError> {
    let request = build_request(args)?;
    let preview_ctx = PreviewContext {
        boundary: &ctx.tools.boundary,
        plans: ctx.plans,
        limits: &ctx.tools.limits,
        workspace_id: &ctx.tools.workspace_id,
    };
    let outcome = preview(&preview_ctx, &request)?;
    Ok(render_preview(ctx, &outcome, &searched_paths(&request)))
}

/// Validate the arguments into an [`EditRequest`], or say exactly what is missing.
///
/// Every refusal here is `invalid_args` naming the argument and its legal form, because that is
/// the one error a caller can fix without reading any other document.
fn build_request(args: &PreviewArgs) -> Result<EditRequest, ToolError> {
    // The summary is inside the hashed plan and `Plan::check` refuses an empty one, but
    // `TOOLS.md`'s argument table for this tool has no `summary` argument - so it is derived
    // here, deterministically, from the request itself. Deriving rather than defaulting to a
    // constant matters: the summary is hashed, so a constant would make two different rewrites
    // differ only in their edits and a reviewer's line would say nothing about what was asked.
    let summary_for = |what: String| -> String {
        let mut s = what;
        s.truncate(opencrayast_edit::plan::SUMMARY_MAX_BYTES);
        s
    };

    match args.kind.as_str() {
        "rewrite" => {
            let language = required(args.language.as_deref(), "language", "rewrite")?;
            let pattern = required(args.pattern.as_deref(), "pattern", "rewrite")?;
            let replacement = required(args.replacement.as_deref(), "replacement", "rewrite")?;
            let paths = args
                .paths
                .clone()
                .filter(|p| !p.is_empty())
                .ok_or_else(|| missing("paths", "a string[] of files or directories"))?;
            for wrong in [
                ("operation", args.operation.is_some()),
                ("path", args.path.is_some()),
                ("symbol", args.symbol.is_some()),
                ("text", args.text.is_some()),
            ] {
                if wrong.1 {
                    return Err(only_for("rewrite", wrong.0, "symbol"));
                }
            }
            Ok(EditRequest::Rewrite {
                language: language.to_string(),
                paths,
                pattern: pattern.to_string(),
                replacement: replacement.to_string(),
                rule: args.rule.clone(),
                allow_comment_loss: false,
                summary: summary_for(format!("rewrite {pattern}")),
                note: args.note.clone(),
            })
        }
        "symbol" => {
            let path = required(args.path.as_deref(), "path", "symbol")?;
            let symbol = required(args.symbol.as_deref(), "symbol", "symbol")?;
            let operation_raw = required(args.operation.as_deref(), "operation", "symbol")?;
            let operation = SymbolOp::from_id(operation_raw).ok_or_else(|| {
                ToolError::new(
                    ErrorCode::InvalidArgs,
                    format!("operation {operation_raw:?} is not one of the five operations."),
                    "Use replace, replace_body, delete, insert_before or insert_after.",
                )
            })?;
            for wrong in [
                ("language", args.language.is_some()),
                ("paths", args.paths.is_some()),
                ("pattern", args.pattern.is_some()),
                ("replacement", args.replacement.is_some()),
                ("rule", args.rule.is_some()),
            ] {
                if wrong.1 {
                    return Err(only_for("symbol", wrong.0, "rewrite"));
                }
            }
            Ok(EditRequest::Symbol {
                operation,
                path: path.to_string(),
                symbol: symbol.to_string(),
                text: args.text.clone(),
                summary: summary_for(format!("symbol {symbol}")),
                note: args.note.clone(),
            })
        }
        other => Err(ToolError::new(
            ErrorCode::InvalidArgs,
            format!("kind {other:?} is not an edit kind."),
            "Use kind=rewrite (pattern to replacement) or kind=symbol (one named symbol).",
        )),
    }
}

fn required<'a>(
    value: Option<&'a str>,
    name: &'static str,
    kind: &'static str,
) -> Result<&'a str, ToolError> {
    match value {
        Some(v) if !v.is_empty() => Ok(v),
        _ => Err(ToolError::new(
            ErrorCode::InvalidArgs,
            format!("kind={kind} needs a non-empty {name}."),
            format!("Pass {name} as a string, as documented for ast_edit_preview."),
        )),
    }
}

fn missing(name: &'static str, form: &str) -> ToolError {
    ToolError::new(
        ErrorCode::InvalidArgs,
        format!("{name} is missing."),
        format!("Pass {name} as {form}."),
    )
}

fn only_for(given: &str, argument: &str, other: &str) -> ToolError {
    ToolError::new(
        ErrorCode::InvalidArgs,
        format!("{argument} is not an argument of kind={given}."),
        format!("Drop {argument}, or use kind={other}."),
    )
}

fn render_preview(ctx: &EditTools<'_>, outcome: &PreviewOutcome, searched: &str) -> String {
    let mut out = Bounded::new(ctx.tools.limits.max_output_bytes as usize);
    let mut escapes = EscapeCounts::default();

    match (&outcome.plan_id, outcome.stored) {
        (Some(id), _) => {
            let expires = outcome
                .expires_at
                .map(|at| format!("{} UTC", hhmm_utc(at)))
                .unwrap_or_else(|| "--:-- UTC".to_string());
            // `+A −B` comes straight from L3. EDIT-12 split the old `bytes_removed` (which was
            // added *and* removed) into `bytes_added` / `bytes_removed`, so the tool layer no
            // longer recomputes the two figures from the plan's edits — a second implementation
            // of the same rule is a second thing to get wrong.
            out.line(format!(
                "plan {id}  (expires {expires})  — {files} files, {edits} edits, +{added} −{removed} bytes",
                files = outcome.summary.files,
                edits = outcome.summary.edits,
                added = outcome.summary.bytes_added,
                removed = outcome.summary.bytes_removed,
            ));
            for file in &outcome.plan.files {
                // Escaped even though L3 already refuses control characters in a stored path
                // (`Plan::check` -> `path_ok`): this is the only place a path is ever rendered,
                // and the rule that keeps a terminal safe should not depend on a check three
                // layers down continuing to exist. Defence in depth, deliberately untested -
                // see the report: no fixture can produce a hostile path here, because L3 will
                // not store one.
                let (name, count) = escape_inline(&file.path);
                escapes.absorb(count);
                out.line(format!(
                    "  {name}   {n} edit{s}   syntax errors {pre} → {post}",
                    n = file.edits.len(),
                    s = if file.edits.len() == 1 { "" } else { "s" },
                    pre = file.pre_errors,
                    post = file.post_errors,
                ));
            }
            if !outcome.summary.skipped.is_empty() {
                out.line(skipped_line(&outcome.summary.skipped, &mut escapes));
            }
            if let Some(note) = &outcome.plan.request.note {
                let (escaped, count) = escape_inline(note);
                escapes.absorb(count);
                out.line(note_line(&escaped));
            }
            out.blank();
            // The two `Next:` lines are written first, so their length is known before the diff
            // is laid out, and the diff then gets whatever is left. Otherwise a large diff would
            // push them past the cap and a truncated preview would not say how to apply the plan -
            // the one line the caller cannot do without.
            let tail = format!(
                "Next: review the diff, then apply with ast_edit_apply plan_id={id}\n\
                 \x20     (write mode) or with `opencrayast edit apply {id}` (CLI).\n"
            );
            // The extra eight bytes are the closing fence a cut hunk forces past the cap.
            out.reserve_for(tail.len() + 8);
            for file in &outcome.diff.files {
                diff_block(&mut out, file, &mut escapes);
            }
            out.reserve_for(0);
            out.raw(&tail);
        }
        (None, _) => {
            // F5: the documented 0-match shape says **what was searched**, so the line names the
            // paths rather than leaving the caller to guess which tree was scanned.
            out.line(zero_match_line(searched, &mut escapes));
            out.line(
                "Next: widen the pattern, or preview a directory instead of one file.".to_string(),
            );
        }
    }
    out.finish(&escapes, || {
        format!(
            "[truncated at {} bytes; the rest is in ast_plan_show plan_id={}]",
            ctx.tools.limits.max_output_bytes,
            outcome.plan_id.clone().unwrap_or_else(|| "-".to_string())
        )
    })
}

/// The 0-match line: what was searched, sanitised like every other caller-controlled string.
///
/// The paths arrive from `Boundary::resolve_read`, which refuses control and invisible characters,
/// so today this cannot be reached with something to escape - and that is exactly why it is worth
/// stating in one place: the safety here would otherwise rest on another crate's gate rather than
/// on this module's own rule, and would silently disappear the day that gate is relaxed. The
/// `edit10_19` unit test drives this function directly with a hostile path, which the public
/// handler cannot.
fn zero_match_line(searched: &str, escapes: &mut EscapeCounts) -> String {
    let (searched, counts) = escape_inline(searched);
    escapes.absorb(counts);
    format!("0 matches — nothing to change in {searched}")
}

/// The note line, marked as the caller's own words: `TOOLS.md` requires a plan note to be
/// "shown sanitised and marked as written by the caller, not by the tool", because a reviewer
/// must never read it as something the tool concluded.
fn note_line(escaped: &str) -> String {
    format!("note (written by the caller): {escaped}")
}

/// What a preview scanned, for the 0-match line: the paths as the caller wrote them.
fn searched_paths(request: &EditRequest) -> String {
    match request {
        EditRequest::Rewrite { paths, .. } => paths.join(", "),
        EditRequest::Symbol { path, .. } => path.clone(),
    }
}

fn skipped_line(skipped: &[SkippedFile], escapes: &mut EscapeCounts) -> String {
    let mut parts: Vec<String> = Vec::new();
    for file in skipped {
        let (name, count) = escape_inline(&file.path);
        escapes.absorb(count);
        parts.push(format!("{} ({name})", file.reason.as_str()));
    }
    format!(
        "skipped: {} {}",
        parts.len(),
        if parts.len() == 1 { "file" } else { "files" }
    ) + &format!(" {}", parts.join(", "))
}

/// One file's unified diff, as data with its fences - never next to prose
/// (`TOOLS.md` §Output sanitising).
fn diff_block(out: &mut Bounded<'_>, file: &FileDiff, escapes: &mut EscapeCounts) {
    let hunks: Vec<&Hunk> = file.hunks.iter().collect();
    diff_block_from_hunks(out, &file.path, &hunks, escapes);
}

/// One file's diff, rendered the same way everywhere: `--- a/`, `+++ b/`, `@@`, fenced lines.
///
/// Shared by preview and `ast_plan_show`, so the page a truncated preview points at is produced
/// by the same code rather than a second renderer that could drift.
fn diff_block_from_hunks(
    out: &mut Bounded<'_>,
    path: &str,
    hunks: &[&Hunk],
    escapes: &mut EscapeCounts,
) {
    if hunks.is_empty() {
        return;
    }
    let (name, count) = escape_inline(path);
    escapes.absorb(count);
    // F6: if a header line does not fit, no fence may follow it. An orphan fence with no
    // `--- a/` above it renders as a stray code block in any client that honours fences.
    if !out.line(format!("--- a/{name}")) || !out.line(format!("+++ b/{name}")) {
        return;
    }
    for hunk in hunks {
        // The smallest block worth writing is an opener, one body line and a closer. If even that
        // does not fit, nothing of this hunk is written: writing the opener and closing it
        // immediately leaves an empty fenced block, and writing the opener without closing it
        // makes everything after it render as code. Both are worse than stopping here and letting
        // the truncation notice explain that the rest is in `ast_plan_show`.
        let header = format!(
            "@@ -{},{} +{},{} @@",
            hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
        );
        // The header is part of what has to fit, or the room the block checks against is the
        // room the header is about to take away from it.
        let minimum = header.len()
            + 1
            + hunk
                .lines
                .first()
                .map(|l| format!("{}{}\n", l.kind.prefix(), l.text).len())
                .unwrap_or(0)
            + 8;
        if !out.fits(minimum) {
            return;
        }
        if !out.line(header) {
            return;
        }
        // One `fenced_block` call for the whole hunk - it is the only thing that knows how long
        // the fence has to be - and then line by line through the bounded writer, so a hunk that
        // does not fit is cut between lines instead of vanishing and leaving a `@@` header
        // pointing at nothing.
        let body: String = hunk
            .lines
            .iter()
            .map(|l| format!("{}{}\n", l.kind.prefix(), l.text))
            .collect();
        let (block, block_escapes) = fenced_block(body.trim_end_matches('\n'), "");
        escapes.absorb(block_escapes);
        let lines: Vec<&str> = block.lines().collect();
        let mut opened = false;
        let mut body = 0usize;
        for (i, line) in lines.iter().enumerate() {
            let is_closer = i + 1 == lines.len();
            if out.line((*line).to_string()) {
                if i == 0 {
                    opened = true;
                } else if is_closer {
                    // closed
                } else {
                    body += 1;
                }
                continue;
            }
            if is_closer && opened && body > 0 {
                // The body was cut short and the cap would not allow the closing fence. An
                // unclosed fence makes everything after it render as code, which is a far worse
                // failure than being a few bytes over `max_output_bytes`; the truncation notice
                // follows immediately, so the reader is not misled. Forcing a second fence after
                // one that already fit would leave an empty block, so this only runs when the
                // closer was the line that did not fit.
                out.force_line((*line).to_string());
            }
        }
    }
}

/// Add one batch of escape counts to a running total.
///
/// `EscapeCounts` exposes its three fields but no `Add`, and this module reports the total over
/// every string it rendered, so the sum lives here rather than being repeated at each call site.
trait Absorb {
    fn absorb(&mut self, other: EscapeCounts);
}

impl Absorb for EscapeCounts {
    fn absorb(&mut self, other: EscapeCounts) {
        self.control += other.control;
        self.bidi += other.bidi;
        self.invisible += other.invisible;
    }
}

fn hhmm_utc(epoch_seconds: u64) -> String {
    let day_seconds = 86_400;
    let within_day = epoch_seconds % day_seconds;
    format!("{:02}:{:02}", within_day / 3600, (within_day % 3600) / 60)
}

// -------------------------------------------------------------------------------------------
// ast_plan_show / ast_plan_list
// -------------------------------------------------------------------------------------------

/// Show a stored plan's summary and diff, paged.
///
/// A prefix of at least ten characters is accepted here and in `ast_plan_list`'s sibling
/// read-only tools; `ast_edit_apply` and `ast_undo` require the full id (E-15), which is why the
/// check lives in those handlers and not here.
pub fn ast_plan_show(ctx: &EditTools<'_>, args: &PlanShowArgs) -> Result<String, ToolError> {
    let id = args.plan_id.trim();
    if id.is_empty() {
        return Err(missing("plan_id", "a string"));
    }
    if id.chars().count() < MIN_PREFIX_CHARS {
        return Err(ToolError::new(
            ErrorCode::InvalidArgs,
            format!("plan_id {id:?} is too short to be unambiguous."),
            format!(
                "Pass at least {MIN_PREFIX_CHARS} characters of the id, or the full id \
                 ({} characters).",
                FULL_ID_CHARS
            ),
        ));
    }
    let (plan, meta) = ctx.plans.get_for_read(id)?;

    // `file`, `offset` and `limit` are validated before anything is rendered, because a paging
    // argument that is accepted and then ignored is worse than one that is refused: the caller
    // cannot tell a short page from a filter that did nothing.
    if let Some(file) = &args.file
        && !plan.files.iter().any(|f| &f.path == file)
    {
        return Err(ToolError::new(
            ErrorCode::InvalidArgs,
            format!("file {file:?} is not in this plan."),
            "Use one of the paths ast_edit_preview printed for this plan.",
        ));
    }
    if args.limit == Some(0) {
        return Err(ToolError::new(
            ErrorCode::InvalidArgs,
            "limit 0 would show nothing.",
            "Pass limit 1 or more, or leave it out to show every hunk.",
        ));
    }

    let selected: Vec<&PlanFile> = plan
        .files
        .iter()
        .filter(|f| args.file.as_ref().is_none_or(|wanted| &f.path == wanted))
        .collect();

    let mut out = Bounded::new(ctx.tools.limits.max_output_bytes as usize);
    let mut escapes = EscapeCounts::default();
    out.line(plan_header(&plan, &meta, &mut escapes));
    for file in &selected {
        let (name, count) = escape_inline(&file.path);
        escapes.absorb(count);
        out.line(format!(
            "  {name}   {n} edit{s}   {pre} → {post} bytes   syntax errors {e0} → {e1}",
            n = file.edits.len(),
            s = if file.edits.len() == 1 { "" } else { "s" },
            pre = file.pre_size,
            post = file.post_size,
            e0 = file.pre_errors,
            e1 = file.post_errors,
        ));
    }

    // The diff, which is the reason this tool exists: a truncated preview points here for "the
    // rest", so an answer without it would leave that pointer aimed at nothing.
    out.blank();
    let offset = args.offset.unwrap_or(0);
    let limit = args.limit;
    // Paging is over the **plan's hunks**, not each file's: with `limit=1` a plan that covers
    // two files must answer with one hunk, not one per file. So the hunks are collected across
    // the selected files first and sliced once.
    let mut all: Vec<(String, Hunk)> = Vec::new();
    for file in &selected {
        match file_diff(ctx, file) {
            Ok(hunks) => all.extend(hunks.into_iter().map(|hunk| (file.path.clone(), hunk))),
            // A file that cannot be diffed is named, not fatal: the summary above is still true
            // and the caller learns why instead of getting silence.
            Err(e) => {
                let (name, count) = escape_inline(&file.path);
                escapes.absorb(count);
                out.line(format!("  [no diff for {name}: {:?}]", e.code));
            }
        }
    }
    let total = all.len();
    let start = offset.min(total);
    let end = limit.map_or(total, |n| start.saturating_add(n).min(total));

    let mut i = start;
    while i < end {
        let path = all[i].0.clone();
        let group: Vec<&Hunk> = all[i..end]
            .iter()
            .take_while(|(p, _)| *p == path)
            .map(|(_, hunk)| hunk)
            .collect();
        diff_block_from_hunks(&mut out, &path, &group, &mut escapes);
        i += group.len();
    }
    // An offset past the last hunk is also announced: silently printing a summary with no diff
    // is what the caller cannot tell apart from "this plan has no changes".
    if end < total || offset >= total {
        let past = if offset >= total {
            " (offset is past the last hunk)"
        } else {
            ""
        };
        out.line(format!(
            "[showing {} of {total} hunks from offset {start}{past}; page with offset= and limit=]",
            end - start
        ));
    }
    Ok(out.finish(&escapes, || {
        "[truncated; page with offset= and limit=, or narrow with file=]".to_string()
    }))
}

/// The hunks for one planned file: its current bytes against the bytes its edits produce.
///
/// Derived from the plan's **edit list**, not from a text comparison, and that is not a second
/// implementation of the diff preview builds. Preview compares two texts because its edit list has
/// just come back from the engine and the diff is what a reviewer reads *before* trusting that
/// list. Here the edit list **is** the stored plan, so the changed ranges are known exactly and
/// nothing has to be inferred - which also means this cannot disagree with what apply will write.
///
/// The file is read through the boundary, like everywhere else, and is not modified.
fn file_diff(ctx: &EditTools<'_>, file: &PlanFile) -> Result<Vec<Hunk>, ToolError> {
    let resolved = ctx.tools.boundary.resolve_read(&file.path)?;
    let (handle, _identity) = ctx.tools.boundary.open_read(&resolved)?;
    let mut buf = Vec::new();
    {
        use std::io::Read;
        handle
            .take(ctx.tools.limits.max_file_bytes.saturating_add(1))
            .read_to_end(&mut buf)
            .map_err(|_| {
                ToolError::new(
                    ErrorCode::IoError,
                    "The file could not be read.",
                    "Check the permissions of the file.",
                )
            })?;
    }
    let source = opencrayast_core::text::decode_utf8(&buf, ctx.tools.limits.max_file_bytes)?;
    if source.len() as u64 != file.pre_size {
        return Err(ToolError::new(
            ErrorCode::StalePlan,
            format!("{} changed since the plan was previewed.", file.path),
            "Run ast_edit_preview again to see the file as it is now.",
        ));
    }
    Ok(hunks_from_edits(source, &file.edits))
}

/// Hunks for one file from its edit list: [`CONTEXT_LINES`] of context on each side, the
/// original lines the edit touches as `Removed`, their replacements as `Added`.
///
/// **Line granularity on both sides**, the same as the diff `ast_edit_preview` prints, because
/// `ast_plan_show` is where a truncated preview sends the caller for the rest: the two must not
/// disagree about what a line became. So the replacement is expanded to whole lines - the text
/// before the edit on its first line and the text after it on its last - rather than shown as
/// the bare replaced bytes.
///
/// Derived from the edit list rather than by comparing two texts, which is not a second
/// implementation of preview's diff: there the edit list has just come back from the engine and
/// the diff is what a reviewer reads *before* trusting it. Here the edit list **is** the stored
/// plan, so the changed ranges are known exactly.
fn hunks_from_edits(source: &str, edits: &[opencrayast_edit::Edit]) -> Vec<Hunk> {
    // `split` leaves a trailing empty piece after a final newline. That is not a line of the
    // file, and showing it as trailing context would make the hunk headers disagree with the
    // preview diff for the same plan, so it is dropped.
    let lines: Vec<&str> = match source.strip_suffix('\n') {
        Some(without) => without.split('\n').collect(),
        None => source.split('\n').collect(),
    };
    // Byte offset of the start of each line, so an edit's byte range maps to line numbers.
    let mut starts: Vec<usize> = Vec::with_capacity(lines.len());
    let mut at = 0usize;
    for line in &lines {
        starts.push(at);
        at += line.len() + 1;
    }
    let line_of = |pos: usize| -> usize {
        match starts.binary_search(&pos) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        }
    };

    let mut hunks: Vec<Hunk> = Vec::new();
    for edit in edits {
        let first = line_of(edit.start);
        let last = if edit.end > edit.start {
            line_of(edit.end - 1)
        } else {
            first
        };
        let before = CONTEXT_LINES.min(first);
        let after = CONTEXT_LINES.min(lines.len().saturating_sub(last + 1));

        // The whole replacement, spread over whole lines: whatever precedes the edit on its
        // first line, then the replacement, then whatever follows it on its last line.
        let head = &lines[first][..edit.start - starts[first]];
        let tail = if edit.end > starts[last] {
            &lines[last][edit.end - starts[last]..]
        } else {
            ""
        };
        let joined = format!("{head}{}{tail}", edit.replacement);
        let added: Vec<&str> = joined.split('\n').collect();

        let mut hunk_lines: Vec<DiffLine> = Vec::new();
        for line in &lines[(first - before)..first] {
            hunk_lines.push(DiffLine {
                kind: DiffLineKind::Context,
                text: (*line).to_string(),
            });
        }
        for line in &lines[first..=last] {
            hunk_lines.push(DiffLine {
                kind: DiffLineKind::Removed,
                text: (*line).to_string(),
            });
        }
        for text in &added {
            hunk_lines.push(DiffLine {
                kind: DiffLineKind::Added,
                text: (*text).to_string(),
            });
        }
        for line in &lines[(last + 1)..(last + 1 + after)] {
            hunk_lines.push(DiffLine {
                kind: DiffLineKind::Context,
                text: (*line).to_string(),
            });
        }

        hunks.push(Hunk {
            old_start: (first - before + 1) as u32,
            old_lines: hunk_lines
                .iter()
                .filter(|l| !matches!(l.kind, DiffLineKind::Added))
                .count() as u32,
            new_start: (first - before + 1) as u32,
            new_lines: hunk_lines
                .iter()
                .filter(|l| !matches!(l.kind, DiffLineKind::Removed))
                .count() as u32,
            lines: hunk_lines,
        });
    }
    hunks
}

/// How many unchanged lines a hunk carries around each change.
const CONTEXT_LINES: usize = 3;

/// The shortest prefix `ast_plan_show` accepts, and the length of a full id (EDIT-MODEL).
const MIN_PREFIX_CHARS: usize = 10;
const FULL_ID_CHARS: usize = 28;

/// List the workspace's stored plans.
pub fn ast_plan_list(ctx: &EditTools<'_>, args: &PlanListArgs) -> Result<String, ToolError> {
    let limit = args.limit.unwrap_or(20);
    if limit == 0 || limit > 200 {
        return Err(ToolError::new(
            ErrorCode::InvalidArgs,
            format!("limit {limit} is outside 1..=200."),
            "Pass limit between 1 and 200, or leave it out for the default of 20.",
        ));
    }
    let apply_ctx = apply_context(ctx);
    let (plans, corrupt) = ctx.plans.list()?;
    let mut out = Bounded::new(ctx.tools.limits.max_output_bytes as usize);
    let mut escapes = EscapeCounts::default();
    out.line(format!(
        "{shown} plan{s} for workspace {ws} (state, expiry)",
        shown = plans.len().min(limit),
        s = if plans.len().min(limit) == 1 { "" } else { "s" },
        ws = ctx.tools.workspace_id,
    ));
    for summary in plans.iter().take(limit) {
        out.line(plan_row(ctx, &apply_ctx, summary, &mut escapes)?);
    }
    if plans.len() > limit {
        out.line(format!(
            "[truncated: showing {limit} of {} plans; raise limit]",
            plans.len()
        ));
    }
    if !corrupt.is_empty() {
        out.line(format!(
            "[{} unreadable plan file(s) ignored; a person must remove them]",
            corrupt.len()
        ));
    }
    Ok(out.finish(&escapes, || "[truncated; raise limit]".to_string()))
}

/// One row of `ast_plan_list`: id, note, files, edits, state, expiry.
///
/// The state comes from the journal, not from the plan store: a plan with no journal was never
/// applied, a journal in a terminal state was, and a plan whose expiry has passed is expired
/// whether or not anything was applied to it.
fn plan_row(
    ctx: &EditTools<'_>,
    apply_ctx: &ApplyContext<'_>,
    summary: &PlanSummary,
    escapes: &mut EscapeCounts,
) -> Result<String, ToolError> {
    let (plan, _) = ctx.plans.get_for_read(&summary.id)?;
    let note = match &plan.request.note {
        Some(n) => {
            let (escaped, count) = escape_inline(n);
            escapes.absorb(count);
            if escaped.is_empty() {
                "-".to_string()
            } else {
                note_line(&escaped)
            }
        }
        None => "-".to_string(),
    };
    // F3: the state is the journal's own state, never a guess. A `Prepared` journal means
    // originals are saved and **no target has been touched**, and a `RolledBack` one means every
    // target is back to its original; calling either of those "applied" is how an agent decides
    // the wrong thing about a plan. No journal at all means nothing was ever applied to it.
    let state = match opencrayast_edit::journal_of(apply_ctx, &summary.id) {
        Ok(manifest) => manifest.state.as_str(),
        Err(_) => "ready",
    };
    Ok(format!(
        "  {id}   {state}   expires {expires} UTC   {files} files   {edits} edits   {note}",
        id = summary.id,
        expires = hhmm_utc(summary.meta.expires_at),
        files = summary.files,
        edits = summary.edits,
    ))
}

fn plan_header(plan: &Plan, meta: &PlanMeta, escapes: &mut EscapeCounts) -> String {
    let (summary, count) = escape_inline(&plan.request.summary);
    escapes.absorb(count);
    let mut header = format!(
        "plan {}  (expires {} UTC)  — {} files, {} edits   {}",
        plan.id(),
        hhmm_utc(meta.expires_at),
        plan.files.len(),
        plan.files.iter().map(|f| f.edits.len()).sum::<usize>(),
        summary,
    );
    if let Some(note) = &plan.request.note {
        let (escaped, count) = escape_inline(note);
        escapes.absorb(count);
        header.push('\n');
        header.push_str(&note_line(&escaped));
    }
    header
}

// -------------------------------------------------------------------------------------------
// ast_edit_apply / ast_undo / ast_recover
// -------------------------------------------------------------------------------------------

/// Apply a previewed plan. Write mode; the full plan id only (E-15).
pub fn ast_edit_apply(ctx: &EditTools<'_>, args: &ApplyArgs) -> Result<String, ToolError> {
    let id = full_plan_id(&args.plan_id, "ast_edit_apply")?;
    let apply_ctx = apply_context(ctx);
    let result = opencrayast_edit::apply(&apply_ctx, &id)?;
    Ok(render_apply(ctx, &result))
}

fn render_apply(ctx: &EditTools<'_>, result: &ApplyResult) -> String {
    let mut out = Bounded::new(ctx.tools.limits.max_output_bytes as usize);
    let mut escapes = EscapeCounts::default();
    out.line(format!(
        "Applied {id} — {n} file{s} changed.",
        id = result.plan_id,
        n = result.changed.len(),
        s = if result.changed.len() == 1 { "" } else { "s" },
    ));
    // Per-file syntax errors come from the stored plan: `ApplyResult` reports which files were
    // written, and the plan is the record of what the edit was supposed to do to each of them.
    if let Ok((plan, _)) = ctx.plans.get_for_read(&result.plan_id) {
        for path in &result.changed {
            let file = plan.files.iter().find(|f| &f.path == path);
            let (name, count) = escape_inline(path);
            escapes.absorb(count);
            let (pre, post) = match file {
                Some(f) => (f.pre_errors.to_string(), f.post_errors.to_string()),
                // The plan is gone or unreadable: the counts are unknown, and printing a number
                // that came from nowhere would be worse than saying so.
                None => ("?".to_string(), "?".to_string()),
            };
            out.line(format!("  {name}   syntax errors {pre} → {post}"));
        }
    }
    out.line(format!(
        "Undo with ast_undo plan_id={} (kept {} days).",
        result.plan_id, ctx.tools.limits.journal_retention_days
    ));
    out.line(format!(
        "Next: verify the semantics — for example {}",
        result.suggestion
    ));
    out.line("      (lsp_diagnostics) on the changed files.".to_string());
    out.finish(&escapes, || {
        "[truncated; the plan is applied; ast_plan_show lists what changed]".to_string()
    })
}

/// Undo an applied plan. Write mode; the full plan id only.
pub fn ast_undo(ctx: &EditTools<'_>, args: &UndoArgs) -> Result<String, ToolError> {
    let id = full_plan_id(&args.plan_id, "ast_undo")?;
    let apply_ctx = apply_context(ctx);
    let result: UndoResult = opencrayast_edit::undo(&apply_ctx, &id)?;
    let mut out = Bounded::new(ctx.tools.limits.max_output_bytes as usize);
    let mut escapes = EscapeCounts::default();
    out.line(format!(
        "Undone {id} — {n} file{s} restored.",
        n = result.restored.len(),
        s = if result.restored.len() == 1 { "" } else { "s" },
    ));
    for path in &result.restored {
        let (name, count) = escape_inline(path);
        escapes.absorb(count);
        out.line(format!("  {name}"));
    }
    if result.restored.is_empty() {
        out.line(
            "  (nothing to restore: the files were already at their pre-plan content)".to_string(),
        );
    }
    out.line(format!(
        "The journal for {id} is kept {} days; after that undo is no longer possible.",
        ctx.tools.limits.journal_retention_days
    ));
    Ok(out.finish(&escapes, || {
        format!(
            "[truncated at {} bytes; rerun ast_undo plan_id={id} with a narrower journal to see \
             the rest]",
            ctx.tools.limits.max_output_bytes
        )
    }))
}

/// Recover every half-applied plan. Write mode; no arguments.
pub fn ast_recover(ctx: &EditTools<'_>) -> Result<String, ToolError> {
    let apply_ctx = apply_context(ctx);
    let recovered: Vec<Recovered> = opencrayast_edit::recover(&apply_ctx)?;
    let mut out = Bounded::new(ctx.tools.limits.max_output_bytes as usize);
    let mut escapes = EscapeCounts::default();
    if recovered.is_empty() {
        out.line("Nothing to recover.".to_string());
        return Ok(out.finish(&escapes, || {
            format!(
                "[truncated at {} bytes; ast_plan_list shows the plans whose journals are still \
                 open]",
                ctx.tools.limits.max_output_bytes
            )
        }));
    }
    out.line(format!(
        "Recovered {n} plan{s}.",
        n = recovered.len(),
        s = if recovered.len() == 1 { "" } else { "s" }
    ));
    for entry in &recovered {
        let (id, count) = escape_inline(&entry.plan_id);
        escapes.absorb(count);
        out.line(format!(
            "  {id}   {from} → {to}   {n} file{s} restored",
            from = entry.from.as_str(),
            to = entry.to.as_str(),
            n = entry.restored.len(),
            s = if entry.restored.len() == 1 { "" } else { "s" },
        ));
    }
    Ok(out.finish(&escapes, || {
        format!(
            "[truncated at {} bytes; ast_plan_list shows the plans whose journals are still open]",
            ctx.tools.limits.max_output_bytes
        )
    }))
}

/// The L3 apply context, carrying the write capability the shell minted for this call.
///
/// The capability is **not** invented here. `opencrayast-edit` closed the forge in SEC-FIX 4
/// and WCAP-1 opened exactly one door: `WriteCap::mint(&WritePermission)`, where `WritePermission`
/// has a private field and no `Default`, so only `opencrayast_core::config` can make one — and it
/// makes one only from parsed operator configuration. The shells put that capability on
/// [`ToolContext::write`]; this function forwards it, and nothing else in this crate can.
///
/// `None` is the honest answer for a read-only call and for every in-process caller that has no
/// configuration behind it: `apply` / `undo` / `recover` then answer `write_disabled` from L3.
fn apply_context<'a>(ctx: &'a EditTools<'a>) -> ApplyContext<'a> {
    ApplyContext::new(
        &ctx.tools.boundary,
        ctx.plans,
        ctx.journals,
        &ctx.tools.limits,
        ctx.state_dir,
        &ctx.tools.workspace_id,
        ctx.tools.write,
        ctx.lock_timeout,
        &opencrayast_edit::NoFault,
    )
}

/// A **full** plan id, or `invalid_args`.
///
/// This is the E-15 gate and it is deliberately not a prefix lookup: a 50-bit prefix is a
/// convenience for reading, never an authority for writing. `is_full_plan_id` is the same
/// predicate the plan store uses when it verifies a plan against its own name.
fn full_plan_id(candidate: &str, tool: &'static str) -> Result<String, ToolError> {
    let id = candidate.trim();
    if id.is_empty() {
        return Err(missing("plan_id", "a string"));
    }
    if !is_full_plan_id(id) {
        let short = id.chars().count() < FULL_ID_CHARS;
        return Err(ToolError::new(
            ErrorCode::InvalidArgs,
            if short {
                format!("{tool} needs the full plan id; {id:?} is a prefix.")
            } else {
                format!("{id:?} is not a plan id.")
            },
            format!(
                "Pass all {FULL_ID_CHARS} characters from ast_edit_preview. Abbreviations are \
                 accepted by ast_plan_show, never by a tool that writes."
            ),
        ));
    }
    Ok(id.to_string())
}

// -------------------------------------------------------------------------------------------
// Bounded output
// -------------------------------------------------------------------------------------------

/// An output buffer capped at `max_output_bytes`.
///
/// Every handler routes its text through this, so the cap cannot be forgotten on one path - and
/// because the handlers themselves are what the golden tests call, the capped string is the one
/// that reaches a client. There is no second renderer for tests to compare against.
struct Bounded<'a> {
    cap: usize,
    out: String,
    /// Room kept for the truncation notice, so the notice is never what gets cut.
    reserve: usize,
    /// Extra room reserved for a trailing block that must survive truncation.
    extra_reserve: usize,
    truncated: bool,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> Bounded<'a> {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            out: String::new(),
            reserve: 160,
            extra_reserve: 0,
            truncated: false,
            _marker: std::marker::PhantomData,
        }
    }

    /// Push one line if it fits. Returns whether it was accepted, so a caller that must close a
    /// block it could not finish can tell.
    fn line(&mut self, line: String) -> bool {
        if self.out.len() + line.len() + 1 > self.room() {
            self.truncated = true;
            return false;
        }
        self.out.push_str(&line);
        self.out.push('\n');
        true
    }

    /// Push one line regardless of the cap, and mark the output truncated.
    ///
    /// Only for a closing fence: the cap exists to bound what a caller reads, and a block left
    /// open is a rendering bug rather than a large output.
    fn force_line(&mut self, line: String) {
        self.out.push_str(&line);
        self.out.push('\n');
        self.truncated = true;
    }

    /// Lower the effective cap so that `bytes` more are guaranteed to fit.
    ///
    /// Used for a trailing block that must survive: the notice is short and the caller needs it,
    /// so the body gives up room rather than pushing the notice past the cap where it would be
    /// silently dropped. Passing 0 restores the full cap.
    fn reserve_for(&mut self, bytes: usize) {
        self.extra_reserve = bytes;
    }

    /// Whether `bytes` more can still be written. `room()` is the absolute limit this output may
    /// reach, so the bytes already written have to come off it - comparing a size against it
    /// directly would be wrong by however much is already there.
    fn fits(&self, bytes: usize) -> bool {
        self.out.len() + bytes <= self.room()
    }

    fn room(&self) -> usize {
        self.cap
            .saturating_sub(self.reserve)
            .saturating_sub(self.extra_reserve)
    }

    fn blank(&mut self) {
        if !self.out.is_empty() && !self.out.ends_with("\n\n") {
            self.out.push('\n');
        }
    }

    /// Text that already carries its own newlines, such as a fenced block.
    fn raw(&mut self, text: &str) {
        if self.out.len() + text.len() > self.room() {
            self.truncated = true;
            return;
        }
        self.out.push_str(text);
    }

    /// The finished string: the text, then the truncation notice if anything was cut, then the
    /// escaped-character report when anything had to be escaped.
    /// The hard end of the budget: append the truncation notice and the escape count, and return
    /// an output that is **never longer than `max_output_bytes`**.
    ///
    /// These two lines used to be appended unconditionally after the body had already spent the
    /// budget, so every truncated output came back roughly a notice longer than the cap the caller
    /// configured - reachable straight from config, since `Limits::validate` rejects only a zero
    /// `max_output_bytes`. The tail now takes its bytes out of the body: the body is trimmed at a
    /// line boundary (never mid-line, so no half-rendered diff line) to make room, and the tail
    /// goes on the end.
    ///
    /// When the cap is so small that even the tail cannot fit, the tail is what survives and the
    /// body goes: a caller who configured a 40-byte output cannot be given 100 bytes of it. Every
    /// cap a real deployment uses is orders of magnitude larger than the notice, so this only
    /// decides degenerate values.
    fn finish(mut self, escapes: &EscapeCounts, notice: impl Fn() -> String) -> String {
        let mut tail = String::new();
        if self.truncated {
            tail.push_str(&notice());
            tail.push('\n');
        }
        if escapes.total() > 0 {
            let mut parts: Vec<String> = Vec::new();
            if escapes.control > 0 {
                parts.push(format!("{} control", escapes.control));
            }
            if escapes.bidi > 0 {
                parts.push(format!("{} bidi", escapes.bidi));
            }
            if escapes.invisible > 0 {
                parts.push(format!("{} invisible", escapes.invisible));
            }
            tail.push_str(&format!(
                "[escaped: {} characters in names or paths]\n",
                parts.join(", ")
            ));
        }
        if tail.is_empty() {
            return self.out;
        }
        if tail.len() >= self.cap {
            // Nothing but the tail can fit, and the tail alone is already over the cap.
            let kept: String = tail.chars().take(self.cap).collect();
            return kept;
        }
        let budget = self.cap - tail.len();
        if self.out.len() > budget {
            // Back off to the last whole line that still fits.
            let cut = self.out[..budget].rfind('\n').map_or(0, |i| i + 1);
            self.out.truncate(cut);
        }
        self.out.push_str(&tail);
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CR-1: the 0-match line's searched text goes through `escape_inline`.
    ///
    /// The public handler cannot demonstrate this - `Boundary::resolve_read` rejects control and
    /// invisible characters before a path reaches here - so this calls the renderer with a path
    /// the boundary would never let through. Mutation self-proof: return the searched string
    /// unescaped from `zero_match_line` and the count assertion fails.
    #[test]
    fn edit10_19_the_zero_match_line_sanitises_the_searched_paths() {
        let mut escapes = EscapeCounts::default();
        let line = zero_match_line("src/a\u{202e}gpj.txt, b\u{200b}.ts", &mut escapes);
        assert!(
            !line.contains('\u{202e}') && !line.contains('\u{200b}'),
            "a raw bidi override or zero-width space reached the output: {line:?}"
        );
        assert!(line.contains("\\u{202e}"), "{line:?}");
        assert!(line.contains("\\u{200b}"), "{line:?}");
        // One bidi override and one zero-width space; the renderer counts each in its own bucket,
        // so this also pins that the counts are reported rather than swallowed.
        assert_eq!(escapes.total(), 2, "{line:?}");
        assert!(escapes.bidi >= 1, "{line:?}");
        assert!(
            line.starts_with("0 matches — nothing to change in "),
            "{line:?}"
        );
    }
}
