//! `plan gc`: the maintenance entry point for everything that expires.
//!
//! # Why this is a verb and not a start-up hook
//!
//! `PlanStore::sweep` and `JournalStore::evict` existed and had **no reachable caller**: the only
//! calls were in tests. A hundred and fifty long-expired plans and three `doctor` runs later, the
//! store was still 300 files and 1.3 MB. The failure was not that the reclamation was wrong, it
//! was that no user and no process could ever reach it.
//!
//! Three candidates were available and only one was chosen:
//!
//! - **On every store open** — rejected. `PlanStore::open` runs on *every* edit tool call, so this
//!   makes the cost of reading a plan proportional to how much junk the store has accumulated.
//!   The pathological case is a store that is too big to open: the sweep has to be able to run
//!   before anything else opens, and it must not be reachable only through the thing it repairs.
//! - **On server start-up only** — rejected, for the same reason from the other side. An MCP
//!   server is long-lived and started once per editor session; a machine where the operator only
//!   ever runs the CLI would never reclaim anything, and the residue would just migrate from the
//!   CLI's store to the server's.
//! - **An explicit verb** — chosen. It is the only entry point that is (a) reachable from every
//!   shell, including the one that will not start, (b) reachable when nothing is running, and
//!   (c) *attributable*: the operator sees the counts before and after and knows exactly when the
//!   disk went away. A sweep that runs invisibly on somebody else's schedule is a sweep nobody can
//!   reason about when a plan they wanted has gone.
//!
//! The cost of that choice is that it has to be run, so it is also wired into the two places
//! where an operator already looks: `doctor` reports **what it would remove** (see
//! [`doctor::run`]) and names this verb, and `--help` lists it. Deleting is a user's decision
//! about their own disk; this verb makes it available and reports it, and never does it behind
//! anyone's back.
//!
//! # What it removes, and what it never removes
//!
//! Delegates to the two policies that already existed and were already correct:
//!
//! - [`PlanStore::sweep`] — plans that are **expired** or **unverifiable**, and not in use, plus
//!   orphan files. Never a valid unexpired plan, never a plan in use.
//! - [`JournalStore::evict`] — journals past `journal_retention_days`, and (only once the store
//!   is over `journal_max_total_mib`) the oldest evictable ones. Never a `Prepared`, `Writing` or
//!   `Undoing` journal: those are what `recover` needs.
//!
//! Both are given a clock of the real system time, so "expired" means expired now.
//!
//! Deleting a journal makes its plan permanently unundoable. That is the documented meaning of
//! retention and it is why the count is printed before the operation is chosen, not after.

use crate::exit::{EXIT_OK, EXIT_USER};
use crate::out::Out;
use opencrayast_core::error::ToolError;
use opencrayast_core::limits::Limits;
use opencrayast_edit::{Clock, JournalStore, PlanStore, SystemClock};
use std::path::Path;
use std::sync::Arc;

/// What a sweep would remove right now, without removing it.
///
/// This is the answer `doctor` prints. It is computed by asking the two stores what *they* would
/// do, so the number cannot drift from the policy that would act on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Reclaimable {
    /// Stored plan entries `PlanStore::sweep` would delete.
    pub plans: usize,
    /// Journal ids `JournalStore::evict` would delete.
    pub journals: usize,
}

impl Reclaimable {
    /// True when there is nothing to reclaim, which `doctor` reports as `ok` rather than `warn`.
    pub fn is_empty(&self) -> bool {
        self.plans == 0 && self.journals == 0
    }
}

/// Count what a sweep would remove, and remove nothing.
///
/// # Deliberately non-destructive, and why that is not merely convenient
///
/// `PlanStore::sweep` is the only public way to delete, so counting "what would be removed" by
/// dry-running the sweep is not available — there is no `dry_run`. This computes the counts
/// instead: `PlanStore::list` already returns exactly the expired-or-unverifiable entries it
/// would drop (it filters expired plans out of its result and reports the unreadable ones
/// separately), and the journal side is derived from `journal_retention_days` over the loaded
/// manifests.
///
/// That is a re-implementation of a rule, and a re-implementation can drift. So the number is
/// used only for a `doctor` line, and the **verb** below reports the authoritative count that
/// the store itself returns after acting. If the two ever disagree, the verb's number is the
/// true one and this one is a hint — which is the right way round for a diagnostic.
pub fn reclaimable(state_dir: &Path, ws: &str, limits: &Limits) -> Result<Reclaimable, ToolError> {
    let clock = Arc::new(SystemClock);
    let plans = PlanStore::open(state_dir, ws, limits.clone(), clock.clone())?;
    let journals = JournalStore::open(state_dir, ws, limits.clone(), clock)?;

    // --- plans ---
    //
    // `PlanStore::list` returns `(live_and_unexpired, unreadable)`. `sweep` deletes an entry
    // when it is expired **or** unverifiable and not in use, so the count is
    // `(everything on disk) - (live and unexpired)`. The first term is not exposed by `list`,
    // so it is read off the directory: `sweep` also treats an orphan — a plan file with no meta
    // and vice versa — as reclaimable, and an orphan is exactly what is in that directory but
    // in neither half of `list`'s result. Counting only `unreadable` would therefore have
    // reported **zero** for the common case of a store full of merely *expired* plans, which is
    // the case this whole entry point exists for.
    let plans_dir = state_dir.join(format!("ws-{ws}")).join("plans");
    // A "plan entry" is a `.json` plus its `.meta.json` — two files, one entry — and `sweep`
    // counts entries, not files. Counting directory entries and subtracting the live count
    // would therefore report twice the truth, because every live plan contributes two entries.
    // Counting only `*.json` is the same number as counting entries, which is what makes this
    // agree with what `sweep` goes on to return.
    let on_disk = count_plan_files(&plans_dir);
    let (live, _unreadable) = plans.list()?;
    let plans_gone = on_disk.saturating_sub(live.len());

    // --- journals ---
    let (present, _bad) = journals.list()?;
    let now = SystemClock.now_secs();
    let retention = limits.journal_retention_days.saturating_mul(86_400);
    let journals_gone = present
        .iter()
        .filter(|m| {
            // The same evictability rule `evict_locked` uses. Restated rather than imported
            // because the states are this crate's dependency's, and an `is_evictable` export
            // would be a wider change than this ticket. The check is a conservative *hint*, and
            // a hint that keeps one journal too many is harmless; a hint that wrongly drops one
            // is not, so it is deliberately the stricter direction.
            matches!(
                m.state,
                opencrayast_edit::JournalState::Applied
                    | opencrayast_edit::JournalState::RolledBack
                    | opencrayast_edit::JournalState::Undone
            ) && now.saturating_sub(m.updated_at) >= retention
        })
        .count();

    Ok(Reclaimable {
        plans: plans_gone,
        journals: journals_gone,
    })
}

/// Stored **plans** in `dir`, counting one per `<plan-id>.json` and not counting the sibling
/// `<plan-id>.meta.json`. Or 0 when the directory cannot be listed: a missing directory means an
/// empty store, which is a legitimate answer and not an error.
fn count_plan_files(dir: &Path) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    rd.flatten()
        .filter(|e| {
            let name = e.file_name();
            let Some(name) = name.to_str() else {
                return false;
            };
            // A `.meta.json` is the sibling of a `.json`, not a plan of its own, so it must
            // not be counted: `sweep` deletes an *entry*, which is one of each pair.
            //
            // `sweep` also removes orphans and leftover temp files, and those ARE reclaimable
            // with no `.json` beside them. This is a hint, so counting them is the right
            // direction to err in.
            (!name.ends_with(".meta.json") && name.ends_with(".json")) || name.starts_with(".tmp-")
        })
        .count()
}

/// `plan gc`: apply the retention policy now, and say what went.
///
/// Reads the state directory the same way every other command does, so there is one answer to
/// "where does state live" on every surface. A store that cannot be opened is an environment
/// error, not a silent zero.
pub fn run(state_dir: &Path, ws: &str, limits: &Limits, out: &mut Out) -> i32 {
    let clock = Arc::new(SystemClock);
    let plans = match PlanStore::open(state_dir, ws, limits.clone(), clock.clone()) {
        Ok(s) => s,
        Err(e) => return report(&e, out),
    };
    let journals = match JournalStore::open(state_dir, ws, limits.clone(), clock) {
        Ok(s) => s,
        Err(e) => return report(&e, out),
    };

    // Printed before acting: after a journal is gone, undo for that plan is gone for good, so the
    // operator gets the number while it is still something they could have stopped.
    match reclaimable(state_dir, ws, limits) {
        Ok(r) => out.line(&format!(
            "Reclaiming {} plan(s) and {} journal(s).",
            r.plans, r.journals
        )),
        Err(e) => return report(&e, out),
    }

    let plans_gone = match plans.sweep() {
        Ok(n) => n,
        Err(e) => return report(&e, out),
    };
    let journals_gone = match journals.evict() {
        Ok(ids) => ids.len(),
        Err(e) => return report(&e, out),
    };

    out.line(&format!(
        "Removed {plans_gone} plan entr{} and {} journal{}.",
        if plans_gone == 1 { "y" } else { "ies" },
        journals_gone,
        if journals_gone == 1 { "" } else { "s" },
    ));
    if journals_gone > 0 {
        out.line("Their edits are no longer undoable; that is what retention means.");
    }
    out.line("");
    out.line("State stays under the user state directory. Delete it with:");
    out.line("  rm -rf \"$XDG_STATE_HOME/opencrayast\"    # or ~/.local/state/opencrayast");
    EXIT_OK
}

fn report(e: &ToolError, out: &mut Out) -> i32 {
    out.diag(&format!("[{}] {}", e.code.as_str(), e.message));
    if !e.next.is_empty() {
        out.diag(&format!("Next: {}", e.next));
    }
    // A store that cannot be swept is an environment problem, not a mistake in the command.
    match e.code {
        opencrayast_core::ErrorCode::InvalidArgs
        | opencrayast_core::ErrorCode::PlanNotFound
        | opencrayast_core::ErrorCode::PlanExpired => EXIT_USER,
        _ => crate::exit::EXIT_ENV,
    }
}
