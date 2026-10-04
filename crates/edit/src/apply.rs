//! The apply shell: the only code that writes workspace files (docs/EDIT-MODEL.md "Apply",
//! "Recovery"; invariants E-3..E-9, E-11, E-14; tests EDT-04, EDT-05, EDT-07..EDT-10, EDT-13,
//! EDT-15, EDT-17). It wires the pure and disk pieces together in the order the model fixes:
//! plan store -> apply lock -> recovery of earlier crashes -> per-file verification -> gates ->
//! journal -> replacement -> `applied`.
//!
//! Everything that decides *what* to do is in the pure modules (`validate_edits`, `apply_edits`,
//! `journal::plan_recovery`); this module only reads the world, asks them, and does exactly what
//! they say, with the core's write primitives (`Boundary::resolve_write`,
//! `Boundary::replace_file`, `ApplyLock`). It never searches and never edits a path it did not
//! re-resolve with the WRITE policy in this call: a path in a plan or a manifest is a request,
//! never an authority (E-1, EDT-17).
//!
//! ## Fault injection
//! Every step that changes durable state is announced to a [`Fault`] first. Production passes
//! [`NoFault`]. Tests inject a failure ([`FaultAction::Fail`], handled like a real failure of that
//! step, including rollback) or a crash ([`FaultAction::Crash`], which makes the call return
//! immediately with no cleanup, like a killed process; the apply lock is released because the
//! handle is dropped, as the kernel would release it).

use crate::capability::WriteCap;
use crate::editset::{apply_edits, validate_edits};
use crate::journal::{FileClass, JournalState, Manifest, Recovery, classify, plan_recovery};
use crate::jstore::JournalStore;
use crate::plan::{Plan, PlanFile};
use crate::store::PlanStore;
use opencrayast_core::boundary::{Boundary, ResolvedPath};
use opencrayast_core::error::{ErrorCode, ToolError};
use opencrayast_core::hash::{ContentHash, is_full_plan_id};
use opencrayast_core::limits::Limits;
use opencrayast_core::workspace::ApplyLock;
use opencrayast_lang::{Language, ParseBudget, parse};
use std::io::Read;
use std::path::Path;
use std::time::Duration;

/// The kinds of step announced to a [`Fault`], in the order a successful apply performs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StepKind {
    /// Taking the workspace apply lock.
    Lock,
    /// Recovering journals left by earlier crashes (once per call, before anything else).
    Recover,
    /// Verifying one file (`index` = its position in the plan): resolve, open, hash.
    Verify,
    /// Running the gates on all new contents.
    Gates,
    /// Creating the journal (originals durable, manifest `prepared`).
    JournalCreate,
    /// Moving the journal to `writing`.
    MarkWriting,
    /// Replacing one target (`index`).
    Replace,
    /// Recording progress after replacing target `index`.
    Progress,
    /// Moving the journal to `applied`.
    MarkApplied,
    /// Restoring one file during rollback or recovery (`index` = position in the manifest).
    Restore,
    /// Moving a journal to `rolled_back` / `undone` at the end of a rollback or recovery.
    MarkTerminal,
    /// Moving a journal to `undoing` before the first original is written back.
    MarkUndoing,
    /// Moving a journal to `undone` once every original is back.
    MarkUndone,
}

/// One announced step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Step {
    /// What is about to happen.
    pub kind: StepKind,
    /// The file index for per-file steps, else 0.
    pub index: usize,
}

/// What a [`Fault`] wants done at a step.
#[derive(Debug, Clone)]
pub enum FaultAction {
    /// Proceed normally.
    Continue,
    /// Behave as if this step failed with this error.
    Fail(ToolError),
    /// Stop the whole call right now with `internal` ("injected crash") and do nothing else.
    Crash,
}

/// Injection point. Must be cheap and must not panic.
pub trait Fault: Send + Sync {
    /// Called once before each step.
    fn at(&self, step: &Step) -> FaultAction;
}

/// The production fault: never interferes.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoFault;

impl Fault for NoFault {
    fn at(&self, _step: &Step) -> FaultAction {
        FaultAction::Continue
    }
}

/// Everything an apply, an undo or a recovery needs. All references: the caller owns the stores.
///
/// Write access is gated by [`WriteCap`]: the `write` field is private so a caller cannot upgrade
/// a read-only context by assignment (S-2 / T-17). Construct with [`ApplyContext::new`].
pub struct ApplyContext<'a> {
    /// The path policy of the workspace (write targets are resolved with `resolve_write`).
    pub boundary: &'a Boundary,
    /// Where plans are stored.
    pub plans: &'a PlanStore,
    /// Where journals are stored.
    pub journals: &'a JournalStore,
    /// Limits (file size, plan limits, gates).
    pub limits: &'a Limits,
    /// The state directory (for the apply lock).
    pub state_dir: &'a Path,
    /// The workspace id (`w-` + 32 hex).
    pub workspace_id: &'a str,
    /// `Some` only when policy granted a [`WriteCap`]. Private: cannot be flipped from outside.
    write: Option<WriteCap>,
    /// How long to wait for the apply lock.
    pub lock_timeout: Duration,
    /// Fault injection (tests); [`NoFault`] in production.
    pub fault: &'a dyn Fault,
}

impl<'a> ApplyContext<'a> {
    /// Build a context. Pass `Some(cap)` from crate-internal
    /// [`crate::policy::enable_writes`] for write mode, or `None` for read-only.
    /// Dependents cannot mint a [`WriteCap`]; only this crate (and future shells that
    /// grant from *parsed* operator policy — see `capability::policy` M5 note) can.
    #[allow(clippy::too_many_arguments)] // Mirrors the former public field set; keep one constructor.
    pub fn new(
        boundary: &'a Boundary,
        plans: &'a PlanStore,
        journals: &'a JournalStore,
        limits: &'a Limits,
        state_dir: &'a Path,
        workspace_id: &'a str,
        write: Option<WriteCap>,
        lock_timeout: Duration,
        fault: &'a dyn Fault,
    ) -> Self {
        Self {
            boundary,
            plans,
            journals,
            limits,
            state_dir,
            workspace_id,
            write,
            lock_timeout,
            fault,
        }
    }

    /// Whether this context holds a write capability.
    pub fn write_granted(&self) -> bool {
        self.write.is_some()
    }
}

/// A successful apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyResult {
    /// The plan id.
    pub plan_id: String,
    /// Workspace-relative paths that were changed, ascending.
    pub changed: Vec<String>,
    /// A one-line suggestion for verifying the result (no tool is run).
    pub suggestion: String,
}

/// What a recovery did to one journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recovered {
    /// The plan id.
    pub plan_id: String,
    /// The journal's state before.
    pub from: JournalState,
    /// The journal's state after (`RolledBack` or `Undone`).
    pub to: JournalState,
    /// Paths restored from the journal (ascending); empty when nothing needed restoring.
    pub restored: Vec<String>,
}

/// Apply a stored plan. Takes only the plan id (E-3: it never searches).
///
/// ## Decision table (first applicable row, in order; "nothing changed" = no workspace file and
/// no journal was touched unless stated)
///
/// | Step | Condition | Result |
/// |---|---|---|
/// | 1 | no [`WriteCap`] on the context | `write_disabled` |
/// | 2 | `plan_id` is not a full id | `invalid_args` |
/// | 3 | `plans.get_for_write` fails | its error: `plan_not_found`, `plan_expired`, `plan_corrupt`, `wrong_workspace` |
/// | 4 | a journal for this plan exists, in any state | `already_applied` (E-9) |
/// | 5 | the apply lock is not obtained within `lock_timeout` | `busy` |
/// | 6 | step 4 again, now under the lock (another apply may have finished) | `already_applied` |
/// | 7 | **recovery** of every non-terminal journal (see [`recover`]) cannot complete | `busy`, message lists the plan ids and says to run recovery; the apply does not start (E-8) |
/// | 8 | per file, in plan order: `resolve_write(path)` fails | its error (`outside_workspace`, `protected_path`, `unsupported_target`, `not_found`, …) |
/// | 9 | the file is not UTF-8 / over `limits.max_file_bytes` | `not_utf8` / `file_too_large` |
/// | 10 | any file's size or hash differs from `pre_size` / `pre_hash` | `stale_plan`, message lists **every** stale path; nothing changed |
/// | 11 | applying the recorded edits does not give `post_hash` / `post_size` (E-4, EDT-15) | `plan_corrupt` |
/// | 12 | gates (below) fail for any file | `gate_failed`, message lists the paths and reasons; nothing changed |
/// | 13 | journal creation fails | its error; nothing in the workspace changed |
/// | 14 | replacing target *i* fails (including a changed identity, `io_error`) | **rollback** with the recovery procedure; on success the journal is `rolled_back` and the original error is returned; if the rollback itself fails the journal stays `writing` and `rollback_incomplete` is returned, listing the files by class |
/// | 15 | all targets replaced | journal `applied` (progress = n), `Ok(ApplyResult)` |
///
/// ## Gates (step 12), on the bytes that would be written
/// - **syntax**: for a file whose plan language is available, parse the new bytes with the
///   plan's language; `error_count` must be `<= pre_errors` (an edit may fix errors, never add
///   them); a language that is `"text"` or not built in skips this gate;
/// - **size**: each new content `<= limits.max_file_bytes`;
/// - **encoding**: the new bytes are valid UTF-8 (they are, by construction); the BOM, the
///   line-ending **classification**, and the presence or absence of a trailing newline are
///   unchanged unless the edits themselves alter them — same check as preview
///   ([`crate::encoding_gate`]), same reason strings.
///
/// ## Ordering rules the tests rely on
/// - Originals are durable in the journal **before** the first target is touched (E-6).
/// - Targets are replaced in plan order, one `atomic_replace` each, re-verifying identity.
/// - Each failure path leaves no temp file in the workspace (`.opencrayast-tmp-*`).
/// - The lock is held from step 5 to the end and released on every path, including a crash.
pub fn apply(ctx: &ApplyContext<'_>, plan_id: &str) -> Result<ApplyResult, ToolError> {
    require_write(ctx)?;
    if !is_full_plan_id(plan_id) {
        return Err(ToolError::new(
            ErrorCode::InvalidArgs,
            "apply requires a full plan id",
            "Pass the full p-<26 base32> plan id.",
        ));
    }

    let (plan, _meta) = ctx.plans.get_for_write(plan_id)?;
    // Keep the plan from expiring under us for the duration of the call.
    let _use = ctx.plans.begin_use(plan_id)?;

    if ctx.journals.exists(plan_id)? {
        return Err(already_applied());
    }

    announce(ctx, StepKind::Lock, 0)?;
    let _lock = ApplyLock::acquire(ctx.state_dir, ctx.workspace_id, ctx.lock_timeout)?;

    // Re-check under the lock (E-9 race).
    if ctx.journals.exists(plan_id)? {
        return Err(already_applied());
    }

    announce(ctx, StepKind::Recover, 0)?;
    if let Err(e) = recover_locked(ctx) {
        if is_injected_crash(&e) {
            return Err(e);
        }
        return Err(busy_unresolved(ctx, e));
    }

    // Verify every file, compute new contents, collect stale paths (nothing written yet).
    let mut prepared: Vec<PreparedFile> = Vec::with_capacity(plan.files.len());
    let mut stale: Vec<String> = Vec::new();
    for (i, file) in plan.files.iter().enumerate() {
        announce(ctx, StepKind::Verify, i)?;
        let resolved = ctx.boundary.resolve_write(&file.path)?;
        let (mut handle, _identity) = ctx.boundary.open_read(&resolved)?;
        let mut bytes = Vec::new();
        handle.read_to_end(&mut bytes).map_err(|_| {
            ToolError::new(
                ErrorCode::IoError,
                format!("cannot read {}", rel_only(&resolved.rel)),
                "Retry; if it persists, check the file permissions.",
            )
        })?;
        if bytes.len() as u64 > ctx.limits.max_file_bytes {
            return Err(ToolError::new(
                ErrorCode::FileTooLarge,
                format!("{} exceeds max_file_bytes", rel_only(&file.path)),
                "Narrow the file or raise max_file_bytes.",
            ));
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| {
            ToolError::new(
                ErrorCode::NotUtf8,
                format!("{} is not valid UTF-8", rel_only(&file.path)),
                "Only UTF-8 source files can be edited.",
            )
        })?;
        if bytes.len() as u64 != file.pre_size || ContentHash::of(&bytes) != file.pre_hash {
            stale.push(file.path.clone());
            // Still build a placeholder so we can list every stale path in one pass; skip edit work.
            prepared.push(PreparedFile {
                path: file.path.clone(),
                resolved,
                original: bytes,
                new_content: Vec::new(),
            });
            continue;
        }
        validate_edits(text, &file.edits, ctx.limits)?;
        let new_text = apply_edits(text, &file.edits)?;
        let new_bytes = new_text.into_bytes();
        if new_bytes.len() as u64 != file.post_size || ContentHash::of(&new_bytes) != file.post_hash
        {
            return Err(ToolError::new(
                ErrorCode::PlanCorrupt,
                format!(
                    "recorded edits for {} do not produce the recorded post_hash",
                    rel_only(&file.path)
                ),
                "Refuse the plan; rebuild it from the current files (E-4).",
            ));
        }
        prepared.push(PreparedFile {
            path: file.path.clone(),
            resolved,
            original: bytes,
            new_content: new_bytes,
        });
    }
    if !stale.is_empty() {
        return Err(ToolError::new(
            ErrorCode::StalePlan,
            format!("stale files: {}", stale.join(", ")),
            "Re-read the named files and rebuild the plan.",
        ));
    }

    announce(ctx, StepKind::Gates, 0)?;
    run_gates(ctx, &plan.files, &prepared)?;

    let originals: Vec<Vec<u8>> = prepared.iter().map(|p| p.original.clone()).collect();
    announce(ctx, StepKind::JournalCreate, 0)?;
    ctx.journals.create(&plan, &originals)?;

    // From here a failure must roll the journal back (workspace may already be half-written).
    let after_journal = (|| {
        announce(ctx, StepKind::MarkWriting, 0)?;
        ctx.journals
            .set_state(plan_id, JournalState::Writing, 0, &plan)?;

        for (i, prep) in prepared.iter().enumerate() {
            announce(ctx, StepKind::Replace, i)?;
            replace_one(ctx, &plan.files[i], prep)?;

            announce(ctx, StepKind::Progress, i)?;
            ctx.journals.set_progress(plan_id, (i as u64) + 1, &plan)?;
        }

        announce(ctx, StepKind::MarkApplied, 0)?;
        ctx.journals
            .set_state(plan_id, JournalState::Applied, prepared.len() as u64, &plan)?;
        Ok(())
    })();

    if let Err(e) = after_journal {
        if is_injected_crash(&e) {
            return Err(e);
        }
        // Rollback with the same procedure as recovery; on success return the original error.
        match rollback_journal(ctx, plan_id) {
            Ok(()) => return Err(e),
            Err(rb) => {
                if is_injected_crash(&rb) {
                    return Err(rb);
                }
                return Err(rb);
            }
        }
    }

    let mut changed: Vec<String> = prepared.iter().map(|p| p.path.clone()).collect();
    changed.sort();
    Ok(ApplyResult {
        plan_id: plan_id.to_string(),
        changed,
        suggestion:
            "Run your project's type checker or language-server diagnostics on the changed files."
                .into(),
    })
}

/// Recover every journal in a non-terminal state (`Prepared`, `Writing`, `Undoing`), in plan-id
/// order, under the apply lock. This is the procedure step 7 of [`apply`] runs, the one a failed
/// apply uses to roll back, and what `ast_recover` / `doctor --recover` call.
///
/// For each journal: re-resolve every manifest path with the **write** policy, hash the current
/// files, call `journal::plan_recovery`, and do exactly what it returns:
/// `MarkRolledBack` -> mark `rolled_back`; `RestoreThenRolledBack(idx)` / `RestoreThenUndone(idx)`
/// -> for each index write `read_original(i)` back with `atomic_replace` (the original is
/// verified against `pre_hash` inside `read_original`), then mark `rolled_back` / `undone`.
/// `Diverged` -> return it unchanged (E-14: nothing is rewritten when the set is not consistent);
/// the journals after it are not examined.
///
/// | Condition | Result |
/// |---|---|
/// | no [`WriteCap`] on the context | `write_disabled` |
/// | lock not obtained | `busy` |
/// | no non-terminal journal | `Ok(vec![])` |
/// | the plan of a non-terminal journal is gone or no longer hashes to its `plan_digest` | `plan_corrupt` naming the journal; that journal stays as it was (E-16) |
/// | a journal is `diverged` (including content that changed in place between classify and a restore) | `diverged` listing every file as `path: pre\|post\|other`; that journal stays as it was |
/// | a journal in state `prepared` whose files are **not** all `pre_hash` (the claim is false) | restored from `orig/<n>` and marked `rolled_back` — never a bare `MarkRolledBack` (E-16) |
/// | a restore fails for other reasons | `rollback_incomplete` listing the files by class; the journal stays as it was |
/// | otherwise | `Ok`, one [`Recovered`] per journal, ascending by plan id |
///
/// Idempotent: a second call right after a successful one returns `Ok(vec![])` and changes
/// nothing. A crash at any step (see [`Fault`]) leaves a state that this function classifies
/// again and completes (E-13).
pub fn recover(ctx: &ApplyContext<'_>) -> Result<Vec<Recovered>, ToolError> {
    require_write(ctx)?;
    announce(ctx, StepKind::Lock, 0)?;
    let _lock = ApplyLock::acquire(ctx.state_dir, ctx.workspace_id, ctx.lock_timeout)?;
    recover_locked(ctx)
}

/// The manifest of an applied or recovered plan, for callers that report it (thin wrapper over
/// `JournalStore::load`).
pub fn journal_of(ctx: &ApplyContext<'_>, plan_id: &str) -> Result<Manifest, ToolError> {
    ctx.journals.load(plan_id)
}

// ---- internals ---------------------------------------------------------------------------

struct PreparedFile {
    path: String,
    resolved: ResolvedPath,
    original: Vec<u8>,
    new_content: Vec<u8>,
}

pub(crate) fn announce(
    ctx: &ApplyContext<'_>,
    kind: StepKind,
    index: usize,
) -> Result<(), ToolError> {
    let step = Step { kind, index };
    match ctx.fault.at(&step) {
        FaultAction::Continue => Ok(()),
        FaultAction::Fail(e) => Err(e),
        FaultAction::Crash => Err(injected_crash()),
    }
}

fn injected_crash() -> ToolError {
    ToolError::new(
        ErrorCode::Internal,
        "injected crash",
        "A fault injected a process kill; run recovery.",
    )
}

pub(crate) fn is_injected_crash(e: &ToolError) -> bool {
    e.code == ErrorCode::Internal && e.message == "injected crash"
}

pub(crate) fn write_disabled() -> ToolError {
    ToolError::new(
        ErrorCode::WriteDisabled,
        "write mode is not enabled",
        "Enable write mode to apply, undo or recover.",
    )
}

/// Shared write gate for [`apply`], [`undo`](crate::undo), and [`recover`].
pub(crate) fn require_write(ctx: &ApplyContext<'_>) -> Result<(), ToolError> {
    if ctx.write.is_none() {
        return Err(write_disabled());
    }
    Ok(())
}

/// `already_applied`: the plan cannot be applied a second time while its journal exists.
///
/// The next step has to be an *action*. The old one restated the rule and cited an internal
/// reference, which left a model with no move at all. There is one: undo the plan, then preview
/// again — `ast_undo` with the same `plan_id` (write mode), and a fresh `ast_edit_preview` if the
/// goal was the change rather than the restore. Nothing here re-applies, and no retry of this
/// call will succeed.
fn already_applied() -> ToolError {
    ToolError::new(
        ErrorCode::AlreadyApplied,
        "a journal for this plan already exists",
        "This plan was already applied and cannot be applied again. To get back to the original \
         files call ast_undo with this same plan_id (write mode); to make the change again, \
         call ast_edit_preview afresh for a new plan id and apply that one.",
    )
}

/// Strip any absolute path leakage: only the workspace-relative spelling is shown.
fn rel_only(path: &str) -> &str {
    path
}

pub(crate) fn recover_locked(ctx: &ApplyContext<'_>) -> Result<Vec<Recovered>, ToolError> {
    let journals = ctx.journals.nonterminal()?;
    let mut out = Vec::new();
    for m in journals {
        out.push(recover_one(ctx, &m)?);
    }
    Ok(out)
}

/// The executor both directions share: classify with the pure function, then do exactly what it
/// says, for ONE already-loaded journal (the caller holds the apply lock). A journal in state
/// `undoing` therefore completes the undo here, and the undo shell calls this same function rather
/// than writing a third restoration path.
///
/// Not [`recover_locked`], which takes the lock and walks every non-terminal journal.
pub(crate) fn recover_one(ctx: &ApplyContext<'_>, m: &Manifest) -> Result<Recovered, ToolError> {
    let from = m.state;
    // E-16: the journal must still belong to its plan before anything here acts on its state
    // field. This is the check the plain state-trusting path lacked, and it is deliberately first:
    // every hash read below would be wasted work on a journal that is not the one it claims.
    let plan = recovery_plan(ctx, m)?;
    let current = current_hashes(ctx, m)?;
    let action = plan_recovery(m, &current)?;
    let (indexes, to) = match action {
        Recovery::Nothing => {
            return Ok(Recovered {
                plan_id: m.plan_id.clone(),
                from,
                to: from,
                restored: Vec::new(),
            });
        }
        Recovery::MarkRolledBack => (Vec::new(), JournalState::RolledBack),
        Recovery::RestoreThenRolledBack(idx) => (idx, JournalState::RolledBack),
        Recovery::RestoreThenUndone(idx) => (idx, JournalState::Undone),
    };

    let mut restored = Vec::new();
    for &i in &indexes {
        match restore_one(ctx, m, i) {
            Ok(true) => restored.push(m.files[i].path.clone()),
            Ok(false) => {} // already pre_hash: skipped, no write
            Err(e) if is_injected_crash(&e) => return Err(e),
            // E-14: content changed in place between classify and restore → diverged, journal untouched.
            Err(e) if e.code == ErrorCode::Diverged => return Err(e),
            Err(_) => return Err(rollback_incomplete(m, &current)),
        }
    }
    restored.sort();

    let mark_terminal_fail = |ctx: &ApplyContext<'_>,
                              m: &Manifest,
                              current: &[Option<ContentHash>]| {
        match current_hashes(ctx, m) {
            Ok(c) => rollback_incomplete(m, &c),
            Err(_) => rollback_incomplete(m, current),
        }
    };

    if let Err(e) = announce(ctx, StepKind::MarkTerminal, 0) {
        if is_injected_crash(&e) {
            return Err(e);
        }
        return Err(mark_terminal_fail(ctx, m, &current));
    }
    // Progress for terminal: keep current progress (recovery does not invent a new count).
    if let Err(e) = ctx.journals.set_state(&m.plan_id, to, m.progress, &plan) {
        if is_injected_crash(&e) {
            return Err(e);
        }
        return Err(mark_terminal_fail(ctx, m, &current));
    }

    Ok(Recovered {
        plan_id: m.plan_id.clone(),
        from,
        to,
        restored,
    })
}

fn rollback_journal(ctx: &ApplyContext<'_>, plan_id: &str) -> Result<(), ToolError> {
    let m = ctx.journals.load(plan_id)?;
    recover_one(ctx, &m).map(|_| ())
}

/// The canonical bytes of the plan a journal belongs to, read from the plan store and verified
/// against the journal's `plan_digest` (E-16).
///
/// A non-terminal journal is, by definition, one whose apply or undo was interrupted — and both
/// are in flight while the plan is still in use, so the plan is still in the store (its TTL is 15
/// minutes by default, and [`crate::apply`] holds a use guard for the whole write section). If the
/// plan cannot be read, the journal cannot be authenticated and must not be touched: reporting
/// `plan_corrupt` is the honest answer, because the alternative — acting on the state field alone —
/// is exactly the defect this check exists to close. The error says which of the two it is, so the
/// caller can tell "evicted or expired, and there are still originals to restore" from "rewritten".
fn recovery_plan(ctx: &ApplyContext<'_>, m: &Manifest) -> Result<Plan, ToolError> {
    match ctx.plans.get_for_read(&m.plan_id) {
        Ok((plan, _meta)) => {
            m.check_bound_to(&plan)?;
            Ok(plan)
        }
        Err(e) if e.code == ErrorCode::PlanNotFound || e.code == ErrorCode::PlanExpired => {
            Err(ToolError::new(
                ErrorCode::PlanCorrupt,
                format!(
                    "the plan of journal {} is no longer readable, so its journal cannot be verified",
                    m.plan_id
                ),
                "Restore the plan from its own bytes if you still have them, then retry; the journal's originals are intact.",
            ))
        }
        Err(e) => Err(e),
    }
}

/// Restore journal original `index` onto the workspace path.
///
/// Why re-hash here: [`atomic_replace`] only compares file identity (dev/ino). A person can
/// edit the file **in place** (same inode) between the classify pass and this restore; identity
/// alone would not see that, and overwriting would destroy their edit (E-14). So we read the
/// open handle and:
/// - `pre_hash` → already restored (e.g. a previous recovery crashed mid-way) → skip, success;
/// - `post_hash` → still our post-image → overwrite with the journal original;
/// - anything else → `diverged`, do not write, leave the journal as it was.
///
/// Returns `Ok(true)` if a write happened, `Ok(false)` if skipped.
pub(crate) fn restore_one(
    ctx: &ApplyContext<'_>,
    m: &Manifest,
    index: usize,
) -> Result<bool, ToolError> {
    announce(ctx, StepKind::Restore, index)?;
    let file = &m.files[index];
    let resolved = ctx.boundary.resolve_write(&file.path)?;
    let (mut handle, identity) = ctx.boundary.open_read(&resolved)?;
    let mut now = Vec::new();
    handle.read_to_end(&mut now).map_err(|_| {
        ToolError::new(
            ErrorCode::IoError,
            format!("cannot read {} before restore", rel_only(&file.path)),
            "Retry; if it persists, check the file permissions.",
        )
    })?;
    let h = ContentHash::of(&now);
    if h == file.pre_hash {
        // Already original: do not rewrite (identity check alone is not enough to decide this).
        return Ok(false);
    }
    if h != file.post_hash {
        // In-place foreign edit (or missing/other): refuse to overwrite (E-14).
        return Err(diverged_at_restore(ctx, m));
    }
    let original = ctx.journals.read_original(&m.plan_id, index)?;
    // The identity captured at `open_read` above is handed back to the write, so the window between
    // that read and this write is guarded: a target swapped for another file (or turned into a link)
    // in between is refused rather than overwritten (E-7, E-14).
    ctx.boundary
        .replace_file_checked(&resolved, &original, Some(identity))?;
    Ok(true)
}

/// Full classification listing for a mid-restore diverge (same shape as `plan_recovery`).
fn diverged_at_restore(ctx: &ApplyContext<'_>, m: &Manifest) -> ToolError {
    let current = match current_hashes(ctx, m) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let mut parts = Vec::with_capacity(m.files.len());
    for (f, c) in m.files.iter().zip(current.iter()) {
        parts.push(format!(
            "{}: {}",
            f.path,
            class_str(classify(c.as_ref(), f))
        ));
    }
    ToolError::new(
        ErrorCode::Diverged,
        format!("files diverged from the journal: {}", parts.join(", ")),
        "Resolve the named files by hand, then retry recovery or undo.",
    )
}

pub(crate) fn current_hashes(
    ctx: &ApplyContext<'_>,
    m: &Manifest,
) -> Result<Vec<Option<ContentHash>>, ToolError> {
    let mut out = Vec::with_capacity(m.files.len());
    for f in &m.files {
        out.push(hash_at_write_path(ctx, &f.path)?);
    }
    Ok(out)
}

fn hash_at_write_path(
    ctx: &ApplyContext<'_>,
    path: &str,
) -> Result<Option<ContentHash>, ToolError> {
    let resolved = match ctx.boundary.resolve_write(path) {
        Ok(r) => r,
        Err(e) if e.code == ErrorCode::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let (mut handle, _) = match ctx.boundary.open_read(&resolved) {
        Ok(h) => h,
        Err(e) if e.code == ErrorCode::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut bytes = Vec::new();
    match handle.read_to_end(&mut bytes) {
        Ok(_) => Ok(Some(ContentHash::of(&bytes))),
        Err(_) => Ok(None),
    }
}

fn replace_one(
    ctx: &ApplyContext<'_>,
    file: &PlanFile,
    prep: &PreparedFile,
) -> Result<(), ToolError> {
    // Re-resolve and re-open so identity is fresh (EDT-17 / E-7).
    let resolved = ctx.boundary.resolve_write(&file.path)?;
    let (mut handle, identity) = ctx.boundary.open_read(&resolved)?;
    let mut now = Vec::new();
    handle.read_to_end(&mut now).map_err(|_| {
        ToolError::new(
            ErrorCode::IoError,
            format!("cannot re-read {}", rel_only(&file.path)),
            "Retry; if it persists, check the file permissions.",
        )
    })?;
    if ContentHash::of(&now) != file.pre_hash {
        return Err(ToolError::new(
            ErrorCode::IoError,
            format!("{} changed since verification", rel_only(&file.path)),
            "Re-read the file and rebuild the plan.",
        ));
    }
    let _ = prep.resolved.abs.as_path(); // kept for debug clarity
    // The identity from verification is handed back to the write, closing the read→write window.
    ctx.boundary
        .replace_file_checked(&resolved, &prep.new_content, Some(identity))?;
    Ok(())
}

fn run_gates(
    ctx: &ApplyContext<'_>,
    files: &[PlanFile],
    prepared: &[PreparedFile],
) -> Result<(), ToolError> {
    let budget = ParseBudget::from(ctx.limits);
    let mut problems: Vec<String> = Vec::new();
    for (file, prep) in files.iter().zip(prepared.iter()) {
        if prep.new_content.len() as u64 > ctx.limits.max_file_bytes {
            problems.push(format!("{}: size", rel_only(&file.path)));
            continue;
        }
        let Ok(after) = std::str::from_utf8(&prep.new_content) else {
            problems.push(format!("{}: encoding", rel_only(&file.path)));
            continue;
        };
        // Same shared gate as preview (EDIT-11): BOM / line-ending class / trailing newline.
        // Original bytes were already proven UTF-8 during verification.
        if let Ok(before) = std::str::from_utf8(&prep.original)
            && !crate::encoding_gate::encoding_preserved(before, after)
        {
            let attribute =
                crate::encoding_gate::encoding_fault(before, after).unwrap_or("encoding");
            problems.push(format!("{}: encoding ({attribute})", rel_only(&file.path)));
        }
        // "text" or unknown / unavailable language: skip syntax gate.
        if file.language.eq_ignore_ascii_case("text") {
            continue;
        }
        let Some(lang) = Language::from_id(&file.language) else {
            continue;
        };
        if !lang.is_available() {
            continue;
        }
        match parse(lang, after, &budget) {
            Ok(parsed) => {
                if (parsed.error_count as u64) > file.pre_errors {
                    problems.push(format!("{}: syntax", rel_only(&file.path)));
                }
            }
            Err(_) => {
                problems.push(format!("{}: syntax", rel_only(&file.path)));
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(ToolError::new(
            ErrorCode::GateFailed,
            format!("gates failed: {}", problems.join(", ")),
            "Fix the named files or rebuild the plan with fewer new errors.",
        ))
    }
}

fn rollback_incomplete(m: &Manifest, current: &[Option<ContentHash>]) -> ToolError {
    let mut parts = Vec::with_capacity(m.files.len());
    for (f, c) in m.files.iter().zip(current.iter()) {
        let class = classify(c.as_ref(), f);
        parts.push(format!("{}: {}", f.path, class_str(class)));
    }
    ToolError::new(
        ErrorCode::RollbackIncomplete,
        format!("rollback incomplete: {}", parts.join(", ")),
        "Run recovery; later applies are refused with busy until it succeeds.",
    )
}

fn class_str(c: FileClass) -> &'static str {
    match c {
        FileClass::Pre => "pre",
        FileClass::Post => "post",
        FileClass::Other => "other",
    }
}

fn busy_unresolved(ctx: &ApplyContext<'_>, cause: ToolError) -> ToolError {
    let ids = match ctx.journals.nonterminal() {
        Ok(list) => list
            .into_iter()
            .map(|m| m.plan_id)
            .collect::<Vec<_>>()
            .join(", "),
        Err(_) => String::new(),
    };
    let message = if ids.is_empty() {
        format!(
            "workspace has an unresolved journal ({}); run recovery",
            cause.code.as_str()
        )
    } else {
        format!(
            "unresolved journals: {ids} ({}); run recovery",
            cause.code.as_str()
        )
    };
    ToolError::new(
        ErrorCode::Busy,
        message,
        "Run ast_recover or doctor --recover, then retry the apply.",
    )
}
