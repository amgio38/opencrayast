//! Preview: an edit **request** becomes a stored **plan** (EDIT-MODEL §Preview, §Gates,
//! §Risk summary; E-1, E-4, E-5, E-12, E-15).
//!
//! This module never writes to the workspace (E-12). It reads through the boundary, asks the
//! engine for edit sets, computes the new content in memory, runs the gates on the bytes that
//! *would* be written, and stores the plan. The only write it performs is `PlanStore::put`,
//! and there is no code path that reaches a workspace file: no `File::create`, no `rename`, no
//! `write` outside `store`.
//!
//! ## The seven steps of EDIT-MODEL §Preview
//!
//! | Step | What happens | Where |
//! |---|---|---|
//! | 1 | validate arguments; resolve every path with the **read** policy; collect candidates (ignore rules, `max_scan_files`) | [`collect_candidates`] |
//! | 2 | per file: read size-capped, decode, `pre_hash`, ask the engine for the edit set | [`load`], [`rewrite_edits`], [`symbol_edits`] |
//! | 3 | **validate the edit set in this layer** (E-1), whatever produced it | [`editset::validate_edits`] |
//! | 4 | the new content in memory, and its `post_hash` | [`finish_file`] |
//! | 5 | the five gates, on the bytes that would be written | [`gate_file`] |
//! | 6 | the canonical plan, its derived id, the store, the diff data | [`preview`] |
//! | 7 | plan id, per-file summary, diff, risk summary, expiry | [`PreviewOutcome`] |
//!
//! Step 5 runs per file, as soon as that file's new content exists, rather than in one pass at
//! the end. The effect is identical - nothing is stored unless every file passes every gate -
//! and it means the original bytes do not have to be retained until the end of the preview to
//! prove anything about them.
//!
//! ## Determinism (EDIT-MODEL "Preview is deterministic")
//!
//! The plan id is the hash of the canonical plan bytes, so everything a reviewer is shown and
//! everything that decides what is written is inside it, and nothing else is: not the clock,
//! not the producing binary, not the absolute spelling of a path (files are stored by their
//! workspace-relative path), not the order candidates were collected in (files and edits are
//! sorted), not which spelling of a file won deduplication (the first in path order). The same
//! content with the same request produces the same id on every platform.
//!
//! ## Failure semantics
//!
//! | Situation | Result |
//! |---|---|
//! | the pattern does not parse | `invalid_pattern`, next step `ast_explain_pattern` |
//! | nothing matches anywhere | **success**: zero files, zero matches, nothing stored |
//! | the symbol matches several nodes | `ambiguous`, every candidate named, retry arguments given |
//! | the symbol matches none | `not_found`, next step `ast_outline` |
//! | no grammar for the file or the requested language | `unsupported_language`, supported ids listed |
//! | over a plan / scan / size limit | `limit_exceeded` / `file_too_large`, and the message says how to narrow |
//! | a parse or match budget ran out | `budget_exceeded` / `timeout` |
//! | outside the boundary, or protected | `outside_workspace` / `protected_path`, nothing read, nothing stored |
//! | a gate refuses the new content | `gate_failed`, naming the gate and the file |
//!
//! A file that cannot be edited during a **directory scan** is not an error: it is counted with
//! its reason in the risk summary (OUT-07), because a preview that silently omits a file looks
//! exactly like a preview where that file had nothing to change. A file the caller named
//! explicitly *is* an error, because then the caller asked about that file and deserves an
//! answer about it.

use crate::editset::{Edit, apply_edits, bytes_added, bytes_removed, validate_edits};
use crate::plan::{ENGINE_FORMAT, PLAN_FORMAT, Plan, PlanFile, PlanRequest, SUMMARY_MAX_BYTES};
use crate::rewrite::{RewriteRequest, rewrite_file};
use crate::store::PlanStore;
use opencrayast_core::boundary::{Boundary, FileIdentity, ResolvedPath};
use opencrayast_core::error::{ErrorCode, ToolError};
use opencrayast_core::hash::ContentHash;
use opencrayast_core::limits::Limits;
use opencrayast_core::walk::{WalkOptions, walk};
use opencrayast_lang::{Language, ParseBudget, ParsedFile, parse};
use opencrayast_query::outline::Symbol;
use opencrayast_query::pattern::{CompiledRule, Pattern, Rule, SearchBudget};
use opencrayast_query::{find_symbols, symbol_text};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// What a preview needs from its caller. Everything it is allowed to touch.
pub struct PreviewContext<'a> {
    /// The path policy. Every path goes through `resolve_read`: a preview reads, and being
    /// able to read is not a licence to write.
    pub boundary: &'a Boundary,
    /// Where plans are stored - the only thing this module writes to.
    pub plans: &'a PlanStore,
    /// Limits: file size, plan limits, parse budgets.
    pub limits: &'a Limits,
    /// The workspace id (`w-` + 32 hex) the plan is bound to (E-11).
    pub workspace_id: &'a str,
}

/// The five symbol operations of EDIT-MODEL §Edit kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolOp {
    /// The whole symbol, including its leading doc comment.
    Replace,
    /// The symbol's body, braces included.
    ReplaceBody,
    /// The whole symbol, including its leading doc comment.
    Delete,
    /// A line before the symbol, after its doc comment.
    InsertBefore,
    /// A line after the symbol, at the symbol's indentation.
    InsertAfter,
}

impl SymbolOp {
    /// The id used in messages and in a paste-ready retry argument.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Replace => "replace",
            Self::ReplaceBody => "replace_body",
            Self::Delete => "delete",
            Self::InsertBefore => "insert_before",
            Self::InsertAfter => "insert_after",
        }
    }

    /// Parse an operation id (the `operation` argument of `ast_edit_preview`).
    pub fn from_id(s: &str) -> Option<SymbolOp> {
        match s {
            "replace" => Some(Self::Replace),
            "replace_body" => Some(Self::ReplaceBody),
            "delete" => Some(Self::Delete),
            "insert_before" => Some(Self::InsertBefore),
            "insert_after" => Some(Self::InsertAfter),
            _ => None,
        }
    }

    /// Whether this operation needs replacement text.
    fn needs_text(self) -> bool {
        !matches!(self, Self::Delete)
    }
}

/// What the caller asked for. Both kinds end as the same plan format.
#[derive(Debug, Clone)]
pub enum EditRequest {
    /// Pattern to replacement, over one or more paths (EDIT-MODEL §`rewrite`).
    Rewrite {
        /// Language id the pattern is compiled for; every candidate file must be this language.
        language: String,
        /// Files or directories, resolved with the read policy.
        paths: Vec<String>,
        /// The pattern, compiled for `language`.
        pattern: String,
        /// The replacement template (`$NAME`, `$$$NAME`, `$$`).
        replacement: String,
        /// Optional constraint rule, compiled for `language`.
        ///
        /// Named `rule` after the argument table of `docs/TOOLS.md` §`ast_edit_preview`; the
        /// worked example in EDIT-MODEL spells the same field `constraints`.
        rule: Option<Rule>,
        /// Accept dropping a comment that sits inside a match but outside every capture. Off
        /// by default: a rewrite that silently eats a comment is a surprise, and the refusal
        /// (`comment_loss`) names the spans instead.
        allow_comment_loss: bool,
        /// One line for the reviewer, at most [`SUMMARY_MAX_BYTES`].
        summary: String,
        /// Optional caller note. Part of the hashed plan.
        note: Option<String>,
    },
    /// One named symbol in one file (EDIT-MODEL §`symbol`).
    Symbol {
        /// What to do to the symbol.
        operation: SymbolOp,
        /// The file, resolved with the read policy.
        path: String,
        /// The symbol's name, or its qualified name. Must resolve to exactly one node.
        symbol: String,
        /// Replacement text; required unless the operation is `delete`.
        text: Option<String>,
        /// One line for the reviewer.
        summary: String,
        /// Optional caller note. Part of the hashed plan.
        note: Option<String>,
    },
}

impl EditRequest {
    /// The `request.kind` this request produces.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Rewrite { .. } => "rewrite",
            Self::Symbol { .. } => "symbol",
        }
    }

    /// The reviewer's one-line summary.
    pub fn summary(&self) -> &str {
        match self {
            Self::Rewrite { summary, .. } | Self::Symbol { summary, .. } => summary,
        }
    }

    /// The caller's optional note.
    pub fn note(&self) -> Option<&str> {
        match self {
            Self::Rewrite { note, .. } | Self::Symbol { note, .. } => note.as_deref(),
        }
    }
}

/// Why a candidate file is not in the plan. Counted, never silent (OUT-07).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// No grammar is built in for this file.
    UnsupportedLanguage,
    /// Over `limits.max_file_bytes`.
    TooLarge,
    /// Not valid UTF-8.
    NotUtf8,
    /// A parse budget or timeout ran out.
    Budget,
    /// A protected path: not a target, read or written.
    Protected,
    /// It could not be opened (a link, a special file, a permission error).
    Unreadable,
    /// A match budget or the search deadline ran out.
    MatchBudget,
}

impl SkipReason {
    /// The machine-readable form, for the risk summary data.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedLanguage => "unsupported_language",
            Self::TooLarge => "too_large",
            Self::NotUtf8 => "not_utf8",
            Self::Budget => "budget",
            Self::Protected => "protected",
            Self::Unreadable => "unreadable",
            Self::MatchBudget => "match_budget",
        }
    }

    /// Classify an error raised while reading or parsing one file.
    fn of(error: &ToolError) -> Self {
        match error.code {
            ErrorCode::UnsupportedLanguage => Self::UnsupportedLanguage,
            ErrorCode::FileTooLarge => Self::TooLarge,
            ErrorCode::NotUtf8 => Self::NotUtf8,
            ErrorCode::BudgetExceeded | ErrorCode::Timeout => Self::Budget,
            ErrorCode::LimitExceeded => Self::MatchBudget,
            _ => Self::Unreadable,
        }
    }
}

/// One file that was not edited, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedFile {
    /// Workspace-relative path (the only path spelling ever shown).
    pub path: String,
    /// Why it is not in the plan.
    pub reason: SkipReason,
}

/// The summary a reviewer reads first (EDIT-MODEL §Risk summary).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RiskSummary {
    /// Files in the plan.
    pub files: usize,
    /// Edits in the plan.
    pub edits: u64,
    /// Bytes the plan **inserts** (sum of each edit's `replacement.len()`).
    /// The `+A` half of `+A −B bytes`; never includes removed bytes.
    pub bytes_added: u64,
    /// Bytes the plan **removes** (sum of each edit's `[start, end)` length).
    /// The `−B` half of `+A −B bytes`; never includes inserted bytes.
    /// Cap/limit math that needs inserted+removed uses [`crate::changed_bytes`].
    pub bytes_removed: u64,
    /// Planned files that had syntax errors **before** the edit.
    pub files_with_pre_errors: usize,
    /// Planned files that still have syntax errors after it. The syntax gate allows an equal
    /// count, so this is normally 0 or the pre-existing count.
    pub files_with_post_errors: usize,
    /// Files that were candidates but are not in the plan, each with its reason.
    pub skipped: Vec<SkippedFile>,
    /// Entries the walk never entered (ignored or over the depth ceiling).
    pub skipped_ignored: usize,
    /// Entries the walk refused (links, special files, unlistable directories).
    pub skipped_special: usize,
}

/// The kind of one diff line. The renderer adds the marker; this module only says what a line
/// is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    /// Unchanged, shown as context.
    Context,
    /// Present before the edit only.
    Removed,
    /// Present after the edit only.
    Added,
}

impl DiffLineKind {
    /// The unified-diff marker for this kind.
    pub fn prefix(self) -> char {
        match self {
            Self::Context => ' ',
            Self::Removed => '-',
            Self::Added => '+',
        }
    }
}

/// One line of a diff, without its trailing newline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    /// What kind of line this is.
    pub kind: DiffLineKind,
    /// The line's text.
    pub text: String,
}

/// One hunk: a run of changed lines with context around it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// First old line of the hunk (1-based).
    pub old_start: u32,
    /// Old lines the hunk spans.
    pub old_lines: u32,
    /// First new line of the hunk (1-based).
    pub new_start: u32,
    /// New lines the hunk spans.
    pub new_lines: u32,
    /// The lines, in order, each tagged with its kind.
    pub lines: Vec<DiffLine>,
}

/// The diff of one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    /// Workspace-relative path.
    pub path: String,
    /// Hunks in ascending order; empty when the file has no change.
    pub hunks: Vec<Hunk>,
}

/// The diff data for a whole plan. Text, fences, truncation and cleanup belong to the tools
/// layer (EDIT-MODEL: the diff is "bounded; `ast_plan_show` returns the rest"); this is enough
/// to render every line of the worked example in `docs/TOOLS.md`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diff {
    /// One entry per planned file, in plan order.
    pub files: Vec<FileDiff>,
}

/// What a preview produced.
#[derive(Debug, Clone)]
pub struct PreviewOutcome {
    /// The plan. Files strictly ascending by path, every edit set valid (E-1).
    ///
    /// Empty when nothing matched anywhere. A plan with no files is refused by
    /// [`Plan::check`] - "a plan must list at least one file" - so in that case nothing was
    /// stored and this is not a storable plan. [`Self::stored`] says which happened.
    pub plan: Plan,
    /// The plan id, or `None` when nothing was stored.
    pub plan_id: Option<String>,
    /// When the plan expires (clock seconds), or `None` when nothing was stored.
    pub expires_at: Option<u64>,
    /// Matches the engine found, including matches whose expansion was identical to the text
    /// they matched (which produce no edit).
    pub matches: u64,
    /// Whether the plan was stored. `false` only when there was nothing to store.
    pub stored: bool,
    /// The risk summary.
    pub summary: RiskSummary,
    /// The diff data.
    pub diff: Diff,
}

/// Build a plan from a request, store it, and return everything a reviewer needs.
///
/// The seven steps of EDIT-MODEL §Preview are not rearranged: arguments are validated before a
/// path is resolved, paths are resolved before anything is read, a file is read and hashed
/// before its edit set exists, the edit set is validated in this layer whatever produced it
/// (E-1), the new content and its hash are computed in memory, the gates run on those bytes,
/// and only then is the plan built, stored and diffed.
///
/// Nothing is written to the workspace on any path, including the failure paths: the first
/// write in this function is `PlanStore::put`, and every earlier refusal happens before it.
pub fn preview(ctx: &PreviewContext<'_>, req: &EditRequest) -> Result<PreviewOutcome, ToolError> {
    validate_request(ctx.limits, req)?;

    let mut skipped: Vec<SkippedFile> = Vec::new();
    let mut walk_ignored = 0usize;
    let mut walk_special = 0usize;
    let candidates =
        collect_candidates(ctx, req, &mut skipped, &mut walk_ignored, &mut walk_special)?;

    let mut planned: Vec<PlanFile> = Vec::new();
    let mut diffs: Vec<FileDiff> = Vec::new();
    let mut matches = 0u64;
    let mut gate_problems: Vec<String> = Vec::new();
    let mut files_with_pre_errors = 0usize;

    for candidate in &candidates {
        let loaded = match load(ctx, candidate) {
            Ok(loaded) => loaded,
            Err(e) => {
                // A file the caller named is answered about; one a scan found is counted.
                if candidate.explicit {
                    return Err(e);
                }
                skipped.push(SkippedFile {
                    path: candidate.path.rel.clone(),
                    reason: SkipReason::of(&e),
                });
                continue;
            }
        };

        let outcome = match req {
            EditRequest::Rewrite { .. } => rewrite_edits(ctx, req, &loaded)?,
            EditRequest::Symbol { .. } => symbol_edits(ctx, req, &loaded)?,
        };
        matches += outcome.matches;

        // No edit for this file is not a skip and not an error: the caller asked for a change
        // and this file had nothing to change. It is simply not in the plan.
        if outcome.file.edits.is_empty() {
            continue;
        }

        // Step 5, for this file, with its bytes still in hand.
        if let Err(reason) = gate_file(ctx, &loaded.text, &outcome.file) {
            gate_problems.push(format!("{}: {reason}", outcome.file.path));
            continue;
        }
        if outcome.file.pre_errors > 0 {
            files_with_pre_errors += 1;
        }
        planned.push(outcome.file);
        diffs.push(outcome.diff);
    }

    if !gate_problems.is_empty() {
        return Err(ToolError::new(
            ErrorCode::GateFailed,
            format!("gates failed: {}", gate_problems.join(", ")),
            gate_failed_next(req, &gate_problems),
        ));
    }

    // Strictly ascending by path, no duplicates: `Plan::check` refuses anything else.
    planned.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    planned.dedup_by(|a, b| a.path == b.path);
    diffs.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));

    let summary = RiskSummary {
        files: planned.len(),
        edits: planned.iter().map(|f| f.edits.len() as u64).sum(),
        bytes_added: planned.iter().map(|f| bytes_added(&f.edits)).sum(),
        bytes_removed: planned.iter().map(|f| bytes_removed(&f.edits)).sum(),
        files_with_pre_errors,
        files_with_post_errors: planned.iter().filter(|f| f.post_errors > 0).count(),
        skipped,
        skipped_ignored: walk_ignored,
        skipped_special: walk_special,
    };

    let plan = Plan {
        format: PLAN_FORMAT,
        workspace_id: ctx.workspace_id.to_string(),
        engine_format: ENGINE_FORMAT,
        request: PlanRequest {
            kind: req.kind().to_string(),
            summary: req.summary().to_string(),
            note: req.note().map(str::to_string),
        },
        files: planned,
    };

    // Nothing matched anywhere. That is a success, not a failure, and the message a renderer
    // prints says "0 matches" with a next step. There is no plan to store: `Plan::check`
    // refuses a plan with no files, and storing bytes that fail their own check would be worse
    // than storing nothing.
    if plan.files.is_empty() {
        return Ok(PreviewOutcome {
            plan,
            plan_id: None,
            expires_at: None,
            matches,
            stored: false,
            summary,
            diff: Diff::default(),
        });
    }

    // A plan that fails its own check is a defect here, not a caller mistake: `limit_exceeded`
    // from `Plan::check` means this module built something over a limit, which the limits were
    // supposed to prevent before the store was involved.
    plan.check(ctx.limits)?;
    let (plan_id, meta) = ctx.plans.put(&plan)?;

    Ok(PreviewOutcome {
        plan,
        plan_id: Some(plan_id),
        expires_at: Some(meta.expires_at),
        matches,
        stored: true,
        summary,
        diff: Diff { files: diffs },
    })
}

// -------------------------------------------------------------------------------------------
// Step 1: arguments and candidates
// -------------------------------------------------------------------------------------------

fn validate_request(limits: &Limits, req: &EditRequest) -> Result<(), ToolError> {
    let summary = req.summary();
    if summary.is_empty() || summary.len() > SUMMARY_MAX_BYTES {
        return Err(invalid_args(format!(
            "summary is empty or longer than {SUMMARY_MAX_BYTES} bytes."
        )));
    }
    if summary.chars().any(|c| (c as u32) < 0x20) {
        return Err(invalid_args("summary contains a control character."));
    }
    if let Some(note) = req.note()
        && note.len() as u64 > limits.note_max_bytes
    {
        return Err(ToolError::new(
            ErrorCode::LimitExceeded,
            format!(
                "note is {} bytes, over the {} byte limit",
                note.len(),
                limits.note_max_bytes
            ),
            "Shorten the note, or raise note_max_bytes.",
        ));
    }

    match req {
        EditRequest::Rewrite {
            language,
            paths,
            rule,
            ..
        } => {
            let lang = available_language(language)?;
            if paths.is_empty() {
                return Err(invalid_args("No paths were given."));
            }
            if let Some(rule) = rule {
                // A rule that does not compile is the same class of mistake as a pattern that
                // does not parse, and the same tool explains both.
                CompiledRule::compile(lang, rule).map_err(|e| {
                    invalid_pattern(format!("the constraint rule does not compile: {e:?}"))
                })?;
            }
            Ok(())
        }
        EditRequest::Symbol {
            operation,
            symbol,
            text,
            ..
        } => {
            if symbol.is_empty() {
                return Err(invalid_args("No symbol name was given."));
            }
            if operation.needs_text() && text.is_none() {
                return Err(invalid_args(format!(
                    "operation {} needs replacement text.",
                    operation.as_str()
                )));
            }
            Ok(())
        }
    }
}

/// One candidate file: where it is, whether the caller named it, and which file it is.
struct Candidate {
    path: ResolvedPath,
    /// True when the request named this exact file, so a failure is an error rather than a
    /// counted skip.
    explicit: bool,
    /// File identity (device + inode, or the Windows file id), for deduplication.
    identity: FileIdentity,
}

/// Resolve every path with the **read** policy and collect the candidate files.
///
/// `walk` decides whether a path is a file or a directory - a regular file start returns
/// exactly that file - so this does not have to, and does not, `stat` anything itself.
///
/// Deduplication is by **file identity**, never by path spelling (EDIT-MODEL §Preview): two
/// names for one inode, a hard link or a case-insensitive alias, are one file. Otherwise a
/// plan could hold two sets of edits for one file, and applying them would be a conflict no
/// gate looks at. The spelling kept is the first in path order, so the choice does not depend
/// on the order the paths were given in.
fn collect_candidates(
    ctx: &PreviewContext<'_>,
    req: &EditRequest,
    skipped: &mut Vec<SkippedFile>,
    walk_ignored: &mut usize,
    walk_special: &mut usize,
) -> Result<Vec<Candidate>, ToolError> {
    let requested: Vec<&str> = match req {
        EditRequest::Rewrite { paths, .. } => paths.iter().map(String::as_str).collect(),
        EditRequest::Symbol { path, .. } => vec![path.as_str()],
    };

    let mut out: Vec<Candidate> = Vec::new();
    for raw in requested {
        let resolved = ctx.boundary.resolve_read(raw)?;

        // A named file is explicit; anything a walk finds is not, because the caller never
        // spoke for those files individually.
        let explicit = match &req {
            EditRequest::Symbol { .. } => true,
            EditRequest::Rewrite { .. } => !path_is_directory(ctx, &resolved)?,
        };

        let result = walk(
            ctx.boundary,
            &resolved,
            &WalkOptions {
                max_files: ctx.limits.max_scan_files,
                respect_gitignore: true,
                extra_ignore: Vec::new(),
            },
        )?;
        *walk_ignored += result.skipped_ignored;
        *walk_special += result.skipped_special;
        if result.truncated {
            return Err(ToolError::new(
                ErrorCode::LimitExceeded,
                format!(
                    "the scan of {raw:?} stopped at the {} file limit",
                    ctx.limits.max_scan_files
                ),
                "Narrow the paths, or raise max_scan_files.",
            ));
        }

        for file in result.files {
            if refuse_if_protected(&file.rel) {
                if explicit {
                    return Err(protected_error(&file.rel));
                }
                skipped.push(SkippedFile {
                    path: file.rel,
                    reason: SkipReason::Protected,
                });
                continue;
            }
            match ctx.boundary.open_read(&file) {
                Ok((handle, identity)) => {
                    drop(handle);
                    out.push(Candidate {
                        path: file,
                        explicit,
                        identity,
                    });
                }
                Err(e) => {
                    if explicit {
                        return Err(e);
                    }
                    skipped.push(SkippedFile {
                        path: file.rel,
                        reason: SkipReason::of(&e),
                    });
                }
            }
        }
    }

    // Identity deduplication: exactly one candidate per file identity, keeping the
    // lexicographically smallest path spelling.
    //
    // Done by grouping rather than by keeping a "best so far" while scanning, because that is
    // where the order-dependence came from: a later, smaller spelling replaced the recorded one
    // but the earlier, now-larger candidate had already been kept, so one inode ended up with
    // two entries whenever the request listed the smaller path second. A `BTreeMap` from
    // identity to the smallest spelling is a function of the whole set, not of its order.
    let mut smallest: BTreeMap<(u64, u64), String> = BTreeMap::new();
    for c in &out {
        smallest
            .entry((c.identity.dev, c.identity.ino))
            .and_modify(|kept| {
                if c.path.rel < *kept {
                    *kept = c.path.rel.clone();
                }
            })
            .or_insert_with(|| c.path.rel.clone());
    }
    let chosen: BTreeSet<(u64, u64, String)> = smallest
        .into_iter()
        .map(|((dev, ino), rel)| (dev, ino, rel))
        .collect();
    out.retain(|c| chosen.contains(&(c.identity.dev, c.identity.ino, c.path.rel.clone())));

    out.sort_by(|a, b| a.path.rel.as_bytes().cmp(b.path.rel.as_bytes()));
    Ok(out)
}

/// Whether a resolved path is a directory, asked through the boundary rather than with a
/// `stat`: the boundary is the only thing in this project allowed to ask the disk.
fn path_is_directory(ctx: &PreviewContext<'_>, path: &ResolvedPath) -> Result<bool, ToolError> {
    match ctx.boundary.read_dir(path) {
        Ok(_) => Ok(true),
        // A listing refusal on a regular file is how the walk tells the two apart too; any
        // other refusal is passed on, because guessing "not a directory" for a permission
        // error would turn a real problem into a confusing one.
        Err(e) if e.message.contains("not a directory") => Ok(false),
        Err(e) => Err(e),
    }
}

/// Whether a workspace-relative path is a protected target.
///
/// `Boundary` keeps its own copy of the configured extra patterns and does not re-export it,
/// so this asks about the built-in deny list; a configured extra is caught by the boundary
/// itself when it resolves the path.
fn refuse_if_protected(rel: &str) -> bool {
    opencrayast_core::protected::is_protected(Path::new(rel), &[])
}

fn protected_error(rel: &str) -> ToolError {
    ToolError::new(
        ErrorCode::ProtectedPath,
        format!("{rel} is a protected path and is not an edit target."),
        "Pick a file that is not version-control metadata or a secret.",
    )
}

// -------------------------------------------------------------------------------------------
// Step 2: load one file
// -------------------------------------------------------------------------------------------

struct Loaded {
    path: ResolvedPath,
    language: Language,
    text: String,
    parsed: ParsedFile,
}

/// Read one file through the boundary, decode it, and parse it.
///
/// The order is the one the read tools use, for the same reasons: the boundary decides whether
/// the file may be read at all (a FIFO is refused here rather than blocking forever, BND-22),
/// one byte past the size limit is read so "over the limit" is detectable without holding the
/// whole file, the size check happens before UTF-8 validation, and the language is detected
/// before anything asks for a grammar.
fn load(ctx: &PreviewContext<'_>, candidate: &Candidate) -> Result<Loaded, ToolError> {
    use std::io::Read;

    let max_bytes = ctx.limits.max_file_bytes;
    let (handle, _identity) = ctx.boundary.open_read(&candidate.path)?;

    let mut buf = Vec::new();
    handle
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut buf)
        .map_err(|_| {
            ToolError::new(
                ErrorCode::IoError,
                "The file could not be read.",
                "Check the permissions of the file.",
            )
        })?;
    let text = opencrayast_core::text::decode_utf8(&buf, max_bytes)?;
    let language = Language::detect(&candidate.path.rel, text.lines().next())
        .ok_or_else(|| unsupported_language("this file name"))?;
    let parsed = parse(language, text, &ParseBudget::from(ctx.limits))?;
    Ok(Loaded {
        path: candidate.path.clone(),
        language,
        text: text.to_string(),
        parsed,
    })
}

// -------------------------------------------------------------------------------------------
// Step 2: the engine. One function per edit kind.
// -------------------------------------------------------------------------------------------

/// What the engine produced for one file.
struct FileOutcome {
    file: PlanFile,
    diff: FileDiff,
    matches: u64,
}

/// `rewrite`: every match of the pattern, expanded and re-indented at its site.
///
/// The pattern is compiled once per file, before the file is searched, so a pattern that does
/// not parse is refused without a search having run. Overlapping matches are resolved by the
/// rewrite engine itself - the outer match wins and `overlaps_dropped` reports the rest - so
/// the edit set handed to `validate_edits` is ascending and non-overlapping by construction
/// rather than by a repair pass afterwards.
fn rewrite_edits(
    ctx: &PreviewContext<'_>,
    req: &EditRequest,
    loaded: &Loaded,
) -> Result<FileOutcome, ToolError> {
    let EditRequest::Rewrite {
        language,
        pattern,
        replacement,
        rule,
        allow_comment_loss,
        ..
    } = req
    else {
        return Err(invalid_args("not a rewrite request"));
    };

    let lang = available_language(language)?;
    if loaded.language != lang {
        return Err(ToolError::new(
            ErrorCode::UnsupportedLanguage,
            format!(
                "the request is for {language} but this file is {}.",
                loaded.language.id()
            ),
            "Preview one language at a time, or narrow the paths to files of that language.",
        ));
    }

    let compiled = Pattern::compile(lang, pattern)
        .map_err(|e| invalid_pattern(format!("the pattern does not compile: {e:?}")))?;
    let compiled_rule = match rule {
        Some(r) => Some(CompiledRule::compile(lang, r).map_err(|e| {
            invalid_pattern(format!("the constraint rule does not compile: {e:?}"))
        })?),
        None => None,
    };

    let outcome = rewrite_file(
        &loaded.parsed,
        &loaded.text,
        &RewriteRequest {
            pattern: &compiled,
            rule: compiled_rule.as_ref(),
            replacement,
            allow_comment_loss: *allow_comment_loss,
            search_budget: SearchBudget {
                max_steps: 50_000_000,
                deadline: None,
                max_matches: ctx.limits.plan_max_edits as usize,
            },
            parse_budget: ParseBudget::from(ctx.limits),
            max_expansion_bytes: ctx.limits.plan_max_changed_bytes as usize,
        },
    )?;

    finish_file(ctx, loaded, outcome.edits, outcome.matches_found as u64)
}

/// `symbol`: one named symbol, resolved to exactly one node or refused.
///
/// Never a guess. Two candidates is `ambiguous` with both named, because an edit aimed at the
/// wrong one of two same-named functions is worse than a refusal; zero candidates is
/// `not_found`, pointing at the tool that lists them.
fn symbol_edits(
    ctx: &PreviewContext<'_>,
    req: &EditRequest,
    loaded: &Loaded,
) -> Result<FileOutcome, ToolError> {
    let EditRequest::Symbol {
        operation,
        symbol,
        text,
        ..
    } = req
    else {
        return Err(invalid_args("not a symbol request"));
    };

    let candidates = find_symbols(&loaded.parsed, &loaded.text, symbol);
    match candidates.len() {
        0 => {
            return Err(ToolError::new(
                ErrorCode::NotFound,
                format!("No symbol named {symbol:?} in this file."),
                "Run ast_outline on the file to see the symbols it defines.",
            ));
        }
        1 => {}
        _ => return Err(ambiguous(symbol, &candidates)),
    }

    let found = &candidates[0];
    let edit = symbol_edit(*operation, found, &loaded.text, text.as_deref())?;
    finish_file(ctx, loaded, vec![edit], 1)
}

/// The `ambiguous` refusal, with every candidate named and a paste-ready retry.
///
/// The retry names the first candidate in sorted order, which is a *suggestion of syntax*, not
/// a choice: the tool says which argument would disambiguate and leaves the decision to the
/// caller.
fn ambiguous(symbol: &str, candidates: &[Symbol]) -> ToolError {
    let mut names: Vec<String> = candidates
        .iter()
        .map(|s| format!("{} (line {})", s.qualified, s.start_line))
        .collect();
    names.sort();
    let suggestion = names
        .first()
        .and_then(|n| n.split(" (line").next())
        .unwrap_or(symbol)
        .to_string();
    ToolError::new(
        ErrorCode::Ambiguous,
        format!(
            "{symbol:?} matches {} symbols in this file: {}.",
            candidates.len(),
            names.join(", ")
        ),
        format!(
            "Retry with symbol={suggestion} to pick exactly one of them. This tool never \
             guesses which one you meant."
        ),
    )
}

/// The byte range one operation edits, and the text that replaces it.
///
/// `delete` takes the leading doc comment with the symbol, because deleting a function but
/// leaving its documentation would leave prose describing nothing. `replace` and
/// `insert_before` deliberately do not: a replacement supplies its own text, and an insertion
/// belongs between the doc comment and the symbol, or the doc comment would end up describing
/// the inserted text instead of the symbol.
fn symbol_edit(
    operation: SymbolOp,
    symbol: &Symbol,
    source: &str,
    text: Option<&str>,
) -> Result<Edit, ToolError> {
    let indent = indent_of(source, symbol.start_byte);
    let doc_start = doc_start_byte(source, symbol)?;

    // The range first, because the site's line ending depends on it: EDIT-MODEL §Preserving
    // file properties says replacement text takes "the ending in force at its own match site",
    // and **the site's start is this operation's edit start**, which is not always the symbol's
    // first byte. `ReplaceBody` starts at the body's `{`, which can be on a later line than the
    // declaration; `InsertAfter` starts at the symbol's end, which can be past the last line
    // break in the file. Taking the ending once from `symbol.start_byte` and using it for all
    // five operations gave a `ReplaceBody` on a mixed file the *declaration line's* ending, which
    // is precisely the file this rule exists for.
    let range = match operation {
        SymbolOp::Replace => (symbol.start_byte, symbol.end_byte),
        SymbolOp::Delete => (doc_start, symbol.end_byte),
        SymbolOp::ReplaceBody => body_range(source, symbol)?,
        SymbolOp::InsertBefore => (symbol.start_byte, symbol.start_byte),
        SymbolOp::InsertAfter => (symbol.end_byte, symbol.end_byte),
    };
    // `Delete` writes no text, so it has no ending to choose; asking anyway costs nothing and
    // keeps one code path.
    let eol = crate::rewrite::line_ending_at(source, range.0);

    // The caller's text is written with whatever line ending their editor used, which is not
    // necessarily the file's: pasting LF text into a CRLF file would leave the file with mixed
    // endings, which is invisible in a diff and irreversible by the next edit. A rewrite's
    // replacement is rewritten the same way (`rewrite_line_endings`, shared with the rewrite
    // engine's own expansion), so both edit kinds obey one rule.
    let rewritten = rewrite_line_endings(text.unwrap_or_default(), eol);

    Ok(match operation {
        SymbolOp::Replace | SymbolOp::ReplaceBody => Edit {
            start: range.0,
            end: range.1,
            replacement: rewritten,
        },
        SymbolOp::Delete => Edit {
            start: range.0,
            end: range.1,
            replacement: String::new(),
        },
        // An insertion carries the line break that puts it on its own line, in the site's ending.
        SymbolOp::InsertBefore => Edit {
            start: range.0,
            end: range.1,
            replacement: format!("{rewritten}{eol}{indent}"),
        },
        SymbolOp::InsertAfter => Edit {
            start: range.0,
            end: range.1,
            replacement: format!("{eol}{indent}{rewritten}"),
        },
    })
}

/// The bytes of a symbol's body, braces included, so the replacement supplies the whole body
/// and the symbol keeps its shape.
///
/// This takes the first `{` and the last `}` in the symbol's extent, which is right for an
/// ordinary braced body and *wrong* when the first `{` belongs to something else - a
/// destructuring parameter, a default value, an object type in a signature. Nothing here parses
/// the tree to find the real body, so what catches that case is the syntax gate: a replacement
/// aimed at the wrong range produces new content that does not parse, `post_errors` rises above
/// `pre_errors`, and the preview refuses the plan with `gate_failed`. That is fail-closed -
/// the reviewer never receives the wrong plan - but it is a refusal for the wrong stated reason,
/// so a caller who hits it should be told to re-indent or use `replace` instead.
fn body_range(source: &str, symbol: &Symbol) -> Result<(usize, usize), ToolError> {
    let extent = &source[symbol.start_byte..symbol.end_byte];
    let open = extent.find('{').map(|i| symbol.start_byte + i);
    let close = extent.rfind('}').map(|i| symbol.start_byte + i);
    match (open, close) {
        (Some(open), Some(close)) if close >= open => Ok((open, close + 1)),
        _ => Err(ToolError::new(
            ErrorCode::InvalidArgs,
            format!(
                "{} has no body between braces, so there is no body to replace.",
                symbol.qualified
            ),
            "Use operation=replace or operation=delete for a symbol like this one.",
        )),
    }
}

/// The first byte of the symbol's leading doc comment, or the symbol's own first byte.
fn doc_start_byte(source: &str, symbol: &Symbol) -> Result<usize, ToolError> {
    let text = symbol_text(source, symbol, true, 0)?;
    Ok(byte_of_line(source, text.first_line))
}

/// The indentation of the line a byte is on.
fn indent_of(source: &str, pos: usize) -> String {
    let line_start = source[..pos].rfind('\n').map_or(0, |i| i + 1);
    source[line_start..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// Put `text`'s line endings into the file's style.
///
/// Every `\n` that is not already part of a `\r\n` becomes `eol`, so LF text inserted into a
/// CRLF file arrives as CRLF and CRLF text inserted into an LF file arrives as LF. A `\r` that
/// stands alone (an old Mac file, or a stray carriage return inside a line) is left alone: it is
/// not a line ending this module is entitled to reinterpret.
fn rewrite_line_endings(text: &str, eol: &str) -> String {
    if eol == "\n" {
        // An LF file wants LF, and the only thing to fix is a CRLF the caller pasted.
        return text.replace("\r\n", "\n");
    }
    let mut out = String::with_capacity(text.len() + 8);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
                out.push_str(eol);
            } else {
                out.push('\r');
            }
        } else if c == '\n' {
            out.push_str(eol);
        } else {
            out.push(c);
        }
    }
    out
}

/// The byte offset of the start of a 1-based line.
fn byte_of_line(source: &str, line: usize) -> usize {
    if line <= 1 {
        return 0;
    }
    let mut seen = 1usize;
    for (i, b) in source.bytes().enumerate() {
        if b == b'\n' {
            seen += 1;
            if seen == line {
                return i + 1;
            }
        }
    }
    source.len()
}

// -------------------------------------------------------------------------------------------
// Steps 3 and 4: validate the edit set, then compute the new content and its hash
// -------------------------------------------------------------------------------------------

/// Steps 3 and 4 for one file: validate the edit set (E-1) whatever produced it, then compute
/// the new content in memory and its `post_hash`.
///
/// The edits are sorted here as well, so the plan is well formed by construction rather than
/// by trusting the producer to have sorted them: a plan whose edits are not ascending is
/// refused by `Plan::check`, and the sort costs nothing.
fn finish_file(
    ctx: &PreviewContext<'_>,
    loaded: &Loaded,
    edits: Vec<Edit>,
    matches: u64,
) -> Result<FileOutcome, ToolError> {
    let rel = loaded.path.rel.as_str();
    let source = &loaded.text;
    let pre_hash = ContentHash::of(source.as_bytes());
    let pre_size = source.len() as u64;
    let pre_errors = loaded.parsed.error_count as u64;

    let mut edits = edits;
    if edits.is_empty() {
        // Nothing to do for this file: not a skip, and not an error. The hashes are the
        // original's, which keeps the entry well formed if a caller looks at it.
        edits = Vec::new();
    } else {
        // Step 3: E-1, in this layer, whatever produced the set.
        validate_edits(source, &edits, ctx.limits)?;
        // Ascending by start, so the plan is well formed by construction rather than by
        // trusting the producer to have sorted them.
        edits.sort_by_key(|e| (e.start, e.end));
    }

    // Step 4: the new content, in memory, and its hash.
    let new_text = apply_edits(source, &edits)?;
    let post_errors = match parse(loaded.language, &new_text, &ParseBudget::from(ctx.limits)) {
        Ok(parsed) => parsed.error_count as u64,
        // A parse of the new bytes that cannot even run is not "no errors": it is worse than
        // the original, and the syntax gate refuses it as such.
        Err(_) => pre_errors.saturating_add(1),
    };

    let diff = diff_of(source, &new_text, rel);

    Ok(FileOutcome {
        file: PlanFile {
            path: rel.to_string(),
            language: loaded.language.id().to_string(),
            pre_hash,
            pre_size,
            pre_errors,
            post_hash: ContentHash::of(new_text.as_bytes()),
            post_size: new_text.len() as u64,
            post_errors,
            edits,
        },
        diff,
        matches,
    })
}

// -------------------------------------------------------------------------------------------
// Step 5: the five gates, on the bytes that would be written
// -------------------------------------------------------------------------------------------

/// The gates of EDIT-MODEL §Gates, run at preview as well as at apply, so a plan that apply
/// would refuse is refused now, with the reason.
///
/// | Gate | Rule | Decided by |
/// |---|---|---|
/// | `syntax` | `post_errors <= pre_errors` for every file | below |
/// | `size` | each new content within `max_file_bytes`; the plan's file, edit and changed-byte counts within the plan limits | below, and [`Plan::check`] |
/// | `path` | inside the boundary, a regular file, not protected | step 1, [`collect_candidates`] |
/// | `encoding` | UTF-8 only, with the BOM, the line endings and the presence of a trailing newline preserved | [`crate::encoding_gate::encoding_preserved`] |
/// | `stability` | applying the recorded edits yields exactly `post_hash` | [`stability_holds`] |
///
/// Returns the failed gate names joined with `+`, or `Ok(())`. The `encoding` entry names the
/// attribute that changed (`encoding (trailing newline)`), because a reviewer who is told only
/// that "encoding" failed cannot tell which of the three properties to fix.
fn gate_file(ctx: &PreviewContext<'_>, before: &str, file: &PlanFile) -> Result<(), String> {
    let mut failed: Vec<String> = Vec::new();

    if file.post_size > ctx.limits.max_file_bytes {
        failed.push("size".to_string());
    }
    if file.post_errors > file.pre_errors {
        failed.push("syntax".to_string());
    }
    if !stability_holds(before, file) {
        failed.push("stability".to_string());
    }
    // The new content is a `String`, so it is valid UTF-8 by construction; what can still be
    // broken is the BOM, the line-ending style and the trailing newline, which the encoding
    // gate keeps. The refusal names which of the three changed: "encoding" alone would leave a
    // reviewer guessing which property of their request did it.
    let after = reconstruct(before, file);
    if let Some(attribute) = crate::encoding_gate::encoding_fault(before, &after) {
        failed.push(format!("encoding ({attribute})"));
    }

    if failed.is_empty() {
        Ok(())
    } else {
        Err(failed.join("+"))
    }
}

/// The `stability` gate (E-4): applying the recorded edits to the original bytes must produce
/// exactly the recorded `post_hash` and `post_size`.
///
/// This is the gate with teeth. `post_hash` was computed by this module a moment ago, so
/// checking it against a second, independent application of the same edits is what turns "I
/// wrote a number" into "the number describes the plan". It is the gate that fails if the two
/// ever diverge - a `post_hash` taken from the original bytes, an edit set mutated after the
/// hash, a size computed by arithmetic that the bytes do not agree with - and apply, which
/// holds the real bytes, refuses the same mismatch after writing nothing.
fn stability_holds(before: &str, file: &PlanFile) -> bool {
    let Ok(after) = apply_edits(before, &file.edits) else {
        return false;
    };
    file.post_size == after.len() as u64 && file.post_hash == ContentHash::of(after.as_bytes())
}

/// The content a file's edits produce, for the encoding gate. `apply_edits` again rather than
/// caching: the gates are not on the hot path (a preview is bounded by the plan limits), and a
/// gate that reads a value computed by the code it is checking is not a gate.
fn reconstruct(before: &str, file: &PlanFile) -> String {
    apply_edits(before, &file.edits).unwrap_or_else(|_| before.to_string())
}

// -------------------------------------------------------------------------------------------
// Step 6: the diff data
// -------------------------------------------------------------------------------------------

/// How many unchanged lines a hunk carries around each change.
const CONTEXT_LINES: usize = 3;

/// The diff of one file, as data: hunks of tagged lines with context, which is what a unified
/// diff is made of and enough to render every line of the worked example in `docs/TOOLS.md`.
fn diff_of(before: &str, after: &str, path: &str) -> FileDiff {
    let old = split_lines(before);
    let new = split_lines(after);
    let hunks = build_hunks(&line_ops(&old, &new), &old, &new);
    FileDiff {
        path: path.to_string(),
        hunks,
    }
}

/// Lines split for diffing, with their line ending removed. A file's final line ending does not
/// produce a phantom empty last line, because a diff that counts one has an off-by-one in
/// every hunk that reaches the end of the file.
fn split_lines(text: &str) -> Vec<&str> {
    let body = text.strip_suffix('\n').unwrap_or(text);
    if body.is_empty() {
        return if text.is_empty() {
            Vec::new()
        } else {
            vec![""]
        };
    }
    body.split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect()
}

/// One line-level operation of a diff.
enum Op {
    Keep,
    Remove,
    Add,
}

/// A line-level diff of two texts.
///
/// The common prefix and suffix are trimmed first, which for an edit set reduces the problem to
/// the changed region - a few lines - and then a longest-common-subsequence table is built for
/// that region alone. The table is bounded: past [`MAX_REGION`] cells the region is reported
/// as removed-then-added, which is a correct diff of the same change without the interleaving,
/// and is not reachable for a plan that passed the size gate.
fn line_ops(old: &[&str], new: &[&str]) -> Vec<Op> {
    let mut ops: Vec<Op> = Vec::with_capacity(old.len() + new.len());

    let mut prefix = 0;
    while prefix < old.len() && prefix < new.len() && old[prefix] == new[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old.len() - prefix
        && suffix < new.len() - prefix
        && old[old.len() - 1 - suffix] == new[new.len() - 1 - suffix]
    {
        suffix += 1;
    }
    for _ in 0..prefix {
        ops.push(Op::Keep);
    }

    let old_mid = &old[prefix..old.len() - suffix];
    let new_mid = &new[prefix..new.len() - suffix];
    if old_mid.is_empty() {
        ops.extend((0..new_mid.len()).map(|_| Op::Add));
    } else if new_mid.is_empty() {
        ops.extend((0..old_mid.len()).map(|_| Op::Remove));
    } else if old_mid.len().saturating_mul(new_mid.len()) > MAX_REGION {
        ops.extend((0..old_mid.len()).map(|_| Op::Remove));
        ops.extend((0..new_mid.len()).map(|_| Op::Add));
    } else {
        ops.extend(lcs_ops(old_mid, new_mid));
    }

    for _ in 0..suffix {
        ops.push(Op::Keep);
    }
    ops
}

/// The largest LCS table this module will build, in cells.
const MAX_REGION: usize = 4_000_000;

/// The interleaved diff of two equally sized-ish regions, by the usual table walk.
fn lcs_ops(old: &[&str], new: &[&str]) -> Vec<Op> {
    let n = old.len();
    let m = new.len();
    let stride = m + 1;
    let mut table = vec![0u32; (n + 1) * stride];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i * stride + j] = if old[i] == new[j] {
                table[(i + 1) * stride + j + 1] + 1
            } else {
                table[(i + 1) * stride + j].max(table[i * stride + j + 1])
            };
        }
    }
    let mut ops = Vec::with_capacity(n + m);
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if old[i] == new[j] {
            ops.push(Op::Keep);
            i += 1;
            j += 1;
        } else if table[(i + 1) * stride + j] >= table[i * stride + j + 1] {
            ops.push(Op::Remove);
            i += 1;
        } else {
            ops.push(Op::Add);
            j += 1;
        }
    }
    ops.extend((i..n).map(|_| Op::Remove));
    ops.extend((j..m).map(|_| Op::Add));
    ops
}

/// Group the operations into hunks with [`CONTEXT_LINES`] of context around each change.
fn build_hunks(ops: &[Op], old: &[&str], new: &[&str]) -> Vec<Hunk> {
    // The line number each operation sits at, on each side, so a hunk knows where it starts.
    let mut positions: Vec<(usize, usize)> = Vec::with_capacity(ops.len());
    let (mut old_line, mut new_line) = (0usize, 0usize);
    for op in ops {
        positions.push((old_line, new_line));
        match op {
            Op::Keep => {
                old_line += 1;
                new_line += 1;
            }
            Op::Remove => old_line += 1,
            Op::Add => new_line += 1,
        }
    }

    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, op)| !matches!(op, Op::Keep))
        .map(|(i, _)| i)
        .collect();
    if changed.is_empty() {
        return Vec::new();
    }

    let mut hunks: Vec<Hunk> = Vec::new();
    let mut idx = 0usize;
    while idx < changed.len() {
        let first = changed[idx];
        let mut last = first;
        // One region is one hunk: extend while the next change is within two context windows,
        // which is the same rule a unified diff uses.
        while idx + 1 < changed.len() && changed[idx + 1] <= last + 2 * CONTEXT_LINES + 1 {
            idx += 1;
            last = changed[idx];
        }
        idx += 1;

        let start = first.saturating_sub(CONTEXT_LINES);
        let end = (last + CONTEXT_LINES + 1).min(ops.len());
        let (start_old, start_new) = positions[start];

        let mut lines: Vec<DiffLine> = Vec::with_capacity(end - start);
        let (mut old_lines, mut new_lines) = (0u32, 0u32);
        for op in ops.iter().take(end).skip(start) {
            let (kind, text) = match op {
                Op::Keep => {
                    let text = old
                        .get(start_old + old_lines as usize)
                        .copied()
                        .unwrap_or_default();
                    old_lines += 1;
                    new_lines += 1;
                    (DiffLineKind::Context, text)
                }
                Op::Remove => {
                    let text = old
                        .get(start_old + old_lines as usize)
                        .copied()
                        .unwrap_or_default();
                    old_lines += 1;
                    (DiffLineKind::Removed, text)
                }
                Op::Add => {
                    let text = new
                        .get(start_new + new_lines as usize)
                        .copied()
                        .unwrap_or_default();
                    new_lines += 1;
                    (DiffLineKind::Added, text)
                }
            };
            lines.push(DiffLine {
                kind,
                text: text.to_string(),
            });
        }
        hunks.push(Hunk {
            old_start: (start_old + 1) as u32,
            old_lines,
            new_start: (start_new + 1) as u32,
            new_lines,
            lines,
        });
    }
    hunks
}

// -------------------------------------------------------------------------------------------
// Errors
// -------------------------------------------------------------------------------------------

/// The advice for a gate refusal that is not the `syntax` mistake named below.
const GENERIC_GATE_NEXT: &str =
    "Fix the named files, or narrow the request so the new content is smaller.";

/// The next step for a gate refusal, chosen by which gate refused.
///
/// A `syntax` failure on a `symbol` request is nearly always one thing: `text` that does not stand
/// on its own. For `replace_body` the replacement must **include the surrounding braces** - the
/// edit replaces the bytes between `{` and `}` inclusive, so a bare body leaves the braces doubled
/// up and the file no longer parses. The refusal is correct, but the old next step ("fix the named
/// files") described the *file* as broken when the file was fine and the request was not, which
/// sends a caller off to repair something that was never damaged.
///
/// So the syntax case names the actual fix, and every other gate keeps the honest generic advice.
fn gate_failed_next(req: &EditRequest, problems: &[String]) -> String {
    // `gate_file` names the gate in its reason, so a `syntax` refusal carries that token.
    let syntax_failed = problems.iter().any(|p| p.contains("syntax"));
    let EditRequest::Symbol { operation, .. } = req else {
        return GENERIC_GATE_NEXT.to_string();
    };
    if !syntax_failed {
        return GENERIC_GATE_NEXT.to_string();
    }
    match operation {
        // The fix, not the rule: `replace_body` replaces the bytes between the braces inclusive,
        // so a bare body leaves the braces doubled up. That is the mistake worth naming.
        SymbolOp::ReplaceBody => {
            "`text` replaces the body including its surrounding braces, so it \
             must start with an opening brace and end with a closing one. Resend the whole body, \
             or use operation=replace to replace the symbol's text without braces."
                .to_string()
        }
        SymbolOp::Replace => {
            "`text` must be the complete replacement text of the symbol, parsed as the target \
             language on its own."
                .to_string()
        }
        SymbolOp::InsertBefore | SymbolOp::InsertAfter => {
            "`text` must parse as a complete item in the target language, not a fragment \
             (a bare expression, or half a declaration)."
                .to_string()
        }
        SymbolOp::Delete => "operation=delete takes no text; omit it from the call.".to_string(),
    }
}

/// An argument that does not fit, with a next step the caller can act on from this tool alone.
///
/// The advice used to point at "the tool's argument table" in `docs/TOOLS.md` - a document the
/// caller cannot read from either shell, since neither mode exposes the repository's docs. A next
/// step that cannot be followed is worse than none: it looks like an instruction and sends the
/// caller nowhere. So it names the arguments that decide every request this tool takes, which is
/// also what the shipped description publishes.
fn invalid_args(message: impl Into<String>) -> ToolError {
    ToolError::new(
        ErrorCode::InvalidArgs,
        message,
        "For kind=rewrite pass language, pattern, replacement and paths. For kind=symbol pass \
         path, symbol and operation, plus text for replace, replace_body, insert_before and \
         insert_after. The shipped description lists which argument each kind requires.",
    )
}

/// A pattern or rule that does not compile, pointing at the tool that explains patterns.
fn invalid_pattern(message: impl Into<String>) -> ToolError {
    ToolError::new(
        ErrorCode::InvalidPattern,
        message,
        "Run ast_explain_pattern to see where the pattern stops parsing.",
    )
}

fn unsupported_language(asked: &str) -> ToolError {
    let mut ids: Vec<&str> = Language::all()
        .iter()
        .filter(|l| l.is_available())
        .map(|l| l.id())
        .collect();
    ids.sort_unstable();
    ToolError::new(
        ErrorCode::UnsupportedLanguage,
        format!(
            "No grammar is built in for {asked} in this build. Supported: {}.",
            ids.join(", ")
        ),
        "Use one of the supported languages, or rebuild with this build's language features.",
    )
}

/// The language for a request, refused with the supported list when it has no grammar.
fn available_language(id: &str) -> Result<Language, ToolError> {
    match Language::from_id(id) {
        Some(lang) if lang.is_available() => Ok(lang),
        _ => Err(unsupported_language(id)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencrayast_core::text::LineEnding;

    /// The site's ending is the site's own, never the file's.
    #[test]
    fn the_site_decides_the_line_ending_not_the_file() {
        let mixed = "log(1);\r\nlog(2);\n";
        assert_eq!(
            crate::encoding_gate::classify_line_endings(mixed),
            LineEnding::Mixed
        );
        // A site inside the CRLF line, and a site inside the LF line.
        assert_eq!(crate::rewrite::line_ending_at(mixed, 0), "\r\n");
        assert_eq!(crate::rewrite::line_ending_at(mixed, 9), "\n");
        // A single-style file answers with its own style at every site.
        let crlf = "a\r\nb\r\n";
        assert_eq!(crate::rewrite::line_ending_at(crlf, 0), "\r\n");
        assert_eq!(crate::rewrite::line_ending_at(crlf, 3), "\r\n");
        // A site with no terminator after it falls back to the file's classification.
        assert_eq!(crate::rewrite::line_ending_at("abc", 0), "\n");
        assert_eq!(crate::rewrite::line_ending_at("abc\r\n", 0), "\r\n");
    }

    /// The caller's text is put into the site's ending, in both directions, and a lone `\r` is
    /// left alone because it is not a line ending this module may reinterpret.
    #[test]
    fn caller_text_is_rewritten_into_the_sites_ending() {
        assert_eq!(rewrite_line_endings("a\nb", "\r\n"), "a\r\nb");
        assert_eq!(rewrite_line_endings("a\r\nb", "\n"), "a\nb");
        assert_eq!(rewrite_line_endings("a\nb", "\n"), "a\nb");
        assert_eq!(rewrite_line_endings("a\r\nb", "\r\n"), "a\r\nb");
        // No doubled carriage return when the text is already CRLF.
        assert!(!rewrite_line_endings("a\r\nb", "\r\n").contains("\r\r"));
        // A lone `\r` survives: it may be a character inside a string literal.
        assert_eq!(rewrite_line_endings("a\rb", "\n"), "a\rb");
        // Empty and break-free text are unchanged.
        assert_eq!(rewrite_line_endings("", "\r\n"), "");
        assert_eq!(rewrite_line_endings("ab", "\r\n"), "ab");
    }
}
