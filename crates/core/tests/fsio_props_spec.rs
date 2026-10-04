//! Spec for ISSUE-CORE-ATOMIC-PROPS: extended attributes survive a replace, an attribute
//! that cannot be copied is a refusal that leaves nothing behind, and a rename that
//! succeeded but could not be made durable says so in its own words.
//! Refs: EDIT-MODEL "Preserving file properties" (ACLs / xattr / SELinux row), E-7.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
mod common;

use common::atomic::Ws;
use opencrayast_core::ErrorCode;
use opencrayast_core::boundary::FileIdentity;
use opencrayast_core::limits::Limits;
use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;

fn ident(p: &Path) -> FileIdentity {
    let m = fs::metadata(p).unwrap();
    FileIdentity {
        dev: m.dev(),
        ino: m.ino(),
    }
}

fn is_root() -> bool {
    rustix::process::geteuid().as_raw() == 0
}

/// Names in `dir` other than the one we are allowed to keep.
fn leftovers(dir: &Path, keep: &str) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .filter(|n| n != keep)
        .collect()
}

/// Read one extended attribute.
///
/// Two calls on purpose: an empty buffer asks the kernel for the size, then a buffer of
/// that size receives the bytes. A single call with an empty buffer returns the size and
/// fills nothing, which is a quiet way to end up comparing against an empty value.
fn get_xattr<F: std::os::fd::AsFd>(f: F, name: &[u8]) -> Vec<u8> {
    let mut value: Vec<u8> = Vec::new();
    let n = rustix::fs::fgetxattr(&f, name, &mut value).unwrap();
    value.clear();
    value.resize(n, 0u8);
    let n = rustix::fs::fgetxattr(&f, name, &mut value).unwrap();
    value.truncate(n);
    value
}

/// True when this filesystem can actually carry a `user.*` xattr. Printed when it cannot,
/// so a skipped case is never mistaken for a passing one.
///
/// The probe lives in its own temporary directory and is thrown away with it. An earlier
/// version took a path from the caller and created the probe there, which left a stray
/// `probe` file inside the directory under test: the tests that assert "no temp file may
/// remain" then counted the probe as a leftover and failed (CI run 36921251328 on macOS,
/// where the branch actually executes). Owning the directory keeps every caller's
/// directory free of scaffolding, so `leftovers` stays strict.
fn xattrs_supported() -> bool {
    use std::os::fd::AsFd;
    let scratch = match tempfile::tempdir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("SKIPPED xattr cases: cannot create a probe directory ({e:?})");
            return false;
        }
    };
    let sample = scratch.path().join("probe");
    // The two calls return different error types, so they are not chained.
    let made = match std::fs::File::create_new(&sample) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("SKIPPED xattr cases: cannot create a probe file ({e:?})");
            return false;
        }
    };
    match rustix::fs::fsetxattr(
        made.as_fd(),
        &b"user.opencrayprobe"[..],
        b"1",
        rustix::fs::XattrFlags::empty(),
    ) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("SKIPPED xattr cases: this filesystem refuses user.* xattrs ({e:?})");
            false
        }
    }
}

/// A: an extended attribute on the target survives the replace with the same value, and
/// the replacement really is a new inode rather than the original file left alone.
#[test]
fn user_xattr_survives_the_replace() {
    use std::os::fd::AsFd;
    if !xattrs_supported() {
        // `xattrs_supported` already printed why it declined (probe directory, probe file, or the
        // filesystem refusing `user.*`); naming the test here too means the pair reads as one
        // skip rather than as a test that quietly passed on a filesystem that cannot run it.
        eprintln!("SKIPPED: user_xattr_survives_the_replace (this filesystem cannot store xattrs)");
        return;
    }
    let ws = Ws::new();
    let t = ws.abs("f.txt");
    ws.put("f.txt", b"old");
    let f = fs::OpenOptions::new().write(true).open(&t).unwrap();
    rustix::fs::fsetxattr(
        f.as_fd(),
        &b"user.test"[..],
        b"keep me",
        rustix::fs::XattrFlags::empty(),
    )
    .unwrap();
    drop(f);
    let before = ident(&t);

    ws.replace_ok("f.txt", b"new content");

    assert_eq!(fs::read_to_string(&t).unwrap(), "new content");
    // Safe on every filesystem, including the ones that reuse inode numbers aggressively
    // (APFS): `atomic_replace` creates the replacement file BEFORE the rename frees the old
    // inode, so the new number is allocated while the old one is still live and can never be
    // handed back. Asserting "a different inode" here is asserting that ordering, not a hope
    // about the allocator.
    assert_ne!(ident(&t), before, "the file must be a new inode");
    // The attribute is still there, with the same value.
    let f = fs::File::open(&t).unwrap();
    let value = get_xattr(f.as_fd(), &b"user.test"[..]);
    assert_eq!(
        value, b"keep me",
        "the extended attribute must be preserved"
    );
    // Strict: the target lives in the workspace root, so nothing but the target should be there.
    // This used to filter out a `probe` file; that was masking the scaffolding rather than
    // removing it.
    assert!(
        leftovers(&ws.root, "f.txt").is_empty(),
        "no temp file may be left behind: {:?}",
        leftovers(&ws.root, "f.txt")
    );
}

/// Several attributes at once, including a non-`user.` namespace that an unprivileged user
/// may not write: as root those are settable, as a normal user they are not, so the test
/// branches on the effective uid instead of pretending one answer fits everyone.
#[test]
fn several_xattrs_are_all_preserved() {
    use std::os::fd::AsFd;
    let ws = Ws::new();
    if !xattrs_supported() {
        // As above: the helper prints the cause, this line ties it to the test that did not run.
        eprintln!(
            "SKIPPED: several_xattrs_are_all_preserved (this filesystem cannot store xattrs)"
        );
        return;
    }
    let t = ws.abs("f.txt");
    ws.put("f.txt", b"old");
    let f = fs::OpenOptions::new().write(true).open(&t).unwrap();
    for (name, value) in [
        (&b"user.a"[..], &b"one"[..]),
        (&b"user.b"[..], &b"two"[..]),
        (&b"user.empty"[..], &b""[..]),
    ] {
        rustix::fs::fsetxattr(f.as_fd(), name, value, rustix::fs::XattrFlags::empty()).unwrap();
    }
    let privileged = if is_root() {
        // `security.*` needs CAP_MAC_ADMIN, which the test's own privileges decide.
        rustix::fs::fsetxattr(
            f.as_fd(),
            &b"security.opencraytest"[..],
            b"label",
            rustix::fs::XattrFlags::empty(),
        )
        .is_ok()
    } else {
        false
    };
    drop(f);

    ws.replace_ok("f.txt", b"new");

    let f = fs::File::open(&t).unwrap();
    let get = |name: &[u8]| -> Vec<u8> { get_xattr(f.as_fd(), name) };
    assert_eq!(get(&b"user.a"[..]), b"one");
    assert_eq!(get(&b"user.b"[..]), b"two");
    assert_eq!(
        get(&b"user.empty"[..]),
        b"",
        "an empty value is still an attribute"
    );
    if privileged {
        assert_eq!(
            get(&b"security.opencraytest"[..]),
            b"label",
            "a security label must be preserved like any other"
        );
    } else {
        eprintln!(
            "SKIPPED security.* arm: this process cannot write security.* xattrs \
             (running as uid {})",
            rustix::process::geteuid().as_raw()
        );
    }
}

/// A file with no attributes at all replaces normally: the common case must not become a
/// refusal just because the feature exists.
#[test]
fn a_file_without_xattrs_replaces_normally() {
    let ws = Ws::new();
    let t = ws.abs("f.txt");
    ws.put("f.txt", b"old");
    ws.replace_ok("f.txt", b"new");
    assert_eq!(fs::read_to_string(&t).unwrap(), "new");
    assert!(leftovers(&ws.root, "f.txt").is_empty());
}

/// B: a rename that succeeded but whose directory could not be synced is reported as its
/// own code. `finalize_after_rename` is the seam, exactly as `recheck_target` is for the
/// rename race: a directory that genuinely cannot be fsynced is not something a test can
/// rely on being able to construct portably.
///
/// Here the seam is driven through a directory that does not exist, which makes `fsync_dir`
/// fail for a real reason, and asserts the code, the wording, and that nothing claims the
/// write was lost.
#[test]
fn replaced_but_not_durable_has_its_own_code() {
    // A path that cannot be opened as a directory makes the inner `fsync_dir` fail.
    let missing = Path::new("/nonexistent-opencrayast-dir-for-durability-test");
    // The inner `fsync_dir` genuinely fails for this path; what the caller must see is the
    // outer code, because by the time this point is reached the file HAS been replaced.
    let e = durability_error_for(missing);
    assert_eq!(e.code, ErrorCode::ReplacedNotDurable);
    assert!(
        e.message.contains("may not survive a crash"),
        "the message must say the change may be lost: {}",
        e.message
    );
    // The message must not read as a rollback. "Do not assume the change was lost" is the
    // opposite of that, so the substring is checked for its negation, not banned outright.
    assert!(
        e.message.contains("File was replaced"),
        "the message must lead with the fact that the write happened: {}",
        e.message
    );
    assert!(
        e.next.contains("do not assume the change was lost"),
        "the next step must warn against assuming a rollback: {}",
        e.next
    );
    assert!(
        e.next.contains("Re-read the file"),
        "the next step must tell the caller how to find out what happened: {}",
        e.next
    );
    assert_ne!(
        e.code,
        ErrorCode::IoError,
        "the inner sync failure must not be reported as a plain io_error"
    );
}

/// Calls the step-6b seam directly. Named here so the test reads as the contract it checks.
fn durability_error_for(dir: &Path) -> opencrayast_core::ToolError {
    opencrayast_core::fsio::finalize_after_rename_for_test(dir)
        .expect_err("a directory that cannot be synced must not report success")
}

/// A non-regular target must be refused, and — the point of the fix — refused promptly.
///
/// Before the `is_file()` check, reading a FIFO's extended attributes opened it for
/// reading, which blocks until a writer appears. A caller could therefore hang forever on
/// a path it was only supposed to inspect. Each case below runs under a deadline and
/// reports how long the refusal actually took, so a regression shows up as a hang rather
/// than as a slow test.
const REFUSAL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

/// Run `f` on another thread and fail if it has not finished within the deadline. A blocked
/// thread is left behind, not joined: the test reports the hang and the process still exits.
fn within_deadline<F, T>(what: &str, f: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    let name = what.to_string();
    std::thread::spawn(move || {
        let r = f();
        let _ = tx.send(r);
    });
    match rx.recv_timeout(REFUSAL_DEADLINE) {
        Ok(v) => v,
        Err(_) => panic!("{name} was not refused within {REFUSAL_DEADLINE:?}: it blocked"),
    }
}

/// Refuse a non-regular target under the deadline. Everything the call needs is moved in,
/// because the closure runs on another thread and so outlives the caller.
fn refused_promptly(
    what: &str,
    ws: &Ws,
    rel: &str,
) -> (opencrayast_core::ToolError, std::time::Duration) {
    let started = std::time::Instant::now();
    // F-01b: no identity is handed in any more — the write goes through the policy, which observes
    // it. Everything the call needs is moved in, because the closure runs on another thread.
    let boundary = opencrayast_core::boundary::Boundary::new(
        opencrayast_core::boundary::BoundaryConfig::new(ws.root.clone(), Limits::default()),
    )
    .unwrap();
    // The policy may refuse the target already (`resolve_write` rejects a non-regular target), and
    // that refusal is prompt and correct. This helper is about the WRITE path being prompt, so it
    // handles both: a refusal from either stage counts.
    let resolved = match boundary.resolve_write(rel) {
        Ok(r) => r,
        Err(e) => return (e, started.elapsed()),
    };
    let e = within_deadline(what, move || boundary.replace_file(&resolved, b"x"))
        .expect_err("a non-regular target must be refused");
    (e, started.elapsed())
}

/// A FIFO target is refused without blocking.
#[test]
fn fifo_target_is_refused_promptly() {
    let ws = Ws::new();
    let fifo = ws.abs("pipe");
    let rel_fifo = "pipe".to_string();
    // Created with `mkfifo` from coreutils rather than a syscall wrapper: `rustix` exposes
    // `mkfifoat` but excludes it on Apple, and `mknodat` does not exist on macOS at all,
    // so there is no single rustix call that builds a FIFO on every unix this crate targets.
    // `mkfifo` needs no capability and no device node, so it is safe to run as any user.
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("mkfifo must be available to run this test");
    assert!(made.success(), "mkfifo failed: {made:?}");
    if !fs::symlink_metadata(&fifo).unwrap().file_type().is_fifo() {
        eprintln!("SKIPPED FIFO case: mkfifo did not create a FIFO on this platform");
        return;
    }
    let (e, took) = refused_promptly("FIFO target", &ws, &rel_fifo);
    assert_eq!(e.code, ErrorCode::UnsupportedTarget, "{e}");
    assert!(
        e.message.contains("not a regular file") || e.message.contains("must be a regular file"),
        "the message must say what is wrong, without repeating the target's type: {}",
        e.message
    );
    eprintln!("FIFO target refused in {took:?}");
    assert!(took < REFUSAL_DEADLINE, "refusal took {took:?}");
    // The FIFO itself is untouched and no temp file was left beside it.
    assert!(fs::symlink_metadata(&fifo).unwrap().file_type().is_fifo());
    assert!(leftovers(&ws.root, "pipe").is_empty());
}

/// A directory target is refused without blocking: replacing a directory by a file is not
/// a thing the tool does, and it must be a refusal rather than a confusing later error.
#[test]
fn directory_target_is_refused_promptly() {
    let ws = Ws::new();
    let sub = ws.abs("adir");
    fs::create_dir(&sub).unwrap();
    let rel_sub = "adir".to_string();

    let (e, took) = refused_promptly("directory target", &ws, &rel_sub);
    assert_eq!(e.code, ErrorCode::UnsupportedTarget, "{e}");
    assert!(
        e.message.contains("not a regular file") || e.message.contains("must be a regular file"),
        "the message must say what is wrong, without repeating the target's type: {}",
        e.message
    );
    eprintln!("directory target refused in {took:?}");
    assert!(took < REFUSAL_DEADLINE, "refusal took {took:?}");
    // The directory still exists: a refusal never removes the target.
    assert!(sub.is_dir());
    assert!(leftovers(&ws.root, "adir").is_empty());
}

/// A socket target is refused without blocking. Created with the portable `UnixListener`
/// rather than by hand, so the test needs no capability and no `mknodat`.
#[test]
fn socket_target_is_refused_promptly() {
    use std::os::unix::net::UnixListener;
    let ws = Ws::new();
    let sock = ws.abs("s.sock");
    let _listener = UnixListener::bind(&sock).unwrap();

    let (e, took) = refused_promptly("socket target", &ws, "s.sock");
    assert_eq!(e.code, ErrorCode::UnsupportedTarget, "{e}");
    assert!(
        e.message.contains("not a regular file") || e.message.contains("must be a regular file"),
        "the message must say what is wrong, without repeating the target's type: {}",
        e.message
    );
    eprintln!("socket target refused in {took:?}");
    assert!(took < REFUSAL_DEADLINE, "refusal took {took:?}");
    // The socket is still there.
    assert!(fs::symlink_metadata(&sock).unwrap().file_type().is_socket());
    assert!(leftovers(&ws.root, "s.sock").is_empty());
}

/// A character device is refused too, on the same reasoning: it is not a regular file and
/// it must never be written through.
#[test]
fn character_device_target_is_refused_promptly() {
    let ws = Ws::new();
    // Opening /dev/null is a portable way to obtain a real character device without
    // mknod; a hard link or bind of it is not portable, so the device is checked in place
    // and only skipped when this platform has no /dev/null.
    let Ok(m) = fs::metadata("/dev/null") else {
        eprintln!("SKIPPED character device: no /dev/null on this platform");
        return;
    };
    if !m.file_type().is_char_device() {
        eprintln!("SKIPPED character device: /dev/null is not a character device here");
        return;
    }
    let _ = m;
    // F-01b: `/dev/null` is outside the workspace, so the write is refused before the target is
    // ever examined. Before the fix this path reached `atomic_replace`, which would have reported
    // "not a regular file" — proving it had opened and inspected a file outside the boundary.
    let forged = opencrayast_core::boundary::ResolvedPath {
        rel: "../../dev/null".into(),
        abs: Path::new("/dev/null").to_path_buf(),
    };
    let started = std::time::Instant::now();
    let e = ws
        .boundary
        .replace_file(&forged, b"x")
        .expect_err("a device outside the workspace must be refused");
    let took = started.elapsed();
    eprintln!("character device target refused in {took:?}");
    assert_eq!(e.code, ErrorCode::OutsideWorkspace, "{e}");
    assert!(took < REFUSAL_DEADLINE, "refusal took {took:?}");
    // Nothing was written to the device, and nothing was created beside it either.
    assert!(fs::metadata("/dev/null").is_ok());
    assert!(ws.temp_leftovers().is_empty());
}
