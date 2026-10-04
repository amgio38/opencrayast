//! `plan list` and `plan show`: read the plan store, the way a person reads it.
//!
//! Both are **read-only** and both accept an abbreviated id, because reading is not authority
//! (EDIT-MODEL E-15: a prefix is a convenience for reading, never an authority for writing — the
//! mutating commands require the full id and are not in this ticket).
//!
//! The format follows `docs/TOOLS.md` §`ast_plan_show` / §`ast_plan_list`: the same fields, the
//! same error codes, spelled the same way. A person gets a friendlier layout and an extra line
//! telling them what to do next; the error *code* is the literal one, so a reader of either surface
//! sees the same word for the same condition.

use crate::exit::{EXIT_OK, EXIT_USER};
use crate::out::Out;
use opencrayast_core::error::ToolError;
use opencrayast_core::limits::Limits;
use opencrayast_edit::{Clock, JournalStore, PlanStore, SystemClock};
use std::path::Path;

/// How many plans `plan list` shows when `--limit` is not given (TOOLS.md: default 20).
pub const DEFAULT_LIMIT: usize = 20;

/// A plan id, possibly abbreviated, resolved to exactly one plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The full id, always.
    pub id: String,
}

/// `plan list`: the stored plans of this workspace, newest last, at most `limit` of them.
pub fn list(ws: &str, state_dir: &Path, limits: &Limits, limit: usize, out: &mut Out) -> i32 {
    let clock = std::sync::Arc::new(SystemClock);
    let store = match PlanStore::open(state_dir, ws, limits.clone(), clock.clone()) {
        Ok(s) => s,
        Err(e) => return report(&e, out, ExitCtx::List),
    };
    let (plans, unreadable) = match store.list() {
        Ok(v) => v,
        Err(e) => return report(&e, out, ExitCtx::List),
    };

    if plans.is_empty() {
        // An empty store prints a sentence, never a blank screen (invariant 2).
        out.line("No plans stored for this workspace.");
        out.line("Preview a change with the preview tool first; it stores a plan here.");
        if !unreadable.is_empty() {
            out.line(&format!(
                "Note: {} stored plan(s) could not be read and are not listed.",
                unreadable.len()
            ));
        }
        return EXIT_OK;
    }

    out.line(&format!(
        "{} plan(s) for this workspace{}:",
        plans.len(),
        if plans.len() > limit {
            format!(" (showing the first {limit})")
        } else {
            String::new()
        }
    ));
    out.line("");
    for p in plans.iter().take(limit) {
        // state: TOOLS.md lists ready / applied / undone / expired. `list()` already drops expired
        // plans, so everything shown here is ready unless a journal says otherwise.
        let state = state_of(state_dir, ws, &p.id, limits);
        let left = p.meta.expires_at.saturating_sub(clock.now_secs());
        out.line(&format!(
            "  {}  {} file(s)  {} edit(s)  {}  expires in {}s",
            p.id, p.files, p.edits, state, left
        ));
        out.line(&format!(
            "      note: {}",
            crate::out::escape_line(&note_of(&p.id, &store))
        ));
    }
    if plans.len() > limit {
        out.line("");
        out.line(&format!(
            "{} more plan(s) not shown; pass --limit to see more.",
            plans.len() - limit
        ));
    }
    if !unreadable.is_empty() {
        out.line("");
        out.line(&format!(
            "Warning: {} stored plan(s) could not be read.",
            unreadable.len()
        ));
    }
    EXIT_OK
}

/// `plan show`: one plan in full.
pub fn show(ws: &str, state_dir: &Path, limits: &Limits, prefix: &str, out: &mut Out) -> i32 {
    let clock = std::sync::Arc::new(SystemClock);
    let store = match PlanStore::open(state_dir, ws, limits.clone(), clock) {
        Ok(s) => s,
        Err(e) => return report(&e, out, ExitCtx::Show),
    };
    let (plan, meta) = match store.get_for_read(prefix) {
        Ok(v) => v,
        Err(e) => return report(&e, out, ExitCtx::Show),
    };

    out.line(&format!("Plan {}", plan.id()));
    out.line(&format!("  kind:    {}", plan.request.kind));
    out.line(&format!(
        "  summary: {}",
        crate::out::escape_line(&plan.request.summary)
    ));
    if let Some(note) = &plan.request.note {
        out.line(&format!("  note:    {}", crate::out::escape_line(note)));
    }
    let state = state_of(state_dir, ws, plan.id().as_str(), limits);
    out.line(&format!("  state:   {state}"));
    out.line(&format!(
        "  expires: {} (created {})",
        meta.expires_at, meta.created_at
    ));
    out.line(&format!("  files:   {}", plan.files.len()));
    out.line("");
    for f in &plan.files {
        out.line(&format!(
            "  {}  ({} edit(s), {} bytes -> {} bytes, syntax errors {} -> {})",
            crate::out::escape_line(&f.path),
            f.edits.len(),
            f.pre_size,
            f.post_size,
            f.pre_errors,
            f.post_errors
        ));
    }
    out.line("");
    out.line(
        "This plan has not been applied. Applying it is a separate command that needs the full id.",
    );
    EXIT_OK
}

/// Which of `ready` / `applied` / `undone` a plan is, from its journal.
fn state_of(state_dir: &Path, ws: &str, id: &str, limits: &Limits) -> &'static str {
    let clock = std::sync::Arc::new(SystemClock);
    let Ok(journals) = JournalStore::open(state_dir, ws, limits.clone(), clock) else {
        return "ready";
    };
    // A missing journal means never applied, which is the common case and is not an error here.
    match journals.load(id) {
        Ok(m) => match m.state {
            opencrayast_edit::JournalState::Applied => "applied",
            opencrayast_edit::JournalState::Undone => "undone",
            _ => "ready",
        },
        Err(_) => "ready",
    }
}

/// The note of a plan, read back from the store (the summary line already carries most of it).
fn note_of(id: &str, store: &PlanStore) -> String {
    match store.get_for_read(id) {
        Ok((p, _)) => p
            .request
            .note
            .clone()
            .unwrap_or_else(|| p.request.summary.clone()),
        Err(_) => String::new(),
    }
}

/// Which command is reporting, so the message says the right next step.
#[derive(Debug, Clone, Copy)]
enum ExitCtx {
    List,
    Show,
}

/// Print an error the way the tools spell it: the literal code first, then the message, then what
/// to do. Returns the process exit code.
fn report(e: &ToolError, out: &mut Out, ctx: ExitCtx) -> i32 {
    out.diag(&format!("[{}] {}", e.code.as_str(), e.message));
    let next = match ctx {
        ExitCtx::List => e.next.clone(),
        ExitCtx::Show => e.next.clone(),
    };
    if !next.is_empty() {
        out.diag(&format!("Next: {next}"));
    }
    crate::exit::exit_code_for_error(e)
}

/// Keep the unused import honest: `EXIT_USER` documents the class these commands return.
const _: i32 = EXIT_USER;
