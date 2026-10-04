//! The plan and journal stores the six edit tools write through.
//!
//! # Why a process-wide, lazily-opened holder
//!
//! [`PlanStore`] carries state that must live for the whole process, not for one call:
//!
//! - **`plan_max_plans_per_process`** counts the unexpired plans *this `PlanStore` value* has
//!   put. A fresh store per call would reset that counter on every call, so the quota could
//!   never be reached — a quota that cannot be hit is not a quota.
//! - **`in_use`** counts open use-guards. A plan in use must survive a sweep, which only holds
//!   while the same shared cell is alive.
//! - **Directory identity.** Every store operation re-checks that `ws-<id>/` and `plans/` are
//!   still the directories captured at `open`. Re-capturing per call would re-adopt a directory
//!   swapped between two calls, which is exactly the re-pointing that check exists to catch.
//!
//! So the stores are opened **once**, on first use, and reused. A failure is remembered rather
//! than retried: a state directory that cannot be created is a property of the filesystem, not a
//! transient fault, and retrying on every call would repeat the work for nothing.
//!
//! # Why lazy
//!
//! Opening creates `<state>/ws-<id>/plans/` and the journal directory beside it. A server that
//! only ever runs `ast_outline` should leave nothing behind, and this way it does: the first call
//! to any of the six edit tools is what creates the state directory.
//!
//! # Why not on `ToolContext`
//!
//! `ToolContext` is a public cross-crate interface. Adding a field would break every
//! constructor outside this workspace, so the state lives here and is reached from
//! `ServerConfig`, which is where the resolved state directory is already carried.

use opencrayast_tools::{EditTools, JournalStore, PlanStore, SystemClock, ToolContext};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// How long a write waits for the per-workspace apply lock.
///
/// The value the CLI's `edit` path uses (`crates/cli/src/edit.rs::LOCK_TIMEOUT`). Both shells
/// guard the same lock with the same budget, so which shell answers `busy` must not depend on
/// which shell the caller happened to pick.
pub const LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The two stores, opened against one state directory and one workspace id.
pub struct Stores {
    plans: PlanStore,
    journals: JournalStore,
    state_dir: PathBuf,
}

/// Why the stores could not be opened. Remembered so the reason reaches every later call.
#[derive(Debug, Clone)]
pub struct OpenError {
    pub(crate) message: String,
}

/// The open state for one `(state_dir, workspace_id)` pair.
enum Slot {
    Empty,
    /// Opened, and kept for the process. The stores own a `Mutex` and a clock; they are read on
    /// every tool call and must be the *same* value each time, so they are never dropped. The
    /// key is carried alongside because "already open" is only meaningful **for this key**: a
    /// later call against a different state directory must open its own stores rather than be
    /// handed the first caller's.
    Open(PathBuf, String, &'static Stores),
    Failed(PathBuf, String, String),
}

fn slot() -> &'static Mutex<Slot> {
    static SLOT: OnceLock<Mutex<Slot>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(Slot::Empty))
}

/// Open the stores for `(ctx, state_dir)` on first use; reuse them on every later call.
///
/// `ctx.limits` and `ctx.workspace_id` come from the first call and are then fixed for the
/// process — the same lifetime the stores are designed for. Re-opening per call would reset the
/// per-process plan quota and re-adopt a state directory that may have been swapped between
/// calls; see the module docs.
///
/// The returned borrow is `'static` because the stores live until the process exits. The
/// alternative — a borrow tied to the lock guard — cannot outlive the call, which is exactly the
/// lifetime an `EditTools` needs to be usable inside the handler that asked for it.
pub fn stores<'a>(ctx: &'a ToolContext, state_dir: &'a Path) -> Result<&'static Stores, OpenError> {
    let key = (state_dir.to_path_buf(), ctx.workspace_id.clone());
    let mut guard = slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    // Re-open when the key changed (a later call serving a different workspace or state
    // directory) or when nothing is open yet. Never re-open merely because a previous attempt
    // failed: the reason is a property of the filesystem, and retrying per call would repeat
    // the work for nothing.
    let needs_open = match &*guard {
        Slot::Empty => true,
        Slot::Open(dir, ws, _) => (dir.clone(), ws.clone()) != key,
        Slot::Failed(dir, ws, _) => (dir.clone(), ws.clone()) != key,
    };
    if needs_open {
        let clock = || Arc::new(SystemClock) as Arc<dyn opencrayast_tools::Clock>;
        let opened = PlanStore::open(&key.0, &key.1, ctx.limits.clone(), clock())
            .map_err(|e| e.to_string())
            .and_then(|plans| {
                JournalStore::open(&key.0, &key.1, ctx.limits.clone(), clock())
                    .map_err(|e| e.to_string())
                    .map(|journals| Stores {
                        plans,
                        journals,
                        state_dir: key.0.clone(),
                    })
            });
        *guard = match opened {
            // Opened once per key and kept for the process: the stores are what make a plan
            // visible to every later call, so they cannot be dropped between calls.
            Ok(stores) => Slot::Open(key.0.clone(), key.1.clone(), Box::leak(Box::new(stores))),
            Err(message) => Slot::Failed(key.0, key.1, message),
        };
    }

    match &*guard {
        Slot::Open(_, _, stores) => Ok(stores),
        Slot::Failed(_, _, message) => Err(OpenError {
            message: message.clone(),
        }),
        Slot::Empty => unreachable!("the slot is filled above before it is read"),
    }
}

/// The borrowed [`EditTools`] for one call.
pub fn edit_tools<'a>(ctx: &'a ToolContext, stores: &'static Stores) -> EditTools<'a> {
    EditTools {
        tools: ctx,
        plans: &stores.plans,
        journals: &stores.journals,
        state_dir: &stores.state_dir,
        lock_timeout: LOCK_TIMEOUT,
    }
}
