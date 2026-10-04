//! Workspace identity and the per-workspace apply lock (SECURITY-MODEL T-33; STA-07).

use crate::error::{ErrorCode, ToolError};
use std::path::Path;
use std::time::Duration;

use sha2::{Digest, Sha256};

/// Name of the per-workspace state directory under the state dir.
const WS_DIR_PREFIX: &str = "ws-";
/// Name of the advisory lock file inside it.
const LOCK_FILE_NAME: &str = "apply.lock";
/// Delay between attempts while the lock is held by someone else.
const RETRY_INTERVAL: Duration = Duration::from_millis(10);
/// Hex characters in a workspace id after the `w-` prefix.
const ID_HEX_LEN: usize = 32;

/// 128-bit workspace id, `w-` + 32 lowercase hex chars, derived from the root's device+inode
/// AND its canonical path. Two spellings of one tree (bind mount, symlinked root) give the
/// same id; different trees give different ids.
///
/// The path is canonicalised first, so a symlinked root and the real directory behind it
/// hash the same bytes; the device and inode are mixed in as well, so two different
/// directories that happen to share a path shape cannot collide (T-33). Note that this
/// does not make an id unforgeable across time: a directory that is deleted and recreated
/// at the same path can be handed the same inode, and therefore the same id. An id is a
/// tree identity for the lifetime of that tree, not a permanent name for a path.
///
/// A root that does not exist, or that exists but is not a directory, is `not_found` for
/// the former and `invalid_args` for the latter; a canonicalisation that fails for any
/// other reason (a permission error on a parent, for instance) is `io_error`. All three are
/// refusals, so the caller never gets an id it cannot justify.
///
/// A network or device path is refused before that call, and the distinction is not
/// pedantry: canonicalising `\\server\share` makes Windows resolve a host name and open an
/// SMB session, which for an attacker-supplied host means a DNS lookup and an NTLM challenge
/// sent to a machine they control, plus a hang while the timeout runs (BND-24). The refusal
/// happens on the string, on every platform.
pub fn workspace_id(root: &Path) -> Result<String, ToolError> {
    crate::boundary::refuse_network_path(root)?;
    let canonical = std::fs::canonicalize(root).map_err(|e| {
        let code = match e.kind() {
            std::io::ErrorKind::NotFound => ErrorCode::NotFound,
            _ => ErrorCode::IoError,
        };
        ToolError::new(
            code,
            format!("Workspace root cannot be resolved: {}", e.kind()),
            "Pass a path to a directory that exists and that you can reach.",
        )
    })?;
    let meta = std::fs::metadata(&canonical).map_err(|e| {
        ToolError::new(
            ErrorCode::IoError,
            format!("Workspace root cannot be inspected: {}", e.kind()),
            "Pass a path to a directory that exists and that you can reach.",
        )
    })?;
    if !meta.is_dir() {
        return Err(ToolError::new(
            ErrorCode::InvalidArgs,
            "Workspace root is not a directory",
            "Pass the path of the workspace directory itself.",
        ));
    }

    #[cfg(unix)]
    let (dev, ino) = {
        use std::os::unix::fs::MetadataExt;
        (meta.dev(), meta.ino())
    };
    #[cfg(not(unix))]
    let (dev, ino) = (meta.len(), 0u64);

    let mut hasher = Sha256::new();
    hasher.update(dev.to_le_bytes());
    hasher.update(ino.to_le_bytes());
    // On Unix the path bytes are hashed as they are on disk; on other platforms the lossy
    // form is the best available and the device/inode pair still separates trees.
    #[cfg(unix)]
    hasher.update(canonical.as_os_str().as_encoded_bytes());
    #[cfg(not(unix))]
    hasher.update(canonical.to_string_lossy().as_bytes());
    let digest = hasher.finalize();

    let mut out = String::with_capacity(2 + ID_HEX_LEN);
    out.push_str("w-");
    for byte in digest.iter().take(ID_HEX_LEN / 2) {
        out.push_str(&format!("{byte:02x}"));
    }
    Ok(out)
}

/// True if `s` is exactly `w-` followed by 32 lowercase hex characters.
///
/// The check is a closed character set rather than a filter, so a workspace id can never
/// carry `..`, a separator or any other byte into a path it is about to build (STA-07).
fn is_valid_ws_id(s: &str) -> bool {
    let Some(hex) = s.strip_prefix("w-") else {
        return false;
    };
    hex.len() == ID_HEX_LEN && hex.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

/// Held for the duration of an apply/undo/recover. Released on drop.
#[derive(Debug)]
pub struct ApplyLock {
    _file: std::fs::File,
}

impl ApplyLock {
    /// Take the exclusive advisory lock `<state_dir>/ws-<id>/apply.lock` (creating the
    /// directory `0700` and file `0600`), waiting up to `timeout`; `busy` on timeout.
    ///
    /// The lock is advisory and per workspace, so two applies of the same plan cannot
    /// interleave while applies to two different workspaces never wait for each other
    /// (EDT-07). The handle owns the lock for its whole lifetime and dropping it releases
    /// the lock, including when the holding thread panics or is killed mid-apply, because
    /// the kernel releases the lock when the descriptor closes.
    pub fn acquire(state_dir: &Path, ws_id: &str, timeout: Duration) -> Result<Self, ToolError> {
        if !is_valid_ws_id(ws_id) {
            // Refused before any path is built: a malformed id is the one input that could
            // otherwise escape the state directory.
            return Err(ToolError::new(
                ErrorCode::InvalidArgs,
                "Workspace id is malformed",
                "Use the id returned by workspace_id, which is `w-` and 32 hex characters.",
            ));
        }

        let ws_dir = state_dir.join(format!("{WS_DIR_PREFIX}{ws_id}"));
        // One creator for one directory, with one policy. This used to be a private
        // `create_private_dir` that did `set_permissions(0700)` on **whatever it found**, which
        // is the exact opposite of `ensure_state_dir`'s "refuse, never repair": against a
        // `0777` foreign-owned directory, `ensure_state_dir` refused and the apply path — which
        // went through here — adopted it and silently tightened it. Two creators, opposite
        // policies, and the weaker one was on the apply path.
        //
        // It is the same directory the stores create with the same function, so this is now a
        // second call to the one that decides, not a second policy.
        crate::statedir::ensure_state_dir(&ws_dir)?;
        let lock_path = ws_dir.join(LOCK_FILE_NAME);
        let file = open_lock_file(&lock_path)?;

        let deadline = std::time::Instant::now() + timeout;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(std::fs::TryLockError::Error(e)) => {
                    return Err(ToolError::new(
                        ErrorCode::IoError,
                        format!("Cannot lock the workspace: {}", e.kind()),
                        "Retry; if it persists, run doctor to check the state directory.",
                    ));
                }
                Err(std::fs::TryLockError::WouldBlock) => {}
            }
            if std::time::Instant::now() >= deadline {
                return Err(ToolError::new(
                    ErrorCode::Busy,
                    "Another apply holds the workspace lock",
                    "Wait for it to finish, or retry with a longer timeout.",
                ));
            }
            std::thread::sleep(RETRY_INTERVAL);
        }
    }
}

/// Open the lock file `0600` without following a symlink.
#[cfg(unix)]
fn open_lock_file(path: &Path) -> Result<std::fs::File, ToolError> {
    use std::os::unix::fs::OpenOptionsExt;
    // `O_NOFOLLOW` comes from rustix, which is already a unix dependency of this crate, so
    // this adds no dependency. The value is platform-specific (0o400000 on x86 Linux,
    // 0o100000 on arm64, a different number on the BSDs and macOS), so it must never be
    // written out as a literal: a hardcoded value would silently turn the symlink defence
    // off on every platform except the one it was taken from.
    let nofollow = rustix::fs::OFlags::NOFOLLOW.bits() as i32;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        // Never truncate a lock file, on any platform (clippy::suspicious_open_options
        // flags `create` without it; here it is also the correct behaviour).
        .truncate(false)
        // A symlink planted where the lock file belongs would redirect the lock somewhere
        // else, so the open refuses links outright.
        .custom_flags(nofollow)
        .mode(0o600)
        .open(path)
        .map_err(|e| {
            ToolError::new(
                if e.kind() == std::io::ErrorKind::InvalidInput {
                    ErrorCode::UnsupportedTarget
                } else {
                    ErrorCode::IoError
                },
                format!("Cannot open the workspace lock file: {}", e.kind()),
                "Remove the lock file if it is a symlink, or check the state directory.",
            )
        })?;
    Ok(file)
}

/// The portable open. `O_NOFOLLOW` does not exist here, so a pre-existing symlink is not
/// detected: the caller on such a platform must rely on the state directory being private
/// (`0700` and owned by the user), which is what keeps the lock file unreachable by anyone
/// else.
#[cfg(not(unix))]
fn open_lock_file(path: &Path) -> Result<std::fs::File, ToolError> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        // Never truncate a lock file: another process may be holding it open, and its
        // contents are not ours to clear. Stated explicitly because `create(true)` without
        // it is `clippy::suspicious_open_options`.
        .truncate(false)
        .open(path)
        .map_err(|e| {
            ToolError::new(
                ErrorCode::IoError,
                format!("Cannot open the workspace lock file: {}", e.kind()),
                "Check the state directory.",
            )
        })
}
