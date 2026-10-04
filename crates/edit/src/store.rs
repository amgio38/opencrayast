//! The plan store (docs/EDIT-MODEL.md "Plan store"; invariants E-2, E-15; tests EDT-06, EDT-16,
//! EDT-26, EDT-27). Plans live in `<state_dir>/ws-<workspace id>/plans/` as
//! `<plan-id>.json` (the canonical plan bytes, mode `0600`) next to `<plan-id>.meta.json` (the
//! unhashed envelope: creation time, expiry, producing binary version).
//!
//! The store is the only place that decides which plans exist. Its two promises:
//! 1. **A reviewed plan cannot be pushed out or replaced.** Nothing but an *expired or
//!    unverifiable, not in use* plan is ever deleted to make room; a full store of still-valid
//!    plans refuses new previews instead (E-15). Unverifiable means the envelope or plan bytes
//!    no longer pass verification — such an entry can never be applied, so it must not starve
//!    the store.
//! 2. **What comes out is what went in.** Every read re-verifies the plan bytes against the
//!    plan id (E-2) and the file against the mode/owner rules below; a store directory is
//!    attacker-reachable state, never trusted.

use crate::fsutil::{
    DirIdentity, FILE_TEMP_PREFIX, capture_dir_identity, check_dir_identity, is_workspace_id,
    verify_private_file, write_exclusive,
};
use crate::plan::Plan;
use opencrayast_core::error::{ErrorCode, ToolError};
use opencrayast_core::hash::{is_full_plan_id, resolve_plan_prefix};
use opencrayast_core::limits::Limits;
use opencrayast_core::statedir::ensure_state_dir;
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Seconds since the Unix epoch, injected so expiry is testable (EDT-06).
pub trait Clock: Send + Sync {
    /// The current time in whole seconds since the Unix epoch.
    fn now_secs(&self) -> u64;
}

/// The real clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_secs(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

/// The unhashed envelope of a stored plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanMeta {
    /// Creation time (clock seconds).
    pub created_at: u64,
    /// Expiry time (clock seconds); the plan is expired when `now >= expires_at`.
    pub expires_at: u64,
    /// `CARGO_PKG_VERSION` of the producing binary (informational, never trusted).
    pub producer_version: String,
}

/// One line of [`PlanStore::list`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanSummary {
    /// The full plan id.
    pub id: String,
    /// Its envelope.
    pub meta: PlanMeta,
    /// Number of files in the plan.
    pub files: usize,
    /// Total number of edits.
    pub edits: usize,
}

/// Shared mutable state for one [`PlanStore`]: in-use counts and this value's per-process puts.
/// Held across put make-room and begin_use so an in-use plan cannot be swept under a racer.
struct Shared {
    in_use: HashMap<String, usize>,
    /// `id → expires_at` for unexpired puts performed by this `PlanStore` value.
    process_puts: HashMap<String, u64>,
}

/// A plan marked *in use* (an apply, undo or recovery has opened it). While any guard for an
/// id exists, [`PlanStore::sweep`] and the make-room logic of [`PlanStore::put`] never delete
/// that plan, expired or not. Dropping the guard releases it.
pub struct UseGuard {
    shared: Arc<Mutex<Shared>>,
    id: String,
    plan: Plan,
}

impl fmt::Debug for UseGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UseGuard")
            .field("id", &self.id)
            .field("plan", &self.plan)
            .finish_non_exhaustive()
    }
}

impl UseGuard {
    /// The plan as it was when the guard was taken (already verified).
    pub fn plan(&self) -> &Plan {
        &self.plan
    }
    /// Its id.
    pub fn id(&self) -> &str {
        &self.id
    }
}

impl Drop for UseGuard {
    fn drop(&mut self) {
        let Ok(mut g) = self.shared.lock() else {
            return;
        };
        if let Some(count) = g.in_use.get_mut(&self.id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                g.in_use.remove(&self.id);
            }
        }
    }
}

/// The plan store of one workspace.
pub struct PlanStore {
    plans_dir: PathBuf,
    /// Parent of `plans_dir` (`…/ws-<id>/`), also pinned by identity.
    ws_dir: PathBuf,
    workspace_id: String,
    limits: Limits,
    clock: Arc<dyn Clock>,
    shared: Arc<Mutex<Shared>>,
    plans_identity: DirIdentity,
    ws_identity: DirIdentity,
}

impl fmt::Debug for PlanStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlanStore")
            .field("plans_dir", &self.plans_dir)
            .field("workspace_id", &self.workspace_id)
            .finish_non_exhaustive()
    }
}

impl PlanStore {
    /// Open (creating if missing) the store of `workspace_id` under `state_dir`.
    ///
    /// - `state_dir` is verified with `opencrayast_core::statedir::ensure_state_dir`;
    ///   `ws-<id>/` and `plans/` are created `0700` and verified the same way (owned by the
    ///   current user, no group/other bits, not symlinks); a directory that fails is refused with
    ///   `io_error`, never repaired.
    /// - `workspace_id` must be `w-` + 32 lowercase hex, else `invalid_args` (it becomes a path
    ///   component).
    /// - `limits` supplies `plan_ttl_minutes`, `plan_max_plans`, `plan_max_store_mib`,
    ///   `plan_max_plans_per_process` and the plan limits passed on to [`Plan::check`].
    ///
    /// The per-process quota counts the **unexpired plans this `PlanStore` value (one per server
    /// process) has put**, not the plans already on disk.
    ///
    /// On every later operation the store re-checks that `ws-<id>/` and `plans/` are still the
    /// same directories captured here (not symlinks; same `dev`+`ino`). A replaced directory is
    /// `io_error` and nothing is written elsewhere.
    pub fn open(
        state_dir: &Path,
        workspace_id: &str,
        limits: Limits,
        clock: Arc<dyn Clock>,
    ) -> Result<PlanStore, ToolError> {
        // Validate before creating anything (invalid id must leave state_dir untouched).
        if !is_workspace_id(workspace_id) {
            return Err(ToolError::new(
                ErrorCode::InvalidArgs,
                "workspace_id is not w- plus 32 lowercase hex",
                "Pass a workspace id of the form w-<32 lowercase hex>.",
            ));
        }
        let state = ensure_state_dir(state_dir)?;
        let ws_dir = ensure_state_dir(&state.join(format!("ws-{workspace_id}")))?;
        let plans_dir = ensure_state_dir(&ws_dir.join("plans"))?;
        let ws_identity = capture_dir_identity(&ws_dir)?;
        let plans_identity = capture_dir_identity(&plans_dir)?;
        Ok(PlanStore {
            plans_dir,
            ws_dir,
            workspace_id: workspace_id.to_string(),
            limits,
            clock,
            shared: Arc::new(Mutex::new(Shared {
                in_use: HashMap::new(),
                process_puts: HashMap::new(),
            })),
            plans_identity,
            ws_identity,
        })
    }

    /// Store `plan`; returns its id and envelope.
    ///
    /// ## Decision table (first applicable row, in order)
    ///
    /// | Condition | Result |
    /// |---|---|
    /// | `ws-<id>/` or `plans/` is a symlink, not a directory, or its `dev`+`ino` changed since [`PlanStore::open`] | `io_error` (nothing is written) |
    /// | `plan.workspace_id` differs from the store's | `wrong_workspace` |
    /// | `Plan::check(limits)` fails | its error |
    /// | a plan with this id is stored, unexpired | `Ok`, idempotent: nothing is rewritten, the existing envelope is returned (the TTL is **not** extended) |
    /// | a plan with this id is stored but expired | it is replaced by a fresh copy with a new envelope (same bytes, same id) unless in use, in which case `busy` |
    /// | the per-process quota of unexpired puts is used up | `limit_exceeded` |
    /// | the store holds `plan_max_plans` plans, or adding this one would exceed `plan_max_store_mib` | first delete plans that are **not in use** and either **expired** or **unverifiable** (meta unparsable, plan bytes fail [`Plan::parse_named`]); if that frees enough: `Ok`; otherwise `limit_exceeded` (nothing unexpired-and-valid or in use is touched) |
    /// | otherwise | write, then `Ok` |
    ///
    /// Writing is crash-safe: temp file (exclusive create, `0600`) in the same directory, fsync,
    /// rename, fsync of the directory; the plan file is renamed into place **before** its meta
    /// file, and a plan file without a meta file is treated as absent by every reader (and is
    /// removed by [`PlanStore::sweep`]). A file already present under the id with different bytes
    /// is `plan_corrupt` and is left alone.
    pub fn put(&self, plan: &Plan) -> Result<(String, PlanMeta), ToolError> {
        self.verify_store_dirs()?;
        if plan.workspace_id != self.workspace_id {
            return Err(ToolError::new(
                ErrorCode::WrongWorkspace,
                "plan is bound to a different workspace",
                "Refuse to store a plan from another workspace (E-11).",
            ));
        }
        plan.check(&self.limits)?;
        let id = plan.id();
        let bytes = plan.canonical_bytes();
        let now = self.clock.now_secs();
        let ttl_secs = self.limits.plan_ttl_minutes.saturating_mul(60);
        let fresh_meta = PlanMeta {
            created_at: now,
            expires_at: now.saturating_add(ttl_secs),
            producer_version: env!("CARGO_PKG_VERSION").to_string(),
        };
        let meta_bytes = encode_meta(&fresh_meta);

        // E-17: recover from poisoning instead of refusing. Everything under this lock is
        // in-memory bookkeeping written back to disk only after the operation completes, and
        // `in_use` is decremented in `Drop`, so a panic elsewhere leaves the map consistent
        // enough to keep using; a permanent `internal` here would make `apply` and `undo` fail
        // for the rest of the process over a mutex whose data was never mid-update.
        let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        prune_process_puts(&mut shared, now);

        let plan_path = self.plan_path(&id);
        let meta_path = self.meta_path(&id);

        // Existing plan file with different bytes → corrupt, leave alone (even without meta).
        if plan_path.exists() {
            match fs::read(&plan_path) {
                Ok(existing) if existing != bytes => {
                    return Err(ToolError::new(
                        ErrorCode::PlanCorrupt,
                        "a different plan is already stored under this id",
                        "Refuse the write; the store entry was altered or collided (E-2).",
                    ));
                }
                Err(_) => {
                    return Err(ToolError::new(
                        ErrorCode::IoError,
                        "cannot read the existing plan file",
                        "Check the plan store directory permissions.",
                    ));
                }
                Ok(_) => {}
            }
        }

        // Idempotent / replace-expired path when a complete entry exists.
        if meta_path.exists() && plan_path.exists() {
            let existing_meta = load_meta_file(&meta_path)?;
            if now < existing_meta.expires_at {
                return Ok((id, existing_meta));
            }
            if shared.in_use.get(&id).copied().unwrap_or(0) > 0 {
                return Err(ToolError::new(
                    ErrorCode::Busy,
                    "expired plan is in use and cannot be replaced",
                    "Wait for the apply/undo to finish, then retry the preview.",
                ));
            }
            self.write_plan_then_meta(&plan_path, &bytes, &meta_path, &meta_bytes)?;
            shared
                .process_puts
                .insert(id.clone(), fresh_meta.expires_at);
            return Ok((id, fresh_meta));
        }

        // New id (or completing an orphan plan file): process quota, then make room.
        if !shared.process_puts.contains_key(&id)
            && (shared.process_puts.len() as u64) >= self.limits.plan_max_plans_per_process
        {
            return Err(ToolError::new(
                ErrorCode::LimitExceeded,
                "per-process unexpired plan quota is full",
                "Wait for plans to expire, or raise plan_max_plans_per_process.",
            ));
        }

        let new_bytes = (bytes.len() as u64).saturating_add(meta_bytes.len() as u64);
        self.make_room_locked(&mut shared, now, new_bytes)?;

        if !self.has_room_for_new(&id, new_bytes)? {
            return Err(ToolError::new(
                ErrorCode::LimitExceeded,
                "plan store is full",
                "Wait for expired plans to be swept, or raise plan_max_plans / plan_max_store_mib.",
            ));
        }

        self.write_plan_then_meta(&plan_path, &bytes, &meta_path, &meta_bytes)?;
        shared
            .process_puts
            .insert(id.clone(), fresh_meta.expires_at);
        Ok((id, fresh_meta))
    }

    /// Load for a **write-path** operation (apply, undo, recover): `plan_id` must be a full id
    /// (`is_full_plan_id`), otherwise `invalid_args` (E-15, EDT-26).
    ///
    /// | Condition | Result |
    /// |---|---|
    /// | not a full id | `invalid_args` |
    /// | store directories replaced / are symlinks | `io_error` |
    /// | no such plan (or meta missing) | `plan_not_found` |
    /// | expired | `plan_expired` |
    /// | file is a symlink, not a regular file, has group/other permission bits, or is not owned by the current user (unix) | `plan_corrupt` |
    /// | bytes do not parse, or hash to another id (`Plan::parse_named`) | `plan_corrupt` |
    /// | meta file unparsable, or `expires_at < created_at` | `plan_corrupt` |
    /// | plan bound to another workspace | `wrong_workspace` |
    pub fn get_for_write(&self, plan_id: &str) -> Result<(Plan, PlanMeta), ToolError> {
        if !is_full_plan_id(plan_id) {
            return Err(ToolError::new(
                ErrorCode::InvalidArgs,
                "write paths require a full plan id",
                "Pass the full p-<26 base32> plan id (E-15).",
            ));
        }
        self.verify_store_dirs()?;
        self.load_verified(plan_id, false)
    }

    /// Load for **read-only** inspection: like [`PlanStore::get_for_write`] but accepts an
    /// unambiguous prefix of at least 10 characters (`resolve_plan_prefix`). An ambiguous or
    /// unknown prefix is `plan_not_found`.
    pub fn get_for_read(&self, plan_id_or_prefix: &str) -> Result<(Plan, PlanMeta), ToolError> {
        self.verify_store_dirs()?;
        let id = if is_full_plan_id(plan_id_or_prefix) {
            plan_id_or_prefix.to_string()
        } else {
            let known = self.list_present_ids()?;
            match resolve_plan_prefix(plan_id_or_prefix, &known) {
                Some(full) => full.to_string(),
                None => {
                    return Err(ToolError::new(
                        ErrorCode::PlanNotFound,
                        "no plan matches this id or prefix",
                        "Use a full plan id or a longer unambiguous prefix.",
                    ));
                }
            }
        };
        self.load_verified(&id, false)
    }

    /// Mark a plan in use (see [`UseGuard`]). Same checks as [`PlanStore::get_for_write`]
    /// (full id; not expired at this moment). Many guards for one id may coexist.
    pub fn begin_use(&self, plan_id: &str) -> Result<UseGuard, ToolError> {
        if !is_full_plan_id(plan_id) {
            return Err(ToolError::new(
                ErrorCode::InvalidArgs,
                "write paths require a full plan id",
                "Pass the full p-<26 base32> plan id (E-15).",
            ));
        }
        self.verify_store_dirs()?;
        let (plan, _meta) = self.load_verified(plan_id, false)?;
        // E-17: recover from poisoning instead of refusing. Everything under this lock is
        // in-memory bookkeeping written back to disk only after the operation completes, and
        // `in_use` is decremented in `Drop`, so a panic elsewhere leaves the map consistent
        // enough to keep using; a permanent `internal` here would make `apply` and `undo` fail
        // for the rest of the process over a mutex whose data was never mid-update.
        let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        *shared.in_use.entry(plan_id.to_string()).or_insert(0) += 1;
        Ok(UseGuard {
            shared: Arc::clone(&self.shared),
            id: plan_id.to_string(),
            plan,
        })
    }

    /// The unexpired, readable plans, ascending by id. A plan that fails verification is skipped
    /// here (listing never fails because of one bad file) but is reported in the second element
    /// as its id so a doctor command can say so.
    pub fn list(&self) -> Result<(Vec<PlanSummary>, Vec<String>), ToolError> {
        self.verify_store_dirs()?;
        let now = self.clock.now_secs();
        let mut good = Vec::new();
        let mut bad = Vec::new();
        for id in self.list_present_ids()? {
            match self.load_verified(&id, true) {
                Ok((plan, meta)) => {
                    if now >= meta.expires_at {
                        continue;
                    }
                    let edits = plan.files.iter().map(|f| f.edits.len()).sum();
                    good.push(PlanSummary {
                        id,
                        meta,
                        files: plan.files.len(),
                        edits,
                    });
                }
                Err(e) if e.code == ErrorCode::PlanExpired => {}
                Err(e) if e.code == ErrorCode::PlanNotFound => {}
                Err(e) if e.code == ErrorCode::WrongWorkspace => bad.push(id),
                Err(_) => bad.push(id),
            }
        }
        good.sort_by(|a, b| a.id.cmp(&b.id));
        bad.sort();
        Ok((good, bad))
    }

    /// Delete reclaimable plans and orphans.
    ///
    /// Reclaimable = **not in use** and either **expired** or **unverifiable** (envelope
    /// unparsable, or plan bytes fail [`Plan::parse_named`]). Also removes orphans (a plan file
    /// without meta, a meta without a plan file, leftover temp files). Returns how many complete
    /// plan entries were deleted. Never deletes anything that is still valid-and-unexpired, or
    /// anything in use.
    pub fn sweep(&self) -> Result<usize, ToolError> {
        self.verify_store_dirs()?;
        let now = self.clock.now_secs();
        // E-17: recover from poisoning instead of refusing. Everything under this lock is
        // in-memory bookkeeping written back to disk only after the operation completes, and
        // `in_use` is decremented in `Drop`, so a panic elsewhere leaves the map consistent
        // enough to keep using; a permanent `internal` here would make `apply` and `undo` fail
        // for the rest of the process over a mutex whose data was never mid-update.
        let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        self.sweep_locked(&mut shared, now)
    }

    /// Refuse if `ws-<id>/` or `plans/` was replaced since open (symlink or different inode).
    fn verify_store_dirs(&self) -> Result<(), ToolError> {
        check_dir_identity(&self.ws_dir, self.ws_identity)?;
        check_dir_identity(&self.plans_dir, self.plans_identity)?;
        Ok(())
    }

    fn plan_path(&self, id: &str) -> PathBuf {
        self.plans_dir.join(format!("{id}.json"))
    }

    fn meta_path(&self, id: &str) -> PathBuf {
        self.plans_dir.join(format!("{id}.meta.json"))
    }

    fn load_verified(&self, id: &str, allow_expired: bool) -> Result<(Plan, PlanMeta), ToolError> {
        if !is_full_plan_id(id) {
            return Err(ToolError::new(
                ErrorCode::InvalidArgs,
                "write paths require a full plan id",
                "Pass the full p-<26 base32> plan id (E-15).",
            ));
        }
        let plan_path = self.plan_path(id);
        let meta_path = self.meta_path(id);
        if !meta_path.exists() || !plan_path.exists() {
            return Err(ToolError::new(
                ErrorCode::PlanNotFound,
                "no such plan",
                "Preview again to create a plan.",
            ));
        }
        verify_private_file(&plan_path)?;
        verify_private_file(&meta_path)?;
        let meta = load_meta_file(&meta_path)?;
        let now = self.clock.now_secs();
        if !allow_expired && now >= meta.expires_at {
            return Err(ToolError::new(
                ErrorCode::PlanExpired,
                "plan has expired",
                "Preview again; expired plans cannot be applied (EDT-06).",
            ));
        }
        let bytes = fs::read(&plan_path).map_err(|_| {
            ToolError::new(
                ErrorCode::IoError,
                "cannot read the plan file",
                "Check the plan store directory permissions.",
            )
        })?;
        let plan = Plan::parse_named(id, &bytes, &self.limits).map_err(|e| {
            if e.code == ErrorCode::PlanCorrupt {
                e
            } else {
                ToolError::new(
                    ErrorCode::PlanCorrupt,
                    "stored plan bytes failed verification",
                    "Refuse the plan; re-preview (E-2).",
                )
            }
        })?;
        if plan.workspace_id != self.workspace_id {
            return Err(ToolError::new(
                ErrorCode::WrongWorkspace,
                "plan is bound to a different workspace",
                "Refuse to use a plan from another workspace (E-11).",
            ));
        }
        Ok((plan, meta))
    }

    fn list_present_ids(&self) -> Result<Vec<String>, ToolError> {
        let mut ids = Vec::new();
        let entries = fs::read_dir(&self.plans_dir).map_err(|_| {
            ToolError::new(
                ErrorCode::IoError,
                "cannot list the plan store",
                "Check the plan store directory permissions.",
            )
        })?;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(id) = name.strip_suffix(".meta.json") else {
                continue;
            };
            if !is_full_plan_id(id) {
                continue;
            }
            if self.plan_path(id).exists() {
                ids.push(id.to_string());
            }
        }
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    fn store_byte_size(&self) -> Result<u64, ToolError> {
        let mut total = 0u64;
        let entries = fs::read_dir(&self.plans_dir).map_err(|_| {
            ToolError::new(
                ErrorCode::IoError,
                "cannot list the plan store",
                "Check the plan store directory permissions.",
            )
        })?;
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata()
                && meta.is_file()
            {
                total = total.saturating_add(meta.len());
            }
        }
        Ok(total)
    }

    fn present_count(&self) -> Result<u64, ToolError> {
        Ok(self.list_present_ids()?.len() as u64)
    }

    /// Whether the store can accept a new plan `new_id` of `new_bytes` (plan+meta).
    fn has_room_for_new(&self, new_id: &str, new_bytes: u64) -> Result<bool, ToolError> {
        let ids = self.list_present_ids()?;
        let replacing = ids.iter().any(|i| i == new_id);
        let count = if replacing {
            ids.len() as u64
        } else {
            (ids.len() as u64).saturating_add(1)
        };
        if count > self.limits.plan_max_plans {
            return Ok(false);
        }
        let cap = self.limits.plan_max_store_mib.saturating_mul(1024 * 1024);
        let current = self.store_byte_size()?;
        if replacing {
            Ok(current <= cap)
        } else {
            Ok(current.saturating_add(new_bytes) <= cap)
        }
    }

    /// Delete every reclaimable (expired or unverifiable, not in-use) plan when the store
    /// needs room. Why: corrupt entries must not starve the store forever.
    fn make_room_locked(
        &self,
        shared: &mut Shared,
        now: u64,
        new_bytes: u64,
    ) -> Result<(), ToolError> {
        let count = self.present_count()?;
        let cap = self.limits.plan_max_store_mib.saturating_mul(1024 * 1024);
        let size = self.store_byte_size()?;
        let need_slot = count >= self.limits.plan_max_plans;
        let need_bytes = size.saturating_add(new_bytes) > cap;
        if !need_slot && !need_bytes {
            return Ok(());
        }
        for id in self.list_present_ids()? {
            if !self.entry_is_reclaimable(shared, &id, now) {
                continue;
            }
            self.delete_entry(shared, &id);
        }
        Ok(())
    }

    fn sweep_locked(&self, shared: &mut Shared, now: u64) -> Result<usize, ToolError> {
        let mut deleted = 0usize;
        for id in self.list_present_ids()? {
            if !self.entry_is_reclaimable(shared, &id, now) {
                continue;
            }
            self.delete_entry(shared, &id);
            deleted += 1;
        }
        if let Ok(entries) = fs::read_dir(&self.plans_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name) = name.to_str() else {
                    continue;
                };
                let path = entry.path();
                if name.starts_with(FILE_TEMP_PREFIX) {
                    let _ = fs::remove_file(&path);
                    continue;
                }
                if let Some(id) = name.strip_suffix(".meta.json") {
                    if is_full_plan_id(id) && !self.plan_path(id).exists() {
                        let _ = fs::remove_file(&path);
                    }
                    continue;
                }
                if let Some(id) = name.strip_suffix(".json") {
                    if name.ends_with(".meta.json") {
                        continue;
                    }
                    if is_full_plan_id(id) && !self.meta_path(id).exists() {
                        let _ = fs::remove_file(&path);
                    }
                }
            }
        }
        Ok(deleted)
    }

    /// Not in use, and either expired or unverifiable (meta/plan fail).
    fn entry_is_reclaimable(&self, shared: &Shared, id: &str, now: u64) -> bool {
        if shared.in_use.get(id).copied().unwrap_or(0) > 0 {
            return false;
        }
        match self.try_verify_entry(id) {
            Ok(meta) => now >= meta.expires_at,
            Err(()) => true,
        }
    }

    /// Meta + plan bytes verify as a stored plan for `id`. Any failure → unverifiable.
    fn try_verify_entry(&self, id: &str) -> Result<PlanMeta, ()> {
        let meta = load_meta_file(&self.meta_path(id)).map_err(|_| ())?;
        let bytes = fs::read(self.plan_path(id)).map_err(|_| ())?;
        Plan::parse_named(id, &bytes, &self.limits).map_err(|_| ())?;
        Ok(meta)
    }

    fn delete_entry(&self, shared: &mut Shared, id: &str) {
        let _ = fs::remove_file(self.plan_path(id));
        let _ = fs::remove_file(self.meta_path(id));
        shared.process_puts.remove(id);
    }

    /// Crash-safe write: plan file renamed into place **before** meta (contract).
    fn write_plan_then_meta(
        &self,
        plan_path: &Path,
        plan_bytes: &[u8],
        meta_path: &Path,
        meta_bytes: &[u8],
    ) -> Result<(), ToolError> {
        // Why this order: a plan without meta is treated as absent; meta-without-plan would
        // look present to readers that only check meta first. Plan-first keeps the invariant.
        write_exclusive(plan_path, plan_bytes)?;
        write_exclusive(meta_path, meta_bytes)?;
        Ok(())
    }
}

fn prune_process_puts(shared: &mut Shared, now: u64) {
    shared.process_puts.retain(|_, exp| now < *exp);
}

fn encode_meta(m: &PlanMeta) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(br#"{"created_at":"#);
    write_u64_dec(&mut out, m.created_at);
    out.extend_from_slice(br#","expires_at":"#);
    write_u64_dec(&mut out, m.expires_at);
    out.extend_from_slice(br#","producer_version":"#);
    write_json_string(&mut out, &m.producer_version);
    out.push(b'}');
    out
}

fn write_u64_dec(out: &mut Vec<u8>, n: u64) {
    out.extend_from_slice(n.to_string().as_bytes());
}

fn write_json_string(out: &mut Vec<u8>, s: &str) {
    out.push(b'"');
    for c in s.chars() {
        match c {
            '"' => out.extend_from_slice(br#"\""#),
            '\\' => out.extend_from_slice(br#"\\"#),
            c if (c as u32) < 0x20 => {
                let n = c as u32;
                let hex = [
                    b'\\',
                    b'u',
                    b'0',
                    b'0',
                    b"0123456789abcdef"[(n >> 4) as usize],
                    b"0123456789abcdef"[(n & 0xf) as usize],
                ];
                out.extend_from_slice(&hex);
            }
            c => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    out.push(b'"');
}

fn load_meta_file(path: &Path) -> Result<PlanMeta, ToolError> {
    let bytes = fs::read(path).map_err(|_| {
        ToolError::new(
            ErrorCode::IoError,
            "cannot read the plan envelope",
            "Check the plan store directory permissions.",
        )
    })?;
    parse_meta(&bytes)
}

fn parse_meta(bytes: &[u8]) -> Result<PlanMeta, ToolError> {
    let corrupt = |msg: &str| {
        ToolError::new(
            ErrorCode::PlanCorrupt,
            msg,
            "Refuse the envelope; re-preview the plan.",
        )
    };
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| corrupt("plan envelope is not valid JSON"))?;
    let obj = value
        .as_object()
        .ok_or_else(|| corrupt("plan envelope must be a JSON object"))?;
    for k in obj.keys() {
        if k != "created_at" && k != "expires_at" && k != "producer_version" {
            return Err(corrupt("plan envelope has an unknown key"));
        }
    }
    for k in ["created_at", "expires_at", "producer_version"] {
        if !obj.contains_key(k) {
            return Err(corrupt("plan envelope is missing a required key"));
        }
    }
    let created_at = meta_u64(
        obj.get("created_at")
            .ok_or_else(|| corrupt("missing created_at"))?,
    )?;
    let expires_at = meta_u64(
        obj.get("expires_at")
            .ok_or_else(|| corrupt("missing expires_at"))?,
    )?;
    let producer_version = obj
        .get("producer_version")
        .and_then(Value::as_str)
        .ok_or_else(|| corrupt("producer_version must be a string"))?
        .to_string();
    if expires_at < created_at {
        return Err(corrupt("plan envelope expires before it was created"));
    }
    let meta = PlanMeta {
        created_at,
        expires_at,
        producer_version,
    };
    if encode_meta(&meta) != bytes {
        return Err(corrupt("plan envelope is not in canonical form"));
    }
    Ok(meta)
}

fn meta_u64(v: &Value) -> Result<u64, ToolError> {
    match v {
        Value::Number(n) => n.as_u64().ok_or_else(|| {
            ToolError::new(
                ErrorCode::PlanCorrupt,
                "plan envelope number is not a non-negative integer",
                "Refuse the envelope; re-preview the plan.",
            )
        }),
        _ => Err(ToolError::new(
            ErrorCode::PlanCorrupt,
            "plan envelope field must be a number",
            "Refuse the envelope; re-preview the plan.",
        )),
    }
}
