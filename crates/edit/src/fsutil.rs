//! Shared filesystem helpers for the plan store and the journal store.
//! Attacker-reachable state directories: identity (dev+ino) re-checks, private-file
//! verification, exclusive durable writes.

use opencrayast_core::error::{ErrorCode, ToolError};
use std::fs;
#[cfg(unix)]
use std::io::Write;
use std::path::{Path, PathBuf};

/// Prefix of exclusive temp *files* created by [`write_exclusive`].
/// Why keep the plan-store spelling: PlanStore sweep recognises leftovers by this exact prefix
/// (EDIT3-04); renaming it would silently stop reclaiming orphans.
pub(crate) const FILE_TEMP_PREFIX: &str = ".opencrayast-plan-tmp-";

/// Identity of a store directory, captured when the store opens and re-checked before every
/// operation.
///
/// What the pair *is* depends on the platform, because the platforms do not offer the same
/// thing:
///
/// - **unix**: the directory's `(device, inode)`. This is the OS's own name for the directory,
///   so it survives renames, detects a swap for a copy, and cannot be forged by a path.
/// - **Windows**: the directory's **creation time**, with `ino` unused. Windows names a file by
///   its volume serial number and file index, and neither is reachable from stable Rust — both
///   `MetadataExt::volume_serial_number` and `MetadataExt::file_index` are behind the unstable
///   `windows_by_handle` feature, and this workspace forbids `unsafe`, so the
///   `GetFileInformationByHandle` call that would return them is not available either.
///
/// So the Windows check is **weaker, and this comment is where that is recorded**: it catches a
/// store directory that was deleted and recreated, or replaced by one moved in from elsewhere,
/// because either produces a different creation time. It does **not** catch a same-creation-time
/// substitution. The unix threat model already accepts same-user tampering inside the state
/// directory (SECURITY-MODEL T-11r/T-03r); on Windows that window is wider, and an operator who
/// needs the unix strength should keep the state directory on a unix host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DirIdentity {
    /// unix: `st_dev`. Windows: the directory's creation time.
    pub(crate) dev: u64,
    /// unix: `st_ino`. Windows: unused (always 0).
    pub(crate) ino: u64,
}

/// The refusal every store-directory check shares, on both platforms: it names the property
/// that failed and never quotes a path or file content.
pub(crate) fn dir_io(what: &str) -> ToolError {
    ToolError::new(
        ErrorCode::IoError,
        format!("store directory is not adoptable: {what}"),
        "Refuse the operation; restore the original store directories or reopen the store.",
    )
}

pub(crate) fn is_workspace_id(s: &str) -> bool {
    let Some(hex) = s.strip_prefix("w-") else {
        return false;
    };
    hex.len() == 32 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Cheap non-crypto entropy for temp names. Used on every platform that builds a
/// store temp (unix and Windows); kept out of `#[cfg(unix)]` so the Windows
/// `write_exclusive` / `make_tmp_dir` arms compile.
pub(crate) fn random_u64() -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    std::time::Instant::now().hash(&mut h);
    std::thread::current().id().hash(&mut h);
    h.finish()
}

#[cfg(unix)]
pub(crate) fn capture_dir_identity(path: &Path) -> Result<DirIdentity, ToolError> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::symlink_metadata(path).map_err(|_| dir_io("could not be inspected"))?;
    if meta.file_type().is_symlink() {
        return Err(dir_io("it is a symlink"));
    }
    if !meta.is_dir() {
        return Err(dir_io("it is not a directory"));
    }
    Ok(DirIdentity {
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

/// Windows implementation. See [`DirIdentity`] for why the identity is a creation time here
/// rather than the file index, and what that does and does not detect.
///
/// The three properties the unix version checks carry over — not a link, is a directory, and the
/// identity has not moved — because they are what the store's re-verification is for. Only the
/// number that stands for "this directory" is different.
#[cfg(windows)]
pub(crate) fn capture_dir_identity(path: &Path) -> Result<DirIdentity, ToolError> {
    use std::os::windows::fs::MetadataExt;

    let meta = fs::symlink_metadata(path).map_err(|_| dir_io("could not be inspected"))?;
    if meta.file_type().is_symlink() {
        return Err(dir_io("it is a symlink"));
    }
    if !meta.is_dir() {
        return Err(dir_io("it is not a directory"));
    }
    Ok(DirIdentity {
        dev: meta.creation_time(),
        ino: 0,
    })
}

#[cfg(unix)]
pub(crate) fn check_dir_identity(path: &Path, expect: DirIdentity) -> Result<(), ToolError> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::symlink_metadata(path).map_err(|_| dir_io("could not be inspected"))?;
    if meta.file_type().is_symlink() {
        return Err(dir_io("it is a symlink"));
    }
    if !meta.is_dir() {
        return Err(dir_io("it is not a directory"));
    }
    if meta.dev() != expect.dev || meta.ino() != expect.ino {
        return Err(dir_io("its identity changed since open"));
    }
    Ok(())
}

/// Windows counterpart of the unix check: same three properties, compared against the pair
/// `capture_dir_identity` produced for this platform.
#[cfg(windows)]
pub(crate) fn check_dir_identity(path: &Path, expect: DirIdentity) -> Result<(), ToolError> {
    use std::os::windows::fs::MetadataExt;

    let meta = fs::symlink_metadata(path).map_err(|_| dir_io("could not be inspected"))?;
    if meta.file_type().is_symlink() {
        return Err(dir_io("it is a symlink"));
    }
    if !meta.is_dir() {
        return Err(dir_io("it is not a directory"));
    }
    if meta.creation_time() != expect.dev {
        return Err(dir_io("its identity changed since open"));
    }
    Ok(())
}

/// Neither the identity check nor its inputs exist off unix and Windows: the store has no
/// way to tell whether a directory was replaced between two operations, so it refuses rather
/// than proceeding on a check it cannot make.
#[cfg(not(any(unix, windows)))]
pub(crate) fn capture_dir_identity(_path: &Path) -> Result<DirIdentity, ToolError> {
    Err(ToolError::new(
        ErrorCode::UnsupportedTarget,
        "store directory identity is not implemented on this platform",
        "Run on a unix or Windows platform.",
    ))
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn check_dir_identity(_path: &Path, _expect: DirIdentity) -> Result<(), ToolError> {
    Err(ToolError::new(
        ErrorCode::UnsupportedTarget,
        "store directory identity is not implemented on this platform",
        "Run on a unix or Windows platform.",
    ))
}

#[cfg(unix)]
pub(crate) fn verify_private_file(path: &Path) -> Result<(), ToolError> {
    use std::os::unix::fs::MetadataExt;

    let meta = fs::symlink_metadata(path).map_err(|_| {
        ToolError::new(
            ErrorCode::IoError,
            "cannot inspect a stored file",
            "Check the store directory permissions.",
        )
    })?;
    if meta.file_type().is_symlink() {
        return Err(ToolError::new(
            ErrorCode::PlanCorrupt,
            "stored file is a symlink",
            "Refuse the file; restore a regular 0600 file.",
        ));
    }
    if !meta.is_file() {
        return Err(ToolError::new(
            ErrorCode::PlanCorrupt,
            "stored path is not a regular file",
            "Refuse the path; only regular files are stored.",
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        ToolError::new(
            ErrorCode::IoError,
            "stored path has no parent",
            "Internal store path error.",
        )
    })?;
    let dir_meta = fs::symlink_metadata(parent).map_err(|_| {
        ToolError::new(
            ErrorCode::IoError,
            "cannot inspect the store directory",
            "Check the store directory permissions.",
        )
    })?;
    if meta.uid() != dir_meta.uid() {
        return Err(ToolError::new(
            ErrorCode::PlanCorrupt,
            "stored file is owned by another user",
            "Refuse the file; use a store you own.",
        ));
    }
    let mode = meta.mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(ToolError::new(
            ErrorCode::PlanCorrupt,
            "stored file grants group or other access",
            "Refuse the file; mode must be 0600 with no group/other bits.",
        ));
    }
    Ok(())
}

/// Windows counterpart: refuse a link or a non-file. Ownership and ACL are not checked, for
/// the reason [`DirIdentity`] records — a security descriptor needs an API the standard library
/// does not expose, and `unsafe` is forbidden. The unix version's "owner matches, no group or
/// other bits" has no stable Windows equivalent at all.
#[cfg(windows)]
pub(crate) fn verify_private_file(path: &Path) -> Result<(), ToolError> {
    let meta = fs::symlink_metadata(path).map_err(|_| {
        ToolError::new(
            ErrorCode::PlanCorrupt,
            "cannot inspect the stored file",
            "Refuse the file; use a store you own.",
        )
    })?;
    if meta.file_type().is_symlink() {
        return Err(ToolError::new(
            ErrorCode::PlanCorrupt,
            "stored file is a link",
            "Refuse the file; the store holds regular files only.",
        ));
    }
    if !meta.is_file() {
        return Err(ToolError::new(
            ErrorCode::PlanCorrupt,
            "stored file is not a regular file",
            "Refuse the entry; the store holds regular files only.",
        ));
    }
    Ok(())
}

/// Exclusive-create temp (0600) → write → fsync → rename → fsync directory.
#[cfg(unix)]
pub(crate) fn write_exclusive(target: &Path, content: &[u8]) -> Result<(), ToolError> {
    use std::os::unix::fs::OpenOptionsExt;

    let parent = target.parent().ok_or_else(|| {
        ToolError::new(
            ErrorCode::IoError,
            "path has no parent directory",
            "Internal store path error.",
        )
    })?;
    let mut tmp_path = None;
    let mut file = None;
    for _ in 0..8 {
        let name = format!(
            "{FILE_TEMP_PREFIX}{}-{:x}",
            std::process::id(),
            random_u64()
        );
        let path = parent.join(&name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(f) => {
                tmp_path = Some(path);
                file = Some(f);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => {
                return Err(ToolError::new(
                    ErrorCode::IoError,
                    "cannot create a temporary file",
                    "Check the store directory permissions and free space.",
                ));
            }
        }
    }
    let (tmp_path, mut file) = match (tmp_path, file) {
        (Some(p), Some(f)) => (p, f),
        _ => {
            return Err(ToolError::new(
                ErrorCode::IoError,
                "cannot create a temporary file",
                "Check the store directory permissions and free space.",
            ));
        }
    };
    let result = (|| {
        file.write_all(content).map_err(|_| {
            ToolError::new(
                ErrorCode::IoError,
                "cannot write a temporary file",
                "Check free space in the store directory.",
            )
        })?;
        file.sync_all().map_err(|_| {
            ToolError::new(
                ErrorCode::IoError,
                "cannot fsync a temporary file",
                "Retry; if it persists, run doctor on the filesystem.",
            )
        })?;
        fs::rename(&tmp_path, target).map_err(|_| {
            ToolError::new(
                ErrorCode::IoError,
                "cannot rename a temporary file into place",
                "Check the store directory permissions.",
            )
        })?;
        opencrayast_core::fsio::fsync_dir(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

/// Windows counterpart of [`write_exclusive`]: temp → write → flush → rename, with two
/// differences that Windows forces and that are named here rather than left implicit.
///
/// - **No mode.** `OpenOptionsExt::mode` is unix-only; a new file inherits the directory's ACL,
///   which under `%LOCALAPPDATA%` is the user's own.
/// - **No directory `fsync`.** `std::fs::File::open` cannot open a directory on Windows, so
///   there is no handle to sync. The rename itself is still atomic, so a reader sees one whole
///   file or the other; what is not guaranteed is that the rename survives a power loss before
///   the filesystem flushes it.
#[cfg(windows)]
pub(crate) fn write_exclusive(target: &Path, content: &[u8]) -> Result<(), ToolError> {
    use std::io::Write;

    let parent = target.parent().ok_or_else(|| {
        ToolError::new(
            ErrorCode::IoError,
            "path has no parent directory",
            "Internal store path error.",
        )
    })?;

    let mut attempt = 0;
    let (tmp_path, mut file) = loop {
        attempt += 1;
        if attempt > 8 {
            return Err(ToolError::new(
                ErrorCode::IoError,
                "cannot create a temporary file",
                "Check the store directory permissions and free space.",
            ));
        }
        let name = format!(
            "{FILE_TEMP_PREFIX}{}-{:x}",
            std::process::id(),
            random_u64()
        );
        let path = parent.join(&name);
        match fs::OpenOptions::new()
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
                    "Check the store directory permissions and free space.",
                ));
            }
        }
    };

    let result = (|| {
        file.write_all(content).map_err(|_| {
            ToolError::new(
                ErrorCode::IoError,
                "cannot write a temporary file",
                "Check free space in the store directory.",
            )
        })?;
        file.sync_all().map_err(|_| {
            ToolError::new(
                ErrorCode::IoError,
                "cannot flush a temporary file",
                "Retry; if it persists, run doctor on the filesystem.",
            )
        })?;
        drop(file);
        // `fs::rename` on Windows is `MoveFileEx` with `MOVEFILE_REPLACE_EXISTING`.
        fs::rename(&tmp_path, target).map_err(|_| {
            ToolError::new(
                ErrorCode::IoError,
                "cannot rename a temporary file into place",
                "Check the store directory permissions.",
            )
        })?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

#[cfg(unix)]
pub(crate) fn mkdir_private(path: &Path) -> Result<(), ToolError> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(path).map_err(|_| {
        ToolError::new(
            ErrorCode::IoError,
            "cannot create a private directory",
            "Check the store directory permissions.",
        )
    })
}

/// Windows counterpart of [`mkdir_private`]: create the directory. There is no mode to request
/// from stable Rust, so the ACL is whatever the parent grants — the same caveat as
/// [`write_exclusive`].
#[cfg(windows)]
pub(crate) fn mkdir_private(path: &Path) -> Result<(), ToolError> {
    fs::create_dir(path).map_err(|_| {
        ToolError::new(
            ErrorCode::IoError,
            "cannot create a store directory",
            "Check the store directory permissions.",
        )
    })
}

/// Create `parent/.tmp-<random>/` with mode 0700.
#[cfg(unix)]
pub(crate) fn make_tmp_dir(parent: &Path) -> Result<PathBuf, ToolError> {
    for _ in 0..8 {
        let path = parent.join(format!(".tmp-{:x}", random_u64()));
        if path.exists() {
            continue;
        }
        match mkdir_private(&path) {
            Ok(()) => return Ok(path),
            Err(_) if path.exists() => continue,
            Err(e) => return Err(e),
        }
    }
    Err(ToolError::new(
        ErrorCode::IoError,
        "cannot create a temporary directory",
        "Check the store directory permissions.",
    ))
}

/// Windows counterpart of [`make_tmp_dir`]: the same retry loop over [`mkdir_private`], whose
/// Windows body does the platform-appropriate thing.
#[cfg(windows)]
pub(crate) fn make_tmp_dir(parent: &Path) -> Result<PathBuf, ToolError> {
    for _ in 0..8 {
        let path = parent.join(format!(".tmp-{:x}", random_u64()));
        if path.exists() {
            continue;
        }
        match mkdir_private(&path) {
            Ok(()) => return Ok(path),
            Err(_) if path.exists() => continue,
            Err(e) => return Err(e),
        }
    }
    Err(ToolError::new(
        ErrorCode::IoError,
        "cannot create a temporary directory",
        "Check the store directory permissions.",
    ))
}
