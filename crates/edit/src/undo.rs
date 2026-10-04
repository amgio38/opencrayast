//! The undo shell: the user-requested half of what a crash leaves behind
//! (docs/EDIT-MODEL.md "Undo"; invariants E-8, E-9, E-14; tests EDIT8-xx). [`undo`] is not a
//! third restoration procedure: it decides with the same pure functions recovery uses
//! ([`journal::plan_undo`] for a fresh undo, [`journal::plan_recovery`] for an interrupted one) and
//! then drives the **same executor** [`recover_one`] does, so a direction can never be implemented
//! twice and a crash at any step is repairable by the same code.
//!
//! What is added here is only the part recovery cannot provide: the user's intent. Recovery always
//! converges a journal to a consistent state; undo says which consistent state — the one *before*
//! the plan was applied. The direction therefore comes from the journal state, never from the
//! caller ([`journal::Recovery::RestoreThenUndone`]).

use crate::apply::{
    ApplyContext, StepKind, announce, is_injected_crash, journal_of, recover_one, require_write,
};
use crate::journal::{JournalState, plan_undo};
use opencrayast_core::error::{ErrorCode, ToolError};
use opencrayast_core::hash::is_full_plan_id;

/// A successful undo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoResult {
    /// The plan id whose plan was undone.
    pub plan_id: String,
    /// Workspace-relative paths restored to their pre-plan content, ascending. A file that was
    /// already at `pre_hash` (an undo resumed by recovery) is not listed: this call did not write it.
    pub restored: Vec<String>,
    /// The journal state before the call.
    pub from: JournalState,
    /// The journal state after: always `Undone`.
    pub to: JournalState,
    /// The journal id, so a caller can report or evict it (`plan_id` is the same value today, and
    /// stating it separately keeps the result meaningful if the two ever diverge).
    pub journal_id: String,
}

/// Undo an applied plan: write every target back to the content it had before the plan was applied
/// (docs/EDIT-MODEL.md "Undo" steps 1–4).
///
/// ## Decision table (first applicable row; "nothing changed" = no workspace file and no journal was
/// touched unless stated)
///
/// | Step | Condition | Result |
/// |---|---|---|
/// | 1 | no [`crate::WriteCap`] on the context | `write_disabled` |
/// | 2 | `plan_id` is not a full id | `invalid_args` |
/// | 3 | no journal, and the plan IS still readable in the store | `plan_not_found`: **this plan was never applied**, or its journal was removed; either way there are no originals here, so there is nothing to undo and no retry will help |
/// | 4 | no journal and no readable plan either | `plan_not_found`; nothing is written |
/// | 5 | the journal does not verify | `plan_corrupt`; nothing is written |
/// | 6 | state `applied` (**fresh undo**): some `orig/<n>` does not hash to `pre_hash` | `plan_corrupt`; **nothing is written** — all originals are verified up front, before the first write, so an untrustworthy original can never reach a file |
/// | 7 | state `undoing` (**resumed undo**): an `orig/<n>` does not hash to `pre_hash` | `plan_corrupt` for that file, raised inside its `restore_one`; files restored before it stay restored and the journal stays `undoing`. E-8 allows resuming, and each file is still verified against `pre_hash` before it is written, so this differs from row 6 only in what was already done |
/// | 8 | the apply lock is not obtained within `lock_timeout` | `busy` |
/// | 9 | the journal state is not `applied` and not `undoing` | `already_applied` / `invalid_args` naming the state (the journal is untouched) |
/// | 10 | state `applied`: any file is not `post_hash` (including a file that is already `pre_hash`) | `diverged` listing **every** file as `path: pre\|post\|other`; **nothing is written** |
/// | 11 | state `undoing`: any file is neither `pre_hash` nor `post_hash` | `diverged`, same listing; **nothing is written** |
/// | 12 | the manifest cannot be set to `undoing` durably | its error; nothing was written |
/// | 13 | restoring file *i* fails for a reason other than a crash or a mid-restore diverge | `io_error` naming the files already restored; the journal stays `undoing`, and `recover` finishes the undo |
/// | 14 | a file changed in place between the classify pass and its restore | `diverged`, the full listing; the journal stays `undoing`, and `recover` classifies again |
/// | 15 | all files restored | journal `undone`, `Ok(UndoResult)` |
///
/// **Row 3 is `plan_not_found`, and that is a correction rather than a renaming.** It used to be
/// `journal_missing` ("the journal no longer exists"). That is a claim about the *journal store*,
/// and it is the wrong claim for the way callers actually reach row 3: asking to undo a plan that
/// was never applied. The one thing row 3 knows for certain is the thing a caller acts on — **there
/// is no journal, so there are no originals, so there is nothing to undo.** A journal really gone
/// while its plan really lives is not observable from here at all: the state that would prove the
/// plan was applied lives inside the journal that is gone, so the more specific message was never
/// earned. `journal_missing` keeps its documented spelling and stays in the CLI help table (the
/// taxonomy is not this function's to prune), but no undo path emits it.
///
/// The defaults explain how rare row 3 is beside row 4: `plan_ttl_minutes` = 15 and
/// `journal_retention_days` = 7, so a journal removed for AGE always outlives its plan, and by the
/// time anyone asks, the plan has been gone for a week. Row 4 is the common answer.
///
/// ## Semantics
/// - **Direction comes from the journal, not the caller**: an `undoing` journal is *completed*, not
///   restarted (EDIT-MODEL "Recovery", `undoing` row). A user who asks to undo twice gets the same
///   end state, and an undo interrupted by a crash is finished by the next `recover`.
/// - **All-or-nothing before the first write**: classification reads every file and the pure
///   function decides, so a single changed file refuses the whole undo with the full listing and
///   zero writes. After the first write the journal says `undoing`, which is enough for `recover`
///   to finish it — that is E-8, and it is why there is no rollback path here.
/// - **Every path is re-resolved** with the write policy in this call; a path in a manifest is a
///   request, never an authority (E-1, EDT-17).
/// - **Identity alone is not enough** before overwriting: a person can edit a file in place, so the
///   hash is re-read and re-compared immediately before each [`atomic_replace`] (E-14).
/// - `--only <file>` (partial undo) is deliberately absent: it is a decision about an inconsistent
///   tree that a person must make, and it belongs to the human CLI, not to this function.
pub fn undo(ctx: &ApplyContext<'_>, plan_id: &str) -> Result<UndoResult, ToolError> {
    require_write(ctx)?;
    if !is_full_plan_id(plan_id) {
        return Err(ToolError::new(
            ErrorCode::InvalidArgs,
            "undo requires a full plan id",
            "Pass the full p-<26 base32> plan id.",
        ));
    }

    // A missing journal is one situation, and the caller needs to know what it can rely on.
    // This arm deliberately does **not** claim the journal once existed: the evidence for that
    // would be the journal's own state, which is exactly what is missing, so the more specific
    // `journal_missing` ("the journal no longer exists") was a claim this code could not make. It
    // reports the consequence, which is certain and is what a caller acts on.
    if !ctx.journals.exists(plan_id)? {
        return Err(ToolError::new(
            ErrorCode::PlanNotFound,
            format!("no journal for {plan_id}, so there is nothing to undo"),
            "There are no originals on record for this plan, so there is nothing to undo and no retry will help. Rebuild from the current files.",
        ));
    }
    // Read once before taking the lock so a journal that does not verify is refused without
    // waiting for it; the decision below is made on the fresh read taken under the lock.
    journal_of(ctx, plan_id)?;

    announce(ctx, StepKind::Lock, 0)?;
    let _lock = opencrayast_core::workspace::ApplyLock::acquire(
        ctx.state_dir,
        ctx.workspace_id,
        ctx.lock_timeout,
    )?;

    // Re-read under the lock: another process may have finished an undo between the two reads.
    let manifest = journal_of(ctx, plan_id)?;
    // E-16: bind the journal to the plan before trusting its state field. The undo runs while the
    // plan is in use, so it is always readable here — and if it is not, the journal is not
    // something this call may rewrite a state of.
    let plan = ctx
        .plans
        .get_for_read(plan_id)
        .map(|(p, _)| p)
        .map_err(|_| {
            ToolError::new(
                ErrorCode::PlanCorrupt,
                format!("the plan of journal {plan_id} is no longer readable, so it cannot be undone"),
                "The journal's originals are intact; restore the plan from its own bytes, then retry.",
            )
        })?;
    manifest.check_bound_to(&plan)?;

    if manifest.state == JournalState::Undoing {
        // An interrupted undo: finish the direction the user already asked for. The executor and the
        // pure decision are the ones recovery uses, so this cannot drift from it.
        let recovered = recover_one(ctx, &manifest)?;
        return Ok(UndoResult {
            plan_id: plan_id.to_string(),
            restored: recovered.restored,
            from: JournalState::Undoing,
            to: recovered.to,
            journal_id: recovered.plan_id,
        });
    }

    // `plan_undo` refuses every other state (undoing handled above) and refuses a journal whose
    // files are not all `post_hash`, with the full classification. It writes nothing.
    let _ = plan_undo(&manifest, &current_hashes(ctx, &manifest)?)?;

    // Re-verify every original against its recorded `pre_hash` BEFORE the first write: an original
    // that does not hash to what the manifest says cannot be trusted, and writing it back would
    // silently replace a file with the wrong content.
    for i in 0..manifest.files.len() {
        ctx.journals.read_original(plan_id, i)?;
    }

    announce(ctx, StepKind::MarkUndoing, 0)?;
    ctx.journals
        .set_state(plan_id, JournalState::Undoing, 0, &plan)?;

    let mut restored = Vec::new();
    for i in 0..manifest.files.len() {
        match restore_one(ctx, &manifest, i) {
            Ok(true) => restored.push(manifest.files[i].path.clone()),
            // `plan_undo` said every file is `post_hash`, so a `false` here means the file became
            // `pre_hash` between the classify pass and now; there is nothing to write.
            Ok(false) => {}
            Err(e) if is_injected_crash(&e) => return Err(e),
            Err(e) if e.code == ErrorCode::Diverged => return Err(e),
            Err(_) => {
                return Err(io_error_restored(plan_id, &restored));
            }
        }
        if let Err(e) = announce(ctx, StepKind::Progress, i).and_then(|()| {
            ctx.journals
                .set_progress(plan_id, (i as u64) + 1, &plan)
                .map(|_| ())
        }) {
            if is_injected_crash(&e) {
                return Err(e);
            }
            return Err(io_error_restored(plan_id, &restored));
        }
    }

    announce(ctx, StepKind::MarkUndone, 0)?;
    ctx.journals.set_state(
        plan_id,
        JournalState::Undone,
        manifest.files.len() as u64,
        &plan,
    )?;

    restored.sort();
    Ok(UndoResult {
        plan_id: plan_id.to_string(),
        restored,
        from: manifest.state,
        to: JournalState::Undone,
        journal_id: plan_id.to_string(),
    })
}

// ---- internals ---------------------------------------------------------------------------
//
// These three are the ones already used by the apply/recovery shell, re-used rather than rewritten:
// a second restoration path would be a second set of bugs (and the ticket forbids it).

use crate::apply::{current_hashes, restore_one};

/// A restore failed for an ordinary I/O reason: say what was already restored, so a person knows
/// where the undo stopped. The journal stays `undoing`, and `recover` finishes the job.
fn io_error_restored(plan_id: &str, restored: &[String]) -> ToolError {
    let done = if restored.is_empty() {
        "no file was restored yet".to_string()
    } else {
        format!("restored: {}", restored.join(", "))
    };
    ToolError::new(
        ErrorCode::IoError,
        format!("undo of {plan_id} stopped part-way ({done})"),
        "The journal is left in state undoing; run recovery to finish the undo, then retry.",
    )
}
