//! The on-disk journal store (docs/EDIT-MODEL.md "Journal"; invariants E-6, E-9, E-13; tests
//! EDT-05, EDT-16, EDT-25). Layout, under `<state_dir>/ws-<workspace id>/journal/`:
//!
//! ```text
//! <plan-id>/manifest.json   canonical Manifest bytes, mode 0600
//! <plan-id>/orig/<n>        the original bytes of file n (same order as the manifest), mode 0600
//! .tmp-<random>/            a journal being built, or one being deleted; never read as a journal
//! ```
//!
//! This module does the **disk side only**: it never decides what recovery should do (that is
//! `journal::plan_recovery`), and it never touches workspace files. Like the plan store it treats
//! its directory as attacker-reachable state: directories are `0700` and re-verified by identity
//! on every operation, files are `0600`, regular, not symlinks, owned by the current user, and
//! every original is re-hashed against the manifest before it is handed out (EDT-25).

use crate::fsutil::{
    DirIdentity, capture_dir_identity, check_dir_identity, is_workspace_id, make_tmp_dir,
    mkdir_private, verify_private_file, write_exclusive,
};
use crate::journal::{JournalFile, JournalState, Manifest};
use crate::plan::Plan;
use crate::store::Clock;
use opencrayast_core::error::{ErrorCode, ToolError};
use opencrayast_core::hash::{ContentHash, is_full_plan_id};
use opencrayast_core::limits::Limits;
use opencrayast_core::statedir::ensure_state_dir;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The journal store of one workspace.
pub struct JournalStore {
    journal_dir: PathBuf,
    ws_dir: PathBuf,
    workspace_id: String,
    limits: Limits,
    clock: Arc<dyn Clock>,
    journal_identity: DirIdentity,
    ws_identity: DirIdentity,
    /// Serialises create/evict so size accounting stays coherent under concurrency.
    lock: Mutex<()>,
}

impl std::fmt::Debug for JournalStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JournalStore")
            .field("journal_dir", &self.journal_dir)
            .field("workspace_id", &self.workspace_id)
            .finish_non_exhaustive()
    }
}

impl JournalStore {
    /// Open (creating if missing) the journal store of `workspace_id` under `state_dir`.
    /// Directory creation and verification follow `PlanStore::open` exactly: `state_dir` through
    /// `ensure_state_dir`, `ws-<id>/` and `journal/` created `0700` and verified (owner, mode,
    /// not a symlink); an invalid workspace id is `invalid_args` and creates nothing; a directory
    /// that fails verification is `io_error` and is never repaired.
    ///
    /// `limits` supplies `journal_max_plan_mib`, `journal_retention_days`, `journal_max_total_mib`.
    ///
    /// On every later operation the store re-checks that `ws-<id>/` and `journal/` are still the
    /// same directories captured here (not symlinks; same `dev`+`ino`). A replaced directory is
    /// `io_error` and nothing is written elsewhere.
    pub fn open(
        state_dir: &Path,
        workspace_id: &str,
        limits: Limits,
        clock: Arc<dyn Clock>,
    ) -> Result<JournalStore, ToolError> {
        if !is_workspace_id(workspace_id) {
            return Err(ToolError::new(
                ErrorCode::InvalidArgs,
                "workspace_id is not w- plus 32 lowercase hex",
                "Pass a workspace id of the form w-<32 lowercase hex>.",
            ));
        }
        let state = ensure_state_dir(state_dir)?;
        let ws_dir = ensure_state_dir(&state.join(format!("ws-{workspace_id}")))?;
        let journal_dir = ensure_state_dir(&ws_dir.join("journal"))?;
        let ws_identity = capture_dir_identity(&ws_dir)?;
        let journal_identity = capture_dir_identity(&journal_dir)?;
        Ok(JournalStore {
            journal_dir,
            ws_dir,
            workspace_id: workspace_id.to_string(),
            limits,
            clock,
            journal_identity,
            ws_identity,
            lock: Mutex::new(()),
        })
    }

    /// Create the journal of `plan` (apply step 8: "journal(prepare)"), state `Prepared`,
    /// progress 0, `created_at == updated_at == now`. `originals[i]` is the original bytes of
    /// `plan.files[i]`. Returns the manifest.
    ///
    /// ## Decision table (first applicable row)
    ///
    /// | Condition | Result |
    /// |---|---|
    /// | store directories replaced / are symlinks | `io_error` |
    /// | `plan.workspace_id` differs from the store's | `wrong_workspace` |
    /// | `originals.len() != plan.files.len()`, or any original's length differs from `pre_size` or its hash from `pre_hash` | `plan_corrupt` (nothing is created) |
    /// | the originals exceed `journal_max_plan_mib` | `limit_exceeded` |
    /// | a journal for this plan id already exists, in **any** state | `already_applied` (E-9; nothing is touched) |
    /// | the store would exceed `journal_max_total_mib` | first [`JournalStore::evict`]; if it still would: `limit_exceeded` (non-evictable journals are never touched) |
    /// | otherwise | build the whole journal in `.tmp-<random>/` (originals first, then the manifest), fsync every file and the directories, then **rename the directory to `<plan-id>/`**, then fsync `journal/` |
    ///
    /// The rename is the commit point: before it the journal does not exist (a crash leaves only a
    /// `.tmp-` directory, which [`JournalStore::evict`] removes), after it the journal is complete
    /// (E-6: the originals are durable before the first workspace file is touched). Two creates
    /// of one plan racing: exactly one returns `Ok`, the other `already_applied`.
    pub fn create(&self, plan: &Plan, originals: &[Vec<u8>]) -> Result<Manifest, ToolError> {
        self.verify_dirs()?;
        if plan.workspace_id != self.workspace_id {
            return Err(ToolError::new(
                ErrorCode::WrongWorkspace,
                "plan is bound to a different workspace",
                "Refuse to journal a plan from another workspace.",
            ));
        }
        if originals.len() != plan.files.len() {
            return Err(corrupt(
                "originals count does not match the plan",
                "Pass one original byte vector per plan file.",
            ));
        }
        for (i, (o, f)) in originals.iter().zip(plan.files.iter()).enumerate() {
            if o.len() as u64 != f.pre_size {
                return Err(corrupt(
                    format!("original {i} length does not match pre_size"),
                    "Pass the exact pre-image bytes for each file.",
                ));
            }
            if ContentHash::of(o) != f.pre_hash {
                return Err(corrupt(
                    format!("original {i} hash does not match pre_hash"),
                    "Pass the exact pre-image bytes for each file.",
                ));
            }
        }
        let orig_bytes: u64 = originals.iter().map(|o| o.len() as u64).sum();
        let plan_cap = self.limits.journal_max_plan_mib.saturating_mul(1024 * 1024);
        if orig_bytes > plan_cap {
            return Err(ToolError::new(
                ErrorCode::LimitExceeded,
                "journal originals exceed journal_max_plan_mib",
                "Narrow the plan or raise journal_max_plan_mib.",
            ));
        }

        // Why recover from poisoning instead of refusing: every mutation under this lock is
        // fsync'd and published before the guard is released, so the guarded state is already on
        // disk before any panic can leave it half-updated — `into_inner()` re-acquires a mutex whose
        // *guard* died, not one whose data was corrupted. Poisoning here only means some other
        // caller's frame unwound past this lock, so converting it into a permanent
        // `internal: restart the process` for the life of the process is the wrong trade: it turned
        // `recover` into a silent `Ok(0)` — "nothing to recover" when it could not read a single
        // journal — and `undo` into a misleading `journal_missing` (E-17).
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.verify_dirs()?;

        let id = plan.id();
        if self.journal_path(&id).exists() {
            return Err(ToolError::new(
                ErrorCode::AlreadyApplied,
                "a journal for this plan already exists",
                "A plan can be journaled at most once while its journal exists (E-9).",
            ));
        }

        let now = self.clock.now_secs();
        let manifest = Manifest {
            plan_id: id.clone(),
            plan_digest: ContentHash::of(&plan.canonical_bytes()),
            workspace_id: self.workspace_id.clone(),
            state: JournalState::Prepared,
            files: plan
                .files
                .iter()
                .map(|f| JournalFile {
                    path: f.path.clone(),
                    pre_hash: f.pre_hash,
                    post_hash: f.post_hash,
                })
                .collect(),
            progress: 0,
            created_at: now,
            updated_at: now,
        };
        let manifest_bytes = manifest.canonical_bytes();
        let new_size = orig_bytes.saturating_add(manifest_bytes.len() as u64);
        let total_cap = self
            .limits
            .journal_max_total_mib
            .saturating_mul(1024 * 1024);

        // Why: create must free enough for the *incoming* journal, not only when already over.
        //
        // The second argument is the size cap the age pass is held to, and passing the store's
        // total cap here is the whole point of the fix. It used to pass nothing, so every aged
        // journal went on every single `create` — an unrelated apply silently reaped another
        // plan's undo history, and the resulting `plan_not_found` was indistinguishable from
        // "this plan was never applied". Now an aged journal is only removed when the store
        // really needs the room, and retention proper is `evict()` / `opencrayast plan gc`.
        self.evict_locked(new_size, Some(total_cap))?;
        let current = self.total_bytes()?;
        if current.saturating_add(new_size) > total_cap {
            return Err(ToolError::new(
                ErrorCode::LimitExceeded,
                "journal store exceeds journal_max_total_mib",
                "Wait for journals to age out, or raise journal_max_total_mib.",
            ));
        }

        // Build in .tmp-<random>/ : originals first, then manifest (E-6).
        let tmp = make_tmp_dir(&self.journal_dir)?;
        let cleanup = |tmp: &Path| {
            let _ = fs::remove_dir_all(tmp);
        };
        let result = (|| {
            let orig_dir = tmp.join("orig");
            mkdir_private(&orig_dir)?;
            for (i, o) in originals.iter().enumerate() {
                write_exclusive(&orig_dir.join(i.to_string()), o)?;
            }
            write_exclusive(&tmp.join("manifest.json"), &manifest_bytes)?;
            opencrayast_core::fsio::fsync_dir(&orig_dir)?;
            opencrayast_core::fsio::fsync_dir(&tmp)?;
            let dest = self.journal_path(&id);
            match fs::rename(&tmp, &dest) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    return Err(ToolError::new(
                        ErrorCode::AlreadyApplied,
                        "a journal for this plan already exists",
                        "A plan can be journaled at most once while its journal exists (E-9).",
                    ));
                }
                Err(_) => {
                    return Err(ToolError::new(
                        ErrorCode::IoError,
                        "cannot commit the journal directory",
                        "Check the journal store directory permissions.",
                    ));
                }
            }
            opencrayast_core::fsio::fsync_dir(&self.journal_dir)?;
            Ok(manifest)
        })();
        if result.is_err() {
            cleanup(&tmp);
        }
        result
    }

    /// Load and verify a journal. `plan_id` must be a full plan id (`invalid_args` otherwise).
    /// No such journal: `plan_not_found`. The manifest must parse (`Manifest::parse`), carry the
    /// requested plan id and the store's workspace id, and its file must pass the private-file
    /// check; any failure is `plan_corrupt`.
    pub fn load(&self, plan_id: &str) -> Result<Manifest, ToolError> {
        if !is_full_plan_id(plan_id) {
            return Err(ToolError::new(
                ErrorCode::InvalidArgs,
                "journal operations require a full plan id",
                "Pass the full p-<26 base32> plan id.",
            ));
        }
        self.verify_dirs()?;
        self.load_verified(plan_id)
    }

    /// True if a journal directory exists for this full plan id (whatever its state).
    pub fn exists(&self, plan_id: &str) -> Result<bool, ToolError> {
        if !is_full_plan_id(plan_id) {
            return Err(ToolError::new(
                ErrorCode::InvalidArgs,
                "journal operations require a full plan id",
                "Pass the full p-<26 base32> plan id.",
            ));
        }
        self.verify_dirs()?;
        Ok(self.journal_path(plan_id).is_dir())
    }

    /// Move the journal to state `to` and set `progress`, durably (temp file in the same
    /// directory, fsync, rename over `manifest.json`, fsync the directory), and set
    /// `updated_at = now`. Returns the new manifest.
    ///
    /// | Condition | Result |
    /// |---|---|
    /// | not a full id | `invalid_args` |
    /// | store directories replaced | `io_error` |
    /// | no such journal | `plan_not_found` |
    /// | `!current.state.can_become(to)` | `invalid_args` (a caller bug; nothing is written) |
    /// | `progress > files.len()` | `invalid_args` |
    /// | the journal does not verify (see [`JournalStore::load`]) | `plan_corrupt` |
    /// | `plan` does not match the journal: wrong `plan_digest`, or a `files` entry that differs in path, `pre_hash` or `post_hash` | `plan_corrupt`; **nothing is written** (E-16) |
    ///
    /// `plan` is **not** an option: every state transition the shell makes passes the plan it
    /// is acting on, so a manifest whose state field or file hashes were rewritten by corruption or
    /// by hand is refused at the transition that would have trusted them — before the journal can
    /// become terminal. An optional argument would leave that guarantee one `None` away.
    pub fn set_state(
        &self,
        plan_id: &str,
        to: JournalState,
        progress: u64,
        plan: &Plan,
    ) -> Result<Manifest, ToolError> {
        if !is_full_plan_id(plan_id) {
            return Err(ToolError::new(
                ErrorCode::InvalidArgs,
                "journal operations require a full plan id",
                "Pass the full p-<26 base32> plan id.",
            ));
        }
        self.verify_dirs()?;
        let mut m = self.load_verified(plan_id)?;
        m.check_bound_to(plan)?;
        if !m.state.can_become(to) {
            return Err(ToolError::new(
                ErrorCode::InvalidArgs,
                format!(
                    "illegal journal transition {} -> {}",
                    m.state.as_str(),
                    to.as_str()
                ),
                "Follow the journal state machine.",
            ));
        }
        if progress > m.files.len() as u64 {
            return Err(ToolError::new(
                ErrorCode::InvalidArgs,
                "progress exceeds the number of files",
                "Keep progress at most files.len().",
            ));
        }
        m.state = to;
        m.progress = progress;
        m.updated_at = self.clock.now_secs();
        self.write_manifest(plan_id, &m)?;
        Ok(m)
    }

    /// Update only `progress` (state unchanged), same durability. Allowed only in `Writing` and
    /// `Undoing`, and only to a value `>=` the current one and `<= files.len()`; otherwise
    /// `invalid_args`. `plan_canonical_bytes` is verified against the journal's `plan_digest`
    /// exactly as in [`JournalStore::set_state`]: `progress` is informational, so the check is not
    /// about the value but about not writing over a manifest that has been rewritten.
    pub fn set_progress(
        &self,
        plan_id: &str,
        progress: u64,
        plan: &Plan,
    ) -> Result<Manifest, ToolError> {
        if !is_full_plan_id(plan_id) {
            return Err(ToolError::new(
                ErrorCode::InvalidArgs,
                "journal operations require a full plan id",
                "Pass the full p-<26 base32> plan id.",
            ));
        }
        self.verify_dirs()?;
        let mut m = self.load_verified(plan_id)?;
        m.check_bound_to(plan)?;
        if !matches!(m.state, JournalState::Writing | JournalState::Undoing) {
            return Err(ToolError::new(
                ErrorCode::InvalidArgs,
                format!(
                    "set_progress requires writing or undoing, got {}",
                    m.state.as_str()
                ),
                "Advance progress only while applying or undoing.",
            ));
        }
        if progress < m.progress || progress > m.files.len() as u64 {
            return Err(ToolError::new(
                ErrorCode::InvalidArgs,
                "progress must be monotonic and at most files.len()",
                "Pass a progress value >= the current one and <= files.len().",
            ));
        }
        m.progress = progress;
        m.updated_at = self.clock.now_secs();
        self.write_manifest(plan_id, &m)?;
        Ok(m)
    }

    /// The original bytes of file `index`. `index >= files.len()` is `invalid_args`. The file must
    /// pass the private-file check and **hash to the manifest's `pre_hash`**; a truncated or
    /// altered original is `plan_corrupt` and is never returned (EDT-25).
    pub fn read_original(&self, plan_id: &str, index: usize) -> Result<Vec<u8>, ToolError> {
        if !is_full_plan_id(plan_id) {
            return Err(ToolError::new(
                ErrorCode::InvalidArgs,
                "journal operations require a full plan id",
                "Pass the full p-<26 base32> plan id.",
            ));
        }
        self.verify_dirs()?;
        let m = self.load_verified(plan_id)?;
        if index >= m.files.len() {
            return Err(ToolError::new(
                ErrorCode::InvalidArgs,
                "original index is out of range",
                "Pass an index less than the number of journal files.",
            ));
        }
        let path = self
            .journal_path(plan_id)
            .join("orig")
            .join(index.to_string());
        verify_private_file(&path).map_err(|e| {
            if e.code == ErrorCode::IoError {
                // Missing / unreadable original still surfaces as plan_corrupt for EDT-25.
                corrupt(
                    "original file is missing or unreadable",
                    "Refuse the original; re-apply from a fresh plan.",
                )
            } else {
                e
            }
        })?;
        let bytes = fs::read(&path).map_err(|_| {
            corrupt(
                "original file is missing or unreadable",
                "Refuse the original; re-apply from a fresh plan.",
            )
        })?;
        if ContentHash::of(&bytes) != m.files[index].pre_hash {
            return Err(corrupt(
                "original file does not match its recorded pre_hash",
                "Refuse the original; it was truncated or altered (EDT-25).",
            ));
        }
        Ok(bytes)
    }

    /// Every journal, ascending by plan id, plus the ids of directories that exist but do not
    /// verify (listing never fails because of one bad journal). `.tmp-` directories are ignored.
    pub fn list(&self) -> Result<(Vec<Manifest>, Vec<String>), ToolError> {
        self.verify_dirs()?;
        let mut good = Vec::new();
        let mut bad = Vec::new();
        for id in self.scan_plan_ids()? {
            match self.load_verified(&id) {
                Ok(m) => good.push(m),
                Err(_) => bad.push(id),
            }
        }
        good.sort_by(|a, b| a.plan_id.cmp(&b.plan_id));
        bad.sort();
        Ok((good, bad))
    }

    /// The journals that recovery must look at: state `Prepared`, `Writing` or `Undoing`,
    /// ascending by plan id. Unverifiable journals are returned as an error `plan_corrupt` naming
    /// their ids (recovery must not silently skip a journal it cannot read).
    pub fn nonterminal(&self) -> Result<Vec<Manifest>, ToolError> {
        self.verify_dirs()?;
        let mut out = Vec::new();
        let mut bad = Vec::new();
        for id in self.scan_plan_ids()? {
            match self.load_verified(&id) {
                Ok(m) => {
                    if matches!(
                        m.state,
                        JournalState::Prepared | JournalState::Writing | JournalState::Undoing
                    ) {
                        out.push(m);
                    }
                }
                Err(_) => bad.push(id),
            }
        }
        if !bad.is_empty() {
            bad.sort();
            return Err(corrupt(
                format!("unverifiable nonterminal journals: {}", bad.join(", ")),
                "Repair or remove the named journal directories before recovery.",
            ));
        }
        out.sort_by(|a, b| a.plan_id.cmp(&b.plan_id));
        Ok(out)
    }

    /// # What each pass deletes
    ///
    /// - **Age pass** — every *evictable* journal with `now - updated_at >= journal_retention_days
    ///   * 86400`. Bounded by the passed cap.
    /// - **Size pass** — while the total exceeds `journal_max_total_mib`, the evictable journal
    ///   with the oldest `updated_at` (ties: smaller plan id first). Never evicts anything not
    ///   already past retention, so it can only ever finish what the age pass started.
    ///
    /// # Why both passes are bounded by one cap
    ///
    /// They used to be separate: the age pass deleted every aged journal however small the store
    /// was, and the size pass kept going until the total fit. That is the right policy for a
    /// maintenance run and **the wrong policy for a call that just wants room**.
    ///
    /// `create` used to reach this unconditionally. So: apply a plan, wait past
    /// `journal_retention_days`, then apply an **unrelated** plan, and the first plan's journal —
    /// the only copy of the originals that makes its edit undoable — was destroyed as a side
    /// effect of work that had nothing to do with it. The refusal the operator then got was a bare
    /// `plan_not_found`, which cannot distinguish "never applied" from "silently reaped". An
    /// expiry is a statement about *this* journal's age; it must not be triggered by an
    /// unrelated request.
    ///
    /// So [`create`] passes the incoming size and never deletes an aged journal unless the store
    /// genuinely needs the room, and this function's public entry point passes **no cap at all**:
    /// retention then applies to everything at or past it, which is the documented meaning of the
    /// setting. Retention is real, and it is reached by `opencrayast plan gc`.
    pub fn evict(&self) -> Result<Vec<String>, ToolError> {
        self.verify_dirs()?;
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        // `None` = no size bound: this is the maintenance pass, so every aged journal goes
        // whatever the total is.
        self.evict_locked(0, None)
    }

    fn verify_dirs(&self) -> Result<(), ToolError> {
        check_dir_identity(&self.ws_dir, self.ws_identity)?;
        check_dir_identity(&self.journal_dir, self.journal_identity)?;
        Ok(())
    }

    fn journal_path(&self, plan_id: &str) -> PathBuf {
        self.journal_dir.join(plan_id)
    }

    fn load_verified(&self, plan_id: &str) -> Result<Manifest, ToolError> {
        let dir = self.journal_path(plan_id);
        if !dir.is_dir() {
            return Err(ToolError::new(
                ErrorCode::PlanNotFound,
                "no such journal",
                "Apply the plan first, or it was already evicted.",
            ));
        }
        let path = dir.join("manifest.json");
        verify_private_file(&path)?;
        let bytes = fs::read(&path).map_err(|_| {
            corrupt(
                "cannot read the journal manifest",
                "Check the journal store directory permissions.",
            )
        })?;
        let m = Manifest::parse(&bytes)?;
        if m.plan_id != plan_id {
            return Err(corrupt(
                "manifest plan_id does not match the journal directory",
                "Refuse the journal; it was tampered with.",
            ));
        }
        if m.workspace_id != self.workspace_id {
            return Err(corrupt(
                "manifest workspace_id does not match the store",
                "Refuse the journal; it belongs to another workspace.",
            ));
        }
        Ok(m)
    }

    fn write_manifest(&self, plan_id: &str, m: &Manifest) -> Result<(), ToolError> {
        let path = self.journal_path(plan_id).join("manifest.json");
        write_exclusive(&path, &m.canonical_bytes())
    }

    fn scan_plan_ids(&self) -> Result<Vec<String>, ToolError> {
        let mut ids = Vec::new();
        let entries = fs::read_dir(&self.journal_dir).map_err(|_| {
            ToolError::new(
                ErrorCode::IoError,
                "cannot list the journal store",
                "Check the journal store directory permissions.",
            )
        })?;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.starts_with(".tmp-") {
                continue;
            }
            if is_full_plan_id(name) && entry.path().is_dir() {
                ids.push(name.to_string());
            }
        }
        ids.sort();
        Ok(ids)
    }

    fn total_bytes(&self) -> Result<u64, ToolError> {
        let mut total = 0u64;
        for id in self.scan_plan_ids()? {
            total = total.saturating_add(dir_size(&self.journal_path(&id))?);
        }
        Ok(total)
    }

    /// `reserve` = bytes the caller still needs to write (create passes the new journal size).
    /// `max_age_pass` = the cap the **age** pass is held to, or `None` for "no cap" (the public
    /// `evict`, i.e. a maintenance run). The size pass always uses the configured cap.
    fn evict_locked(
        &self,
        reserve: u64,
        max_age_pass: Option<u64>,
    ) -> Result<Vec<String>, ToolError> {
        self.verify_dirs()?;
        // Leftover temp directories from crashed create/delete.
        if let Ok(entries) = fs::read_dir(&self.journal_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name) = name.to_str() else {
                    continue;
                };
                if name.starts_with(".tmp-") {
                    let _ = fs::remove_dir_all(entry.path());
                }
            }
        }

        let now = self.clock.now_secs();
        let retention = self.limits.journal_retention_days.saturating_mul(86_400);
        let cap = self
            .limits
            .journal_max_total_mib
            .saturating_mul(1024 * 1024);
        let mut evicted = Vec::new();

        // Age pass: every evictable journal at or past retention — but ONLY while the store is
        // over the caller's cap. `max_age_pass` is the size the store must get under; `None`
        // (public `evict`) means retention applies unconditionally.
        //
        // Why oldest-first and not id-order: the journals most likely to be past their useful
        // life are the oldest, and when the cap bites we want to free the least valuable bytes
        // first. `updated_at` is the whole tie-break, so the deletion order is total and does not
        // depend on the directory's read order.
        let age_limit = max_age_pass.unwrap_or(0);
        let mut aged: Vec<(u64, String)> = Vec::new();
        for id in self.scan_plan_ids()? {
            let Ok(m) = self.load_verified(&id) else {
                continue;
            };
            if !is_evictable(m.state) {
                continue;
            }
            if now.saturating_sub(m.updated_at) >= retention {
                aged.push((m.updated_at, id));
            }
        }
        aged.sort();
        let mut total = self.total_bytes()?;
        for (_, id) in &aged {
            if total <= age_limit {
                break;
            }
            let size = dir_size(&self.journal_path(id))?;
            self.delete_journal(id)?;
            total = total.saturating_sub(size);
            evicted.push(id.clone());
        }

        // Size-based: while current + reserve exceeds the configured cap, delete oldest evictable
        // (ties: smaller plan id). Why reserve: create must make room for the incoming journal.
        //
        // Restarts from a fresh measurement rather than from the age pass's running `total`: a
        // delete is crash-safe but not atomic across two passes, and one wrong number here deletes
        // a journal that did not need to go. The scan is cheap next to the `dir_size` calls that
        // the first pass already made.
        loop {
            if self.total_bytes()?.saturating_add(reserve) <= cap {
                break;
            }
            let mut candidates: Vec<(u64, String)> = Vec::new();
            for id in self.scan_plan_ids()? {
                let Ok(m) = self.load_verified(&id) else {
                    continue;
                };
                if is_evictable(m.state) {
                    candidates.push((m.updated_at, id));
                }
            }
            if candidates.is_empty() {
                break;
            }
            candidates.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
            let id = candidates[0].1.clone();
            self.delete_journal(&id)?;
            evicted.push(id);
        }

        evicted.sort();
        evicted.dedup();
        Ok(evicted)
    }

    /// Crash-safe delete: rename to `.tmp-<random>` then remove_dir_all.
    fn delete_journal(&self, plan_id: &str) -> Result<(), ToolError> {
        let src = self.journal_path(plan_id);
        if !src.exists() {
            return Ok(());
        }
        let tmp = make_tmp_dir(&self.journal_dir)?;
        // make_tmp_dir created an empty dir; remove it and use that name for the rename target.
        let _ = fs::remove_dir(&tmp);
        fs::rename(&src, &tmp).map_err(|_| {
            ToolError::new(
                ErrorCode::IoError,
                "cannot rename a journal for deletion",
                "Check the journal store directory permissions.",
            )
        })?;
        let _ = fs::remove_dir_all(&tmp);
        let _ = opencrayast_core::fsio::fsync_dir(&self.journal_dir);
        Ok(())
    }
}

fn is_evictable(state: JournalState) -> bool {
    matches!(
        state,
        JournalState::Applied | JournalState::RolledBack | JournalState::Undone
    )
}

fn corrupt(message: impl Into<String>, next: impl Into<String>) -> ToolError {
    ToolError::new(ErrorCode::PlanCorrupt, message, next)
}

fn dir_size(path: &Path) -> Result<u64, ToolError> {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let p = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(p);
            } else if meta.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    Ok(total)
}
