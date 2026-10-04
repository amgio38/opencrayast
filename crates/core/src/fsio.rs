//! Atomic replacement primitives (EDIT-MODEL E-7, "Apply" steps 9-10, "Preserving file properties").
//!
//! # There is no second way in
//!
//! [`atomic_replace`] is the only function in this workspace that writes a file, and it is
//! **crate-private and boundary-bound**: it takes a `&Boundary` and a `&ResolvedPath`, and it
//! re-proves containment and re-reads the target's identity itself. It is deliberately NOT a
//! `pub fn(&Path, &[u8], FileIdentity)`:
//!
//! - such a signature accepts any path the process can write, which is a second way in
//!   (ARCHITECTURE principle 2), and
//! - it trusts a `FileIdentity` the caller computed, so a stale check is the caller's problem.
//!
//! A test outside this crate therefore cannot reach it; the tests in `tests/` drive it through a
//! real [`Boundary`] instead of through a back door, because a back door would be the very thing
//! this signature exists to remove.

#[cfg(not(any(unix, windows)))]
use crate::boundary::FileIdentity;
#[cfg(any(unix, windows))]
use crate::boundary::{Boundary, FileIdentity, ResolvedPath};
use crate::error::{ErrorCode, ToolError};
use std::path::Path;

/// Prefix of the exclusive temp file, so a leftover from a crash is recognisable.
#[cfg(unix)]
const TEMP_PREFIX: &str = ".opencrayast-tmp-";

/// A callback invoked by [`atomic_replace`] after the target has been checked and before the
/// rename, for TESTS ONLY.
///
/// It exists because the alternative cannot witness anything: the window between the check and the
/// rename is a few microseconds, so a test that tries to hit it by racing is flaky in the direction
/// that matters — it passes when the guard is missing. CR F2 is exactly that: the old suite claimed
/// the pre-rename recheck was pinned, the reviewer removed the call, and every test stayed green.
///
/// A test passes a closure that makes the precise change it wants (replace the target, add a hard
/// link, move a directory) and then asserts the entry point refused. Production passes `None` and
/// this is a single `Option` test, not a hook the write path consults for anything else.
///
/// The hook receives the target's own NAME, not a path. Since SEC-FIX 2 the write path holds no
/// path string to hand it — that is the whole point of the fix — so a test that needs to reach the
/// file uses the workspace root it already has.
///
/// ```compile_fail
/// // Not reachable outside this crate: the seam is crate-private.
/// let _ = opencrayast_core::fsio::BeforeRename::default();
/// ```
#[cfg(unix)]
#[derive(Default)]
pub(crate) struct BeforeRename<'a> {
    /// Called with the target's name, once, immediately before the rename.
    pub(crate) hook: Option<&'a (dyn Fn(&std::ffi::OsStr) + Send + Sync)>,
}

#[cfg(unix)]
impl BeforeRename<'_> {
    /// Run the hook, if there is one.
    fn run(&self, leaf: &std::ffi::OsStr) {
        if let Some(hook) = self.hook {
            hook(leaf);
        }
    }
}

/// A test seam for the extended-attribute copy, for the same reason [`BeforeRename`] exists.
///
/// The rule "preserve or refuse, never do our best" can only be tested by making a copy fail,
/// and no portable way of doing that exists: `trusted.*` needs CAP_SYS_ADMIN to write and can be
/// copied by root, so as root the refusal branch is unreachable. The test would then pass whether
/// or not the refusal is implemented — which is the failure mode this seam exists to remove.
///
/// Production passes `None`. The hook REPLACES the copy, so a test supplies the failure itself.
/// Keeping it a single `Option` alongside the other seam means the write path consults it for
/// nothing else.
#[cfg(unix)]
type AttrCopyHook = dyn Fn(&[Xattr]) -> Result<(), rustix::io::Errno> + Send + Sync;

#[cfg(unix)]
#[derive(Default)]
pub(crate) struct PropertyCopy<'a> {
    /// Called instead of the real `copy_xattrs`, receiving the attributes that were listed.
    pub(crate) hook: Option<&'a AttrCopyHook>,
}

#[cfg(unix)]
impl PropertyCopy<'_> {
    /// Run the real copy, or the test's replacement of it.
    fn run<Fd: std::os::fd::AsFd>(
        &self,
        file: Fd,
        attrs: &[Xattr],
    ) -> Result<(), rustix::io::Errno> {
        match self.hook {
            Some(hook) => hook(attrs),
            None => copy_xattrs(file, attrs),
        }
    }
}

/// Replace `target` with `content` atomically, proving containment again on the way in.
///
/// The temp file is created in a **pinned handle on the target's parent directory**, written,
/// `fsync`ed, given the target's properties, `rename`d over — every one of those steps issued
/// relative to that handle, never to a path string that would have to be resolved again — and the
/// directory is synced through the handle. On ANY failure the temp file is removed and the target
/// is untouched. Preserves mode bits, owner and extended attributes (ACLs, SELinux labels).
///
/// ## Why the signature is `(&Boundary, &ResolvedPath, ..)`
///
/// `ResolvedPath` is a public struct, so its fields are not evidence (principle 2: there is no
/// second way in). Therefore, before anything is opened or created:
///
/// 1. `boundary.refuse_leaf_symlink(&resolved.abs)` — the link check has to precede
///    `canonicalize()`, which would resolve the leaf away;
/// 2. `boundary.open_write_parent(&resolved.abs)` re-derives containment from the path alone and
///    returns a directory handle opened beneath the PINNED ROOT HANDLE with
///    `BENEATH | NO_SYMLINKS`, plus that directory's `dev`/`ino`;
/// 3. the target's identity is read **through the handle**, so the identity compared below is one
///    this call observed rather than one the caller asserted. The caller's own earlier observation
///    can also be passed in (`expect`) and is compared, closing the window between its read and
///    this write.
///
/// ## What this guarantees (SEC-FIX 2 / F-01)
///
/// The read path has always worked from a pinned root descriptor; until now the write path did
/// not, so a parent directory moved out of the workspace between the check and the rename left
/// every per-file check passing (the target inode comes along, still `nlink == 1`, same
/// `dev`/`ino`, not a link) while the content landed outside. Three things close that:
///
/// 1. **no re-resolved path operation** — temp create, `fchmod`, `fchown`, `fsetxattr`,
///    `renameat` and the directory `fsync` are all handle-relative;
/// 2. **the parent is re-proved immediately before the rename** — a handle's own `dev`/`ino`
///    cannot detect a move (the inode travels with the directory), so the check re-opens the same
///    entry from the workspace root and compares. A parent that became a symlink, or another
///    directory, is refused with nothing written;
/// 3. **the target is re-stat'ed through the handle** immediately before the rename, so the
///    content cannot be swapped underneath either.
///
/// Refusals (`unsupported_target`): a symlink target, a target that is not a regular file, a
/// hard-linked target, a read-only target, and a target whose extended attributes cannot be
/// copied. Each refusal uses one generic wording per class and never repeats the target's `nlink`,
/// mode bits or file type (F-04, T-19: with a metadata-free message, a caller that can only name
/// paths cannot use the refusal as an oracle for a file's type, permissions or link count).
///
/// Unix only for now; other platforms return `unsupported_target` ("not yet implemented").
#[cfg(unix)]
pub(crate) fn atomic_replace(
    boundary: &Boundary,
    resolved: &ResolvedPath,
    content: &[u8],
    expect: Option<FileIdentity>,
) -> Result<(), ToolError> {
    atomic_replace_with_seam(
        boundary,
        resolved,
        content,
        expect,
        &BeforeRename::default(),
        &PropertyCopy::default(),
    )
}

/// [`atomic_replace`] with a test seam. Production always uses the default (no hook).
#[cfg(unix)]
pub(crate) fn atomic_replace_with_seam(
    boundary: &Boundary,
    resolved: &ResolvedPath,
    content: &[u8],
    expect: Option<FileIdentity>,
    seam: &BeforeRename<'_>,
    attrs_seam: &PropertyCopy<'_>,
) -> Result<(), ToolError> {
    // CR F1: `canonicalize()` resolves the leaf, so the leaf-link refusal has to happen BEFORE it or
    // it can never fire. This is what makes the `is_symlink()` branch below reachable again.
    boundary.refuse_leaf_symlink(&resolved.abs)?;
    // SEC-FIX 2: containment is re-proved and the parent directory is opened as a handle pinned
    // beneath the workspace root. From here on nothing takes a path string.
    let parent = boundary.open_write_parent(&resolved.abs)?;
    // The identity the CALLER verified before deciding to write. Re-checked here so the window
    // between its read and this write is not unguarded: a target replaced by another file, or turned
    // into a link, in that window is refused rather than silently overwritten (E-7).
    if let Some(expect) = expect
        && observe_identity(&parent)? != expect
    {
        return Err(ToolError::new(
            ErrorCode::IoError,
            "Target changed since it was checked",
            "Re-read the file and rebuild the plan.",
        ));
    }
    let observed = observe_identity(&parent)?;
    atomic_replace_inner(boundary, parent, content, observed, seam, attrs_seam)
}

/// The identity of the target, observed through the parent HANDLE rather than through a path.
///
/// `fstatat` with `AT_SYMLINK_NOFOLLOW` on the leaf relative to the pinned directory descriptor:
/// the name is resolved by the kernel inside a directory we already proved, so there is no window
/// in which a swapped path string could redirect the lookup. Asking the leaf rather than opening
/// it is deliberate — opening a FIFO for reading blocks until a writer appears, so the identity
/// check must not open the target (BND-22).
#[cfg(unix)]
fn observe_identity(parent: &crate::boundary::WriteParent) -> Result<FileIdentity, ToolError> {
    use crate::boundary::FileIdentity;

    let st = rustix::fs::statat(
        &parent.dir,
        &parent.leaf,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(|e| {
        if e == rustix::io::Errno::NOENT {
            ToolError::new(
                ErrorCode::NotFound,
                "Target no longer exists",
                "Re-read the file and rebuild the plan.",
            )
        } else {
            ToolError::new(
                ErrorCode::IoError,
                "Cannot inspect the target.",
                "Re-read the file and rebuild the plan.",
            )
        }
    })?;
    Ok(FileIdentity {
        dev: st.st_dev as u64,
        ino: st.st_ino as u64,
    })
}

/// The write itself, with the target's parent already proven to be inside the workspace and pinned
/// as a handle. Kept separate so [`atomic_replace`] reads as the policy and this as the work.
#[cfg(unix)]
fn atomic_replace_inner(
    boundary: &Boundary,
    parent: crate::boundary::WriteParent,
    content: &[u8],
    expect: FileIdentity,
    seam: &BeforeRename<'_>,
    attrs: &PropertyCopy<'_>,
) -> Result<(), ToolError> {
    // Step 1: check the target before touching anything. A symlink, a hard link or a
    // read-only file cannot be replaced without losing a property the caller depends on,
    // and an identity that moved means someone else already wrote here. Every question is
    // asked of the HANDLE, so a name swapped after the open cannot change the answer.
    let before = fstat_leaf(&parent)?;
    // The leaf-link refusal lives in `atomic_replace`, BEFORE the canonicalisation that would
    // resolve it away (`Boundary::refuse_leaf_symlink`). Kept here as a second line of defence: it
    // is reached only if something re-linked the path between that check and this one, which is
    // precisely the window the caller-identity check above also covers. Either way it is no longer
    // a branch that cannot fire (CR F1).
    if rustix::fs::FileType::from_raw_mode(before.st_mode).is_symlink() {
        return Err(ToolError::new(
            ErrorCode::UnsupportedTarget,
            "Target is a link, not a file this tool may replace.",
            "Write to the real path, or remove the link and retry.",
        ));
    }
    // Only a regular file can be replaced. A FIFO would block forever when opened, a
    // directory cannot be replaced by a file, and a socket or device node has no contents
    // to carry. Refused here, before anything opens the target, because the checks that
    // follow read the target's extended attributes and opening a FIFO for reading blocks until a
    // writer appears.
    // F-04: each refusal class gets ONE generic wording. The target's file type, mode bits and
    // link count are properties of a file the caller may not be allowed to know about; repeating
    // them here would turn every refusal into a metadata oracle for any nameable path. Which class
    // refused is still carried by the error code and by the wording itself.
    if !rustix::fs::FileType::from_raw_mode(before.st_mode).is_file() {
        return Err(ToolError::new(
            ErrorCode::UnsupportedTarget,
            "Target is not a regular file.",
            "Only a regular file can be replaced; edit the file this path resolves to.",
        ));
    }
    if before.st_nlink > 1 {
        return Err(ToolError::new(
            ErrorCode::UnsupportedTarget,
            "Target has more than one hard link.",
            "Edit only an unlinked file; a hard link means another path shares this content.",
        ));
    }
    if before.st_mode & 0o200 == 0 {
        return Err(ToolError::new(
            ErrorCode::UnsupportedTarget,
            "Target is read-only",
            "Make the file writable, or write a new file instead.",
        ));
    }
    // `st_dev` / `st_ino` / `st_uid` have platform-dependent widths and signedness: `dev_t` is an
    // `i32` on macOS and a `u64` on Linux, and `uid_t` is `u32` on Linux. These casts therefore
    // look redundant when this file is compiled on Linux, and clippy says so — but they are what
    // makes the same source correct on macOS. `FileIdentity` widens rather than assuming a width
    // (see `Boundary::open_read`), and this module has to agree with it.
    #[allow(clippy::unnecessary_cast)]
    if before.st_dev as u64 != expect.dev || before.st_ino as u64 != expect.ino {
        return Err(ToolError::new(
            ErrorCode::IoError,
            "Target changed since it was checked",
            "Re-read the file and rebuild the plan.",
        ));
    }

    // Step 2: the temp file lives in the SAME directory, so the rename below cannot cross a
    // filesystem boundary (which would turn an atomic replace into a copy). It is created
    // relative to the pinned handle, never through the path.
    let (tmp_name, tmp_file) = create_temp(&parent)?;
    // From here on every failure removes the temp file and leaves the target alone.
    let result = write_and_replace(
        boundary, &parent, &tmp_file, &tmp_name, content, &before, expect, seam, attrs,
    );
    if result.is_err() {
        let _ = rustix::fs::unlinkat(&parent.dir, &tmp_name, rustix::fs::AtFlags::empty());
    }
    result
}

/// `fstat` the target leaf through the parent handle, refusing nothing on its own.
#[cfg(unix)]
fn fstat_leaf(parent: &crate::boundary::WriteParent) -> Result<rustix::fs::Stat, ToolError> {
    rustix::fs::statat(
        &parent.dir,
        &parent.leaf,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(|e| {
        if e == rustix::io::Errno::NOENT {
            ToolError::new(
                ErrorCode::NotFound,
                "Target no longer exists",
                "Re-read the file and rebuild the plan.",
            )
        } else {
            ToolError::new(
                ErrorCode::IoError,
                format!("Cannot inspect the target: {}", e),
                "Re-read the file and rebuild the plan.",
            )
        }
    })
}

/// Steps 3 to 6: write, sync, adopt the target's properties, re-check, rename, sync the
/// directory. Split out so the failure cleanup in [`atomic_replace`] is in one place.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)] // one extra test-only seam, plus the arguments E-7 always needed
fn write_and_replace(
    boundary: &Boundary,
    parent: &crate::boundary::WriteParent,
    tmp_file: &std::fs::File,
    tmp_name: &std::ffi::OsStr,
    content: &[u8],
    before: &rustix::fs::Stat,
    expect: FileIdentity,
    seam: &BeforeRename<'_>,
    attrs_seam: &PropertyCopy<'_>,
) -> Result<(), ToolError> {
    use std::io::Write;
    use std::os::fd::AsFd;

    // Step 3: write everything, then flush it to the device before the rename, so a crash
    // can never leave the target name pointing at a half-written file.
    let mut file = tmp_file;
    file.write_all(content).map_err(|e| {
        ToolError::new(
            ErrorCode::IoError,
            format!("Cannot write the replacement: {}", e.kind()),
            "Check the free space and permissions of the workspace.",
        )
    })?;
    file.sync_all().map_err(|e| {
        ToolError::new(
            ErrorCode::IoError,
            format!("Cannot flush the replacement: {}", e.kind()),
            "Retry; if it persists, run doctor to check the filesystem.",
        )
    })?;

    // Step 4: adopt the target's mode, and its owner when we are allowed to set one. A
    // chown that is refused (unprivileged, or a different owner) is not fatal: the mode
    // bits and the content are what the edit promised. Both go through the HANDLE.
    rustix::fs::fchmod(
        file.as_fd(),
        rustix::fs::Mode::from_bits_truncate(before.st_mode & 0o7777),
    )
    .map_err(|e| {
        ToolError::new(
            ErrorCode::IoError,
            format!("Cannot set the replacement mode: {}", e),
            "Check the permissions of the workspace.",
        )
    })?;
    #[allow(clippy::unnecessary_cast)] // `uid_t` is `u32` on Linux and narrower on other unixes
    if before.st_uid as u32 != rustix::process::geteuid().as_raw()
        && rustix::fs::fchown(
            file.as_fd(),
            Some(rustix::fs::Uid::from_raw(before.st_uid as u32)),
            None,
        )
        .is_err()
    {
        // EPERM or EINVAL here just means we keep our own owner: the mode bits and the
        // content are what the edit promised, and the owner is not.
    }

    // Step 4b: extended attributes. ACLs and SELinux labels live here on Linux, so this
    // runs before the rename: an attribute we cannot copy is a refusal, and a refusal is
    // cheaper to discover while the target is still untouched. The READ goes through the
    // pinned directory handle as well — the leaf is opened `O_RDONLY | O_NONBLOCK | O_NOFOLLOW`
    // relative to the parent and the `f*` forms run on that descriptor, so a FIFO cannot block
    // the caller (BND-22) and a swapped path string cannot redirect the read.
    let attrs = read_target_xattrs(parent).map_err(|e| attrs_unsupported(&e.to_string()))?;
    if !attrs.is_empty() {
        attrs_seam
            .run(file.as_fd(), &attrs)
            .map_err(|e| attrs_unsupported(&e.to_string()))?;
    }

    // Step 5: re-check BOTH identities immediately before the rename. The gap between this
    // check and the rename is the irreducible race, which is why the lock exists; what this
    // catches is every write that landed in the meantime (EDT-19) and any move of the parent
    // directory out of the workspace (SEC-FIX 2, invariant 2).
    // The test seam fires HERE: after the first check and the temp file are in place, immediately
    // before the rename. Whatever a test does in its hook is exactly the "changed while we were
    // writing" case the recheck below exists to catch.
    seam.run(&parent.leaf);
    recheck_leaf(parent, before, expect)?;
    boundary.parent_still_at_root(&parent.parent_rel, &parent.identity)?;

    // Step 6: the rename itself is atomic, and it is issued FROM the pinned handle, so it
    // replaces the entry in exactly the directory this call verified: a reader sees either the
    // whole old file or the whole new one, never a mixture, and never a file somewhere else.
    rustix::fs::renameat(&parent.dir, tmp_name, &parent.dir, &parent.leaf).map_err(|e| {
        ToolError::new(
            ErrorCode::IoError,
            format!("Cannot replace the target: {}", e),
            "Check the permissions of the directory holding the target.",
        )
    })?;

    // Step 6b: the parent directory must be synced for the rename itself to be durable.
    // By this point the file IS replaced, so a failure here gets its own code rather than a
    // plain io_error that a caller would read as "nothing was written".
    finalize_after_rename(parent)
}

/// Step 6b on its own, so the "replaced but not confirmed durable" outcome can be exercised
/// without having to construct a directory that genuinely cannot be synced.
#[cfg(unix)]
fn finalize_after_rename(parent: &crate::boundary::WriteParent) -> Result<(), ToolError> {
    fsync_dir_handle(&parent.dir).map_err(durability_error)
}

/// Turn a failed directory sync into the one error that says "the write happened".
///
/// Split out because the production path syncs the pinned handle and the test seam syncs a path,
/// but both must produce the SAME error: by the time this runs the rename has already happened, so
/// a caller reading a plain `io_error` would conclude nothing was written and retry a change that
/// actually landed.
#[cfg(unix)]
fn durability_error(sync_error: ToolError) -> ToolError {
    ToolError::new(
        ErrorCode::ReplacedNotDurable,
        format!(
            "File was replaced but the directory sync failed ({}); \
             the change may not survive a crash",
            sync_error.code.as_str()
        ),
        "Re-read the file to see whether the new contents are present, then decide \
         whether to retry; do not assume the change was lost.",
    )
}

/// Step 5 on its own: re-stat the target leaf through the pinned handle and refuse if anything
/// about it moved.
///
/// Split out from [`write_and_replace`] so the rename race can be tested directly, without
/// adding a hook or a flag to the production path: the test performs the write and sync,
/// swaps the target, then calls this and asserts the refusal.
/// Hidden from the docs on purpose: it is a seam for the atomic-write tests, not part of
/// what a caller of this crate should reach for. [`atomic_replace`] is the entry point and
/// the one that enforces the ordering.
#[cfg(unix)]
#[doc(hidden)]
pub fn recheck_target(
    target: &Path,
    before: &std::fs::Metadata,
    expect: FileIdentity,
) -> Result<(), ToolError> {
    use std::os::unix::fs::MetadataExt;

    let now = std::fs::symlink_metadata(target).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ToolError::new(
                ErrorCode::IoError,
                "Target disappeared before it could be replaced",
                "Re-read the file and rebuild the plan.",
            )
        } else {
            ToolError::new(
                ErrorCode::IoError,
                format!("Target changed since it was checked: {}", e.kind()),
                "Re-read the file and rebuild the plan.",
            )
        }
    })?;
    if now.dev() != expect.dev || now.ino() != expect.ino || now.nlink() != before.nlink() {
        return Err(ToolError::new(
            ErrorCode::IoError,
            "Target changed since it was checked",
            "Re-read the file and rebuild the plan.",
        ));
    }
    Ok(())
}

/// Step 5's real form, used by the production write path: `fstatat` the leaf through the pinned
/// handle. Kept separate from [`recheck_target`] so the existing path-based test seam still pins
/// the same three comparisons (dev, ino, nlink) without the write path having to go through a
/// path string to reach them.
#[cfg(unix)]
fn recheck_leaf(
    parent: &crate::boundary::WriteParent,
    before: &rustix::fs::Stat,
    expect: FileIdentity,
) -> Result<(), ToolError> {
    let now = fstat_leaf(parent).map_err(|_| {
        ToolError::new(
            ErrorCode::IoError,
            "Target disappeared before it could be replaced",
            "Re-read the file and rebuild the plan.",
        )
    })?;
    #[allow(clippy::unnecessary_cast)] // same platform-dependent width as in `atomic_replace_inner`
    if now.st_dev as u64 != expect.dev
        || now.st_ino as u64 != expect.ino
        || now.st_nlink != before.st_nlink
    {
        return Err(ToolError::new(
            ErrorCode::IoError,
            "Target changed since it was checked",
            "Re-read the file and rebuild the plan.",
        ));
    }
    Ok(())
}

/// Hidden from the docs on purpose: a seam for the atomic-write tests, like
/// [`recheck_target`]. [`atomic_replace`] is the entry point that enforces the ordering.
///
/// It syncs by PATH, because a test needs to point the seam at a directory it controls — one that
/// does not exist, say, so the sync genuinely fails. The production path syncs the pinned handle
/// instead. Both produce the same error via [`durability_error`], because both mean the same
/// thing: the rename already happened.
#[cfg(unix)]
#[doc(hidden)]
pub fn finalize_after_rename_for_test(parent: &Path) -> Result<(), ToolError> {
    fsync_dir(parent).map_err(durability_error)
}

/// Create the temp file exclusively IN THE PINNED DIRECTORY, so two applies can never share one
/// name and no path string takes part.
#[cfg(unix)]
fn create_temp(
    parent: &crate::boundary::WriteParent,
) -> Result<(std::ffi::OsString, std::fs::File), ToolError> {
    // No `create_new` fallback loop with a fixed name: the random component is what makes
    // a collision negligible, and `create(true)` is deliberately NOT used, so a name that
    // already exists is an error rather than something we overwrite.
    for _ in 0..8 {
        let name = std::ffi::OsString::from(format!(
            "{TEMP_PREFIX}{}-{}",
            std::process::id(),
            random_suffix()
        ));
        // `O_EXCL` and `O_NOFOLLOW` relative to the pinned descriptor: the name is created in
        // the directory that was proved, and cannot follow a link left there by anyone else.
        let oflags = rustix::fs::OFlags::WRONLY
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::EXCL
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC;
        match rustix::fs::openat(
            &parent.dir,
            &name,
            oflags,
            rustix::fs::Mode::from_raw_mode(0o600),
        ) {
            Ok(fd) => return Ok((name, std::fs::File::from(fd))),
            Err(e) if e == rustix::io::Errno::EXIST => continue,
            Err(_) => {
                return Err(ToolError::new(
                    ErrorCode::IoError,
                    "Cannot create a temporary file next to the target",
                    "Check that the target's directory is writable.",
                ));
            }
        }
    }
    Err(ToolError::new(
        ErrorCode::IoError,
        "Cannot create a temporary file next to the target",
        "Check that the target's directory is writable, then retry.",
    ))
}

/// The refusal every attribute-preservation failure turns into.
///
/// "Extended attributes cannot be preserved" is the wording the table in EDIT-MODEL uses,
/// and it is deliberately the same whatever went wrong: the caller cannot fix it by
/// retrying, and the important fact is that nothing was written.
#[cfg(unix)]
fn attrs_unsupported(detail: &str) -> ToolError {
    ToolError::new(
        ErrorCode::UnsupportedTarget,
        format!("Extended attributes cannot be preserved on this target ({detail})"),
        "Copy the attributes to the new file yourself, or edit a target without them.",
    )
}

/// One extended attribute: a NUL-terminated name and its bytes.
#[cfg(unix)]
pub(crate) type Xattr = (Vec<u8>, Vec<u8>);

/// Open the target leaf through the pinned parent handle, for an attribute read.
///
/// `O_NONBLOCK` is what keeps this from hanging: opening a FIFO for reading blocks until a
/// writer appears, so an attribute check on a FIFO would hang the caller instead of refusing it.
/// `O_NOFOLLOW` plus the pinned parent mean the descriptor can only be the file the plan named.
/// By the time this runs the target is known to be a regular file, and its identity is
/// re-checked through the same handle immediately before the rename.
#[cfg(unix)]
fn open_leaf_for_attrs(
    parent: &crate::boundary::WriteParent,
) -> Result<rustix::fd::OwnedFd, rustix::io::Errno> {
    rustix::fs::openat(
        &parent.dir,
        &parent.leaf,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
}

/// Read the target's extended attributes through the pinned parent handle.
#[cfg(unix)]
fn read_target_xattrs(
    parent: &crate::boundary::WriteParent,
) -> Result<Vec<Xattr>, rustix::io::Errno> {
    list_xattrs(parent)
}

/// Read every extended attribute of `path`.
///
/// Uses the path-based `listxattr`/`getxattr` rather than opening the file and using the
/// `f*` variants. Opening the target is what makes the `f*` form dangerous here: opening a
/// FIFO for reading blocks until a writer appears, so an attribute check on a FIFO would
/// hang the caller instead of refusing it. The path form never opens the file, and it also
/// has no `O_NOFOLLOW` flag to get wrong; the target is known to be a regular file by the
/// time this runs, and the identity is re-checked immediately before the rename.
///
/// POSIX ACLs and SELinux labels are carried as `system.posix_acl_access` and
/// `security.selinux` on Linux, so they are covered here without special cases.
///
/// A filesystem with no xattr support at all reports `ENOTSUP`/`EOPNOTSUPP`, which is
/// mapped to "no attributes" so that an ordinary replace still works on such a
/// filesystem. Any other error is a real failure and the caller refuses.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn list_xattrs(parent: &crate::boundary::WriteParent) -> Result<Vec<Xattr>, rustix::io::Errno> {
    let fd = open_leaf_for_attrs(parent)?;

    let mut names: Vec<u8> = Vec::new();
    let mut len = match rustix::fs::flistxattr(&fd, &mut names) {
        Ok(n) => n,
        Err(e) if is_no_xattr_support(e) => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    for _ in 0..4 {
        if len == 0 {
            return Ok(Vec::new());
        }
        names.clear();
        names.resize(len, 0u8);
        let n = match rustix::fs::flistxattr(&fd, &mut names) {
            Ok(n) => n,
            Err(e) if is_no_xattr_support(e) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        if n <= len {
            len = n;
            break;
        }
        // The list grew between the two calls; ask again with the new size.
        len = n;
    }
    names.truncate(len);

    let mut attrs = Vec::new();
    for name in names.split(|b| *b == 0) {
        if name.is_empty() {
            continue;
        }
        // Same shape again: size, then fill. A single call with an empty buffer returns
        // the size and fills nothing.
        let mut value: Vec<u8> = Vec::new();
        let vlen = match rustix::fs::fgetxattr(&fd, name, &mut value) {
            Ok(n) => n,
            Err(e) if is_no_xattr_support(e) => continue,
            Err(e) => return Err(e),
        };
        value.clear();
        value.resize(vlen, 0u8);
        match rustix::fs::fgetxattr(&fd, name, &mut value) {
            Ok(n) => value.truncate(n),
            Err(e) if is_no_xattr_support(e) => continue,
            Err(e) => return Err(e),
        }
        attrs.push((name.to_vec(), value));
    }
    Ok(attrs)
}

/// True when the error means "this filesystem has no extended attributes at all".
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn is_no_xattr_support(e: rustix::io::Errno) -> bool {
    matches!(e, rustix::io::Errno::OPNOTSUPP | rustix::io::Errno::NOSYS)
}

/// Copy each attribute onto the temp file.
///
/// Any failure is returned to the caller, which turns it into the single refusal wording:
/// a half-copied set of attributes is worse than none, and the rule is "preserve or
/// refuse", never "do our best".
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn copy_xattrs<Fd: std::os::fd::AsFd>(file: Fd, attrs: &[Xattr]) -> Result<(), rustix::io::Errno> {
    let file = &file;
    for (name, value) in attrs {
        rustix::fs::fsetxattr(file, name, value, rustix::fs::XattrFlags::empty())?;
    }
    Ok(())
}

/// On a unix that is neither Linux nor macOS there is no portable xattr API to call, so a
/// file carrying attributes could not be replaced without losing them. Rather than guess,
/// this refuses whenever the platform cannot even enumerate them: the file system may have
/// none at all, and then the refusal is merely conservative, but nothing is ever written
/// silently without its properties.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn list_xattrs(_parent: &crate::boundary::WriteParent) -> Result<Vec<Xattr>, rustix::io::Errno> {
    Err(rustix::io::Errno::OPNOTSUPP)
}

/// Unreachable here, and only reached if [`list_xattrs`] somehow reported attributes.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn copy_xattrs<Fd: std::os::fd::AsFd>(
    _file: Fd,
    _attrs: &[Xattr],
) -> Result<(), rustix::io::Errno> {
    Ok(())
}

/// A short random-looking suffix. It only has to make a name collision unlikely between
/// concurrent applies; the exclusive create above is what actually guarantees uniqueness.
#[cfg(unix)]
#[cfg(unix)]
fn random_suffix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    // Mix in the address of a stack local so two calls in the same nanosecond differ.
    let local = 0u8;
    let addr = &local as *const u8 as u64;
    nanos ^ addr.rotate_left(17) ^ (std::process::id() as u64).rotate_left(43)
}

/// Windows replace: temp in the same directory → write → flush → rename over the target.
///
/// Weaker than the unix arm (no openat/handle-relative rename, no directory fsync, identity
/// is creation_time rather than dev/ino — see [`crate::boundary::Boundary::open_read`]), but
/// it is a real write path: rename within a directory is atomic on NTFS, and a reader sees
/// one whole file or the other. The write policy has already refused symlinks, non-files
/// and read-only targets (hard-link count is not checked on stable Windows — see
/// `Boundary::resolve_write`).
#[cfg(windows)]
pub(crate) fn atomic_replace(
    boundary: &Boundary,
    resolved: &ResolvedPath,
    content: &[u8],
    expect: Option<FileIdentity>,
) -> Result<(), ToolError> {
    use std::io::Write;
    use std::os::windows::fs::MetadataExt;

    // Re-prove the write policy on the live spelling: a target that became a link or left
    // the workspace between preview and apply must still be refused here.
    let live = boundary.resolve_write(&resolved.rel)?;
    if live.abs != resolved.abs {
        return Err(ToolError::new(
            ErrorCode::IoError,
            "The write target moved between checks.",
            "Re-preview the plan and apply again.",
        ));
    }

    if let Some(expect) = expect {
        let meta = std::fs::metadata(&live.abs).map_err(|_| {
            ToolError::new(
                ErrorCode::IoError,
                "The write target could not be inspected before replace.",
                "Retry; if it persists, check the file permissions.",
            )
        })?;
        let now = FileIdentity {
            dev: meta.creation_time(),
            ino: 0,
        };
        if now != expect {
            return Err(ToolError::new(
                ErrorCode::IoError,
                "The write target changed identity between open and replace.",
                "Re-preview the plan; another process may have replaced the file.",
            ));
        }
    }

    let parent = live.abs.parent().ok_or_else(|| {
        ToolError::new(
            ErrorCode::IoError,
            "path has no parent directory",
            "Internal path error.",
        )
    })?;

    let mut attempt = 0u32;
    let (tmp_path, mut file) = loop {
        attempt += 1;
        if attempt > 8 {
            return Err(ToolError::new(
                ErrorCode::IoError,
                "cannot create a temporary file",
                "Check the directory permissions and free space.",
            ));
        }
        let name = format!(".opencrayast-tmp-{}-{:x}", std::process::id(), {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            std::time::Instant::now().hash(&mut h);
            attempt.hash(&mut h);
            h.finish()
        });
        let path = parent.join(&name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(f) => break (path, f),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => {
                return Err(ToolError::new(
                    ErrorCode::IoError,
                    "cannot create a temporary file",
                    "Check the directory permissions and free space.",
                ));
            }
        }
    };

    let result = (|| -> Result<(), ToolError> {
        file.write_all(content).map_err(|_| {
            ToolError::new(
                ErrorCode::IoError,
                "cannot write the temporary file",
                "Check free space and retry.",
            )
        })?;
        file.flush().map_err(|_| {
            ToolError::new(
                ErrorCode::IoError,
                "cannot flush the temporary file",
                "Check free space and retry.",
            )
        })?;
        drop(file);
        std::fs::rename(&tmp_path, &live.abs).map_err(|_| {
            ToolError::new(
                ErrorCode::IoError,
                "cannot rename a temporary file into place",
                "Check the directory permissions.",
            )
        })?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

/// `fsync` an already-open directory HANDLE, so a rename issued from it is durable.
///
/// The handle form is what the atomic file replace uses: the sync addresses the SAME pinned
/// directory the rename was issued from, so it cannot end up syncing some other directory that
/// took the name in between.
#[cfg(unix)]
fn fsync_dir_handle(dir: &rustix::fd::OwnedFd) -> Result<(), ToolError> {
    let file = std::fs::File::from(dir.try_clone().map_err(|_| {
        ToolError::new(
            ErrorCode::IoError,
            "Cannot sync the directory: the handle could not be used",
            "Retry; if it persists, run doctor to check the filesystem.",
        )
    })?);
    file.sync_all().map_err(|e| {
        ToolError::new(
            ErrorCode::IoError,
            format!("Cannot sync the directory: {}", e.kind()),
            "Retry; if it persists, run doctor to check the filesystem.",
        )
    })
}

/// `fsync` a directory so a rename inside it is durable.
///
/// Still reached by path, and still used by the state-store code (`jstore`, `fsutil`), which has
/// no workspace boundary to pin a handle against. The atomic file replace does NOT use this.
#[cfg(unix)]
pub fn fsync_dir(dir: &Path) -> Result<(), ToolError> {
    let file = std::fs::File::open(dir).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ToolError::new(
                ErrorCode::IoError,
                "Directory to sync no longer exists",
                "Check the workspace path and retry.",
            )
        } else {
            ToolError::new(
                ErrorCode::IoError,
                format!("Cannot open the directory to sync: {}", e.kind()),
                "Check the workspace path and retry.",
            )
        }
    })?;
    file.sync_all().map_err(|e| {
        ToolError::new(
            ErrorCode::IoError,
            format!("Cannot sync the directory: {}", e.kind()),
            "Retry; if it persists, run doctor to check the filesystem.",
        )
    })
}

/// `fsync` a directory so a rename inside it is durable.
///
/// Windows has no portable directory-fsync in `std`. Confirm the path is still a directory
/// and return Ok — NTFS treats the file rename as the durability boundary for the plan
/// store (same accepted weaker guarantee as the Windows `atomic_replace` arm).
#[cfg(windows)]
pub fn fsync_dir(dir: &Path) -> Result<(), ToolError> {
    let meta = std::fs::metadata(dir).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ToolError::new(
                ErrorCode::IoError,
                "Directory to sync no longer exists",
                "Check the workspace path and retry.",
            )
        } else {
            ToolError::new(
                ErrorCode::IoError,
                format!("Cannot open the directory to sync: {}", e.kind()),
                "Check the workspace path and retry.",
            )
        }
    })?;
    if !meta.is_dir() {
        return Err(ToolError::new(
            ErrorCode::IoError,
            "Cannot sync the directory: not a directory",
            "Check the workspace path and retry.",
        ));
    }
    Ok(())
}

/// `fsync` a directory so a rename inside it is durable.
#[cfg(not(any(unix, windows)))]
pub fn fsync_dir(_dir: &Path) -> Result<(), ToolError> {
    Err(ToolError::new(
        ErrorCode::UnsupportedTarget,
        "Directory sync is not yet implemented on this platform",
        "Run on a platform with POSIX directory sync.",
    ))
}
