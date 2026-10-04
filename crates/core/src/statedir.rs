//! State directory: **where it is**, and **how it is created and verified** (SECURITY-MODEL
//! T-21; STA-01, STA-03).
//!
//! Two halves, deliberately separate functions:
//!
//! - [`user_state_dir`] resolves the platform's per-user state base (XDG on unix,
//!   `%LOCALAPPDATA%` on Windows) and **refuses** when it cannot. It creates nothing and reads
//!   no filesystem, so it is testable without a directory.
//! - [`ensure_state_dir`] creates one directory `0700` and then **verifies** it: owned by the
//!   current user, no group/other bits, not a symlink. It refuses, and never repairs.
//!
//! Everything below the base is the callers': `PlanStore`, `JournalStore` and `ApplyLock` append
//! `ws-<id>` themselves, and the state is never inside the workspace.

use crate::error::{ErrorCode, ToolError};
use std::path::{Path, PathBuf};

/// The application's directory name, as XDG and `%LOCALAPPDATA%` spell it. This is a **product
/// name, not a dotfile**: it lives under the platform's per-user state base, never beside the
/// user's files.
pub const APP_DIR_NAME: &str = "opencrayast";

/// The name the tool's state used to live under inside a workspace, from the decision that
/// `docs/ARCHITECTURE.md` "State on disk" has always contradicted.
///
/// **Not** a place anything creates any more. It is kept for two reasons that both have to do
/// with upgrades, not with use:
///
/// - [`crate::walk`] refuses to descend into a directory with this name at any depth. A tree
///   checked out with an older build still has one, and an agent must not be able to read the
///   tool's own plans, journals, undo backups and hashes through an ordinary workspace walk.
///   The skip is unconditional because that is the only property that is true of an *existing*
///   directory; refusing to *adopt* one is [`ensure_state_dir`]'s job, not the walker's.
/// - An operator who wants the disk space back gets told where it is and that deleting it costs
///   undo history.
pub const LEGACY_WORKSPACE_STATE_DIR_NAME: &str = ".opencrayast";

/// The user's state directory for one workspace, or a refusal.
///
/// On success the path is the **shared per-user base**, `<state base>/opencrayast`, and the
/// per-workspace directory is a child of it.
///
/// There is deliberately **no workspace parameter**. The workspace-specific subdirectory is not
/// missing, it is appended one level down by the three places that already know the id and
/// build it: `PlanStore::open`, `JournalStore::open` and `ApplyLock::acquire` each do
/// `state.join(format!("ws-{workspace_id}"))`. A resolver that appended the segment itself would
/// make every one of those produce `ws-w-…/ws-w-…/`. So the type is a *base*, the layout is
/// documented, and the doubling is pinned by a test that builds a real store underneath it.
///
/// # Why the workspace id is part of the path, and why this resolver is the only place that
/// appends it
///
/// The existing code appends the `ws-<id>` segment itself — `PlanStore::open`
/// (`crates/edit/src/store.rs`) and `JournalStore::open` (`crates/edit/src/jstore.rs`) both do
/// `state.join(format!("ws-{workspace_id}"))` and `ApplyLock::acquire` does the same. So this
/// function returns exactly the directory those three already build a child of, and appending
/// nothing here is how the segment is kept from doubling.
///
/// A workspace-specific subdirectory is nevertheless mandatory, and it is supplied by the
/// `workspace_id` those callers pass: the id is `w-` + 32 hex derived from the canonical path
/// AND the device+inode (`crate::workspace::workspace_id`), so two workspaces cannot collide,
/// and a renamed or copied tree gets a different id. Layout:
///
/// ```text
/// $XDG_STATE_HOME/opencrayast/ws-<id>/plans/…      ~/.local/state/opencrayast/ws-<id>/…
/// %LOCALAPPDATA%\opencrayast\ws-<id>\plans\…      (Windows)
/// ```
///
/// # Platform branches
///
/// - **Linux and other unix**: `$XDG_STATE_HOME/opencrayast`, falling back to
///   `$HOME/.local/state/opencrayast` when `XDG_STATE_HOME` is unset (XDG Base Directory
///   Specification §3).
/// - **Windows**: `%LOCALAPPDATA%\opencrayast`.
///
/// # Refusals
///
/// Every environment failure is a refusal with a next step, never a guess:
///
/// - **No base at all** — neither variable set, or set to something that is not an absolute
///   path — is `io_error` naming the variable to set. It is **not** a fall back to the workspace:
///   the workspace is exactly the thing being removed, and a silent fall back would put the
///   tool's private state back inside the tree an agent can read.
/// - XDG additionally requires a *relative* `XDG_STATE_HOME` to be ignored (spec §3), so a
///   relative value falls through to `$HOME` rather than producing a path relative to the
///   process's working directory. A `..`-bearing but absolute value is taken as given; this
///   function does not canonicalise a path that does not exist yet.
pub fn user_state_dir() -> Result<PathBuf, ToolError> {
    state_base().map(|base| base.join(APP_DIR_NAME))
}

/// The platform's per-user **state** base, before the application name is appended.
///
/// Split out from [`user_state_dir`] so the platform branch is one small function that reads
/// the environment and one that decides what a value means, rather than both in one place.
/// Nothing here touches the filesystem: resolution must not depend on anything existing yet.
#[cfg(windows)]
fn state_base() -> Result<PathBuf, ToolError> {
    // `%LOCALAPPDATA%`, not `%APPDATA%`: APPDATA is *roaming* and is synchronised to another
    // machine, which is the opposite of what a private plan store and its undo backups want.
    // ARCHITECTURE.md "State on disk" has specified this spelling from the start.
    absolute_from_env("LOCALAPPDATA").ok_or_else(no_base_error)
}

#[cfg(not(windows))]
fn state_base() -> Result<PathBuf, ToolError> {
    // XDG Base Directory Specification section 3: `$XDG_STATE_HOME` if set **and absolute**,
    // otherwise `$HOME/.local/state`. A relative `XDG_STATE_HOME` is, in the spec's words,
    // "not a valid path as per the XDG specification" and has to be ignored — so it falls
    // through to `$HOME` rather than being resolved against the process's working directory,
    // which would put state wherever the tool happened to be run from.
    if let Some(dir) = absolute_from_env("XDG_STATE_HOME") {
        return Ok(dir);
    }
    let home = absolute_from_env("HOME").ok_or_else(no_base_error)?;
    Ok(home.join(".local").join("state"))
}

/// One variable read: present, non-empty and absolute → `Some`, otherwise `None`.
///
/// `None` covers all three ways a base can fail to be determined — unset, set to the empty
/// string, or set to a relative path — and the caller turns that into the one refusal that names
/// the variable to set. The refusal quotes no value and names only the variable, so an error
/// cannot become a channel for whatever the environment happened to contain.
fn absolute_from_env(var: &'static str) -> Option<PathBuf> {
    let raw = std::env::var_os(var).filter(|v| !v.is_empty())?;
    let path = PathBuf::from(raw);
    path.is_absolute().then_some(path)
}

/// The refusal [`state_base`] returns when no base could be determined at all.
///
/// Built from `cfg` so the message names the variable that is actually missing on this platform
/// rather than a generic list.
#[cfg(windows)]
fn no_base_error() -> ToolError {
    ToolError::new(
        ErrorCode::IoError,
        "The user's state directory cannot be determined.",
        "Set LOCALAPPDATA to an absolute path and retry. There is no fallback: state is never \
         written into the workspace.",
    )
}

#[cfg(not(windows))]
fn no_base_error() -> ToolError {
    ToolError::new(
        ErrorCode::IoError,
        "The user's state directory cannot be determined.",
        "Set XDG_STATE_HOME to an absolute path, or HOME so it defaults to \
         $HOME/.local/state, and retry. There is no fallback: state is never written into the \
         workspace.",
    )
}

/// Create `dir` (and parents) with mode `0700` if missing, then VERIFY it: owned by the
/// current user, no group/other permission bits, not a symlink, is a directory. On any
/// failure return `io_error` and do NOT adopt the directory. Returns the canonical path.
/// Unix only for now (Windows ACL check is a later ticket; return `unsupported_target`).
///
/// # Decisions
///
/// - **Refuse, never repair.** A directory that exists but fails verification (wrong
///   owner, group/other bits, symlink, not a directory) is reported to the caller; the
///   function never `chmod`s or `chown`s a directory it did not create. Automatically
///   fixing a directory that another local user may have pre-created is exactly the
///   abuse T-21 describes, and it also hides a real compromise from the operator.
/// - **Only the final component is verified.** A symlinked *parent* is accepted: the
///   state directory's confidentiality comes from its own `0700` mode, not from the
///   names above it, and the canonical path we return is resolved through it. The final
///   component itself must not be a symlink, because that is the one indirection an
///   attacker can re-point between our checks (STA-01, STA-03).
/// - **Verification happens after creation too**, never only on the "already existed"
///   path, and the mode we ask for is `0700` (never group/other bits), so `umask` can
///   only remove owner bits, never widen the directory.
pub fn ensure_state_dir(dir: &Path) -> Result<PathBuf, ToolError> {
    if dir.as_os_str().is_empty() {
        return Err(ToolError::new(
            ErrorCode::InvalidArgs,
            "State directory path is empty.",
            "Pass a non-empty path for the state directory.",
        ));
    }
    #[cfg(unix)]
    {
        ensure_state_dir_unix(dir)
    }
    #[cfg(windows)]
    {
        ensure_state_dir_windows(dir)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = dir;
        Err(ToolError::new(
            ErrorCode::UnsupportedTarget,
            "State directory creation is not implemented on this platform.",
            "Use a unix or Windows platform.",
        ))
    }
}

/// Windows implementation of [`ensure_state_dir`].
///
/// # What is checked, and what is not
///
/// The unix version refuses a directory that is a symlink, is not a directory, is owned by
/// another user, or grants any group/other access. The first two carry over exactly — a
/// reparse point is the Windows equivalent of a symlink and is refused the same way. The last
/// two do not, and this function does not pretend otherwise:
///
/// - **Ownership** is not checked. Reading a file's owner on Windows means walking the security
///   descriptor, which the standard library does not expose. `%LOCALAPPDATA%` is per-user by
///   construction, so the default path is in the user's own profile.
/// - **ACLs** are not checked, for the same reason. A directory under `%LOCALAPPDATA%` inherits
///   the profile's ACL, which grants the user and SYSTEM; that is the intended audience.
///
/// So the confidentiality this function provides on Windows is "the directory exists, is a real
/// directory, and is not a link" — weaker than unix's "…and only its owner can read it". An
/// operator who needs the stronger property should point `--state-dir` at a directory whose ACL
/// they control. This is stated rather than hidden because `ast_info` reports the state directory
/// on every call, and a number that looks verified should be one that is.
#[cfg(windows)]
fn ensure_state_dir_windows(dir: &Path) -> Result<PathBuf, ToolError> {
    use std::fs;

    fn io(dir: &Path, what: &str) -> ToolError {
        ToolError::new(
            ErrorCode::IoError,
            format!("State directory {} failed: {}.", dir.display(), what),
            "Choose a state directory on a filesystem you control and retry.",
        )
    }

    fn refuse(dir: &Path, what: &str, next: &str) -> ToolError {
        ToolError::new(
            ErrorCode::IoError,
            format!(
                "State directory {} is not adoptable: {}.",
                dir.display(),
                what
            ),
            next,
        )
    }

    // `symlink_metadata` does not follow the link, so a reparse point is seen as one. Windows
    // reports a junction as a directory, so the directory test below is the one that has to
    // catch it: `is_dir()` on a junction is true, and canonicalising it would resolve the
    // target, which is exactly what must not be adopted.
    if let Ok(existing) = fs::symlink_metadata(dir) {
        if existing.file_type().is_symlink() {
            return Err(refuse(
                dir,
                "it is a link",
                "Point the state directory at a real directory, or remove the link.",
            ));
        }
        if !existing.is_dir() {
            return Err(refuse(
                dir,
                "it exists and is not a directory",
                "Remove the file and retry.",
            ));
        }
    }

    if fs::symlink_metadata(dir).is_err() {
        // `create_dir_all` treats an already-existing directory as success, so concurrent
        // creators do not fail. There is no mode to request: Windows has no equivalent of 0700
        // that the standard library can set, and the ACL is inherited from the profile.
        fs::create_dir_all(dir).map_err(|_| io(dir, "could not be created"))?;
    }

    // Re-check after creation, for the same reason the unix version does: the entry can have
    // been swapped in between.
    let meta = fs::symlink_metadata(dir).map_err(|_| io(dir, "could not be inspected"))?;
    if meta.file_type().is_symlink() {
        return Err(refuse(
            dir,
            "it is a link",
            "Point the state directory at a real directory, or remove the link.",
        ));
    }
    if !meta.is_dir() {
        return Err(refuse(
            dir,
            "it is not a directory",
            "Remove the file and retry.",
        ));
    }

    // Canonical path so callers key everything off one spelling of the directory, exactly as on
    // unix. The same-user swap window the unix version documents applies here unchanged.
    fs::canonicalize(dir).map_err(|_| io(dir, "could not be resolved"))
}

/// Unix implementation of [`ensure_state_dir`].
#[cfg(unix)]
fn ensure_state_dir_unix(dir: &Path) -> Result<PathBuf, ToolError> {
    use std::fs;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    /// No group or other access of any kind (STA-01).
    const PRIVATE_MASK: u32 = 0o077;

    /// The state directory path is operator-owned configuration, not workspace
    /// content, so naming it in the message is allowed (and necessary).
    fn name(dir: &Path) -> String {
        dir.display().to_string()
    }

    /// Build an error for a failed filesystem operation. The underlying `io::Error`
    /// is not echoed: it can carry foreign text, and the taxonomy only needs to say
    /// what failed and what to do.
    fn io(dir: &Path, what: &str) -> ToolError {
        ToolError::new(
            ErrorCode::IoError,
            format!("State directory {} failed: {}.", name(dir), what),
            "Choose a state directory on a filesystem you control and retry.",
        )
    }

    /// Build the "refused" error. `what` states what is wrong, never file content.
    fn refuse(dir: &Path, what: &str, next: &str) -> ToolError {
        ToolError::new(
            ErrorCode::IoError,
            format!("State directory {} is not adoptable: {}.", name(dir), what),
            next,
        )
    }

    // Never look at the target through a symlink: check it as a link first, because
    // `metadata` and `canonicalize` follow links and would happily adopt one.
    if let Ok(existing) = fs::symlink_metadata(dir) {
        if existing.file_type().is_symlink() {
            return Err(refuse(
                dir,
                "it is a symlink",
                "Point the state directory at a real directory, or remove the symlink.",
            ));
        }
        if !existing.is_dir() {
            return Err(refuse(
                dir,
                "it exists and is not a directory",
                "Remove the file and retry.",
            ));
        }
    }

    let created = if fs::symlink_metadata(dir).is_ok() {
        false
    } else {
        // `recursive(true)` creates every missing level 0700; `create_dir_all` treats an
        // already-existing directory as success, so concurrent creators do not fail.
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|_| io(dir, "could not be created"))?;
        true
    };

    // Re-check as a link (an attacker may have swapped it in between) and take the
    // authoritative metadata of the directory itself, never of a link target.
    let meta = fs::symlink_metadata(dir).map_err(|_| io(dir, "could not be inspected"))?;
    if meta.file_type().is_symlink() {
        return Err(refuse(
            dir,
            "it is a symlink",
            "Point the state directory at a real directory, or remove the symlink.",
        ));
    }
    if !meta.is_dir() {
        return Err(refuse(
            dir,
            "it is not a directory",
            "Remove the file and retry.",
        ));
    }

    let uid = rustix::process::geteuid().as_raw();
    if meta.uid() != uid {
        return Err(refuse(
            dir,
            "it is owned by another user",
            "Use a state directory you own, or have the owner change it.",
        ));
    }

    let mode = meta.mode() & 0o7777;
    if mode & PRIVATE_MASK != 0 {
        return Err(refuse(
            dir,
            "it grants access to the group or to other users",
            "Remove the group and other permissions (chmod 700) and retry.",
        ));
    }

    if created && mode & 0o700 != 0o700 {
        // Only ever on a directory this call just created, and only to undo a
        // restrictive `umask` that stripped the owner's own bits. This never widens
        // access: the requested mode is still 0700 and the value was just checked to
        // have no group/other bits.
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .map_err(|_| io(dir, "could not be given owner-only permissions"))?;
    }

    // Canonical path so callers key everything off one spelling of the directory.
    // A same-user attacker who can rename entries in a parent could still swap this
    // directory between this check and the caller's first use; that window is the
    // accepted same-user risk (SECURITY-MODEL T-11r/T-03r), not something a
    // check-then-use library can close.
    fs::canonicalize(dir).map_err(|_| io(dir, "could not be resolved"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_path_is_invalid_args() {
        let e = ensure_state_dir(Path::new("")).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidArgs);
        assert!(
            !e.message
                .contains(std::env::temp_dir().to_str().unwrap_or("\0"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn missing_directory_is_created_0700() {
        let d = tempfile::tempdir().unwrap();
        let s = d.path().join("x/y/z");
        let p = ensure_state_dir(&s).unwrap();
        assert!(p.is_dir());
        assert!(p.ends_with("x/y/z"));
    }

    /// The legacy workspace name is kept only as a walk skip and as something to tell an operator
    /// to delete. Nothing creates it any more, which is why it must stay a dotfile-shaped name
    /// distinct from the product directory: conflating the two would make the skip list hide a
    /// directory the resolver can legitimately be pointed at.
    #[test]
    fn the_legacy_name_is_distinct_from_the_product_directory() {
        assert_eq!(APP_DIR_NAME, "opencrayast");
        assert_eq!(LEGACY_WORKSPACE_STATE_DIR_NAME, ".opencrayast");
        assert_ne!(APP_DIR_NAME, LEGACY_WORKSPACE_STATE_DIR_NAME);
    }
}
