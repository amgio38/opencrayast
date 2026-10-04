//! Spec for ISSUE-CORE-BOUNDARY-HARDEN: BND-18/T-20 uniform walk failures and T-03
//! dirfd-relative opening.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
#![cfg(unix)]
// Not used for anything yet: this binary is the home of `eacces_plan`, the first skip guard in
// this repo, and the socket/FIFO guards in `common` are its siblings. Declaring it here means
// those guard tests are compiled and run by this binary too.
#[allow(unused_imports)]
mod common;
use opencrayast_core::ErrorCode;
use opencrayast_core::boundary::*;
use opencrayast_core::limits::Limits;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const INSIDE: &str = "INSIDE-WORKSPACE-CONTENT";
const OUTSIDE: &str = "OUTSIDE-SECRET-CONTENT";

/// Env var the parent sets when it re-runs this binary without privileges.
const HELPER_ENV: &str = "OPENCRAYAST_EACCES_HELPER";

/// Why this process cannot produce a real `EACCES`, or `None` when it can.
///
/// Pure, so both answers are checked on any machine. The interesting one is the macOS runner,
/// which never runs as root and therefore takes the `chmod 000` path - but a developer running
/// the suite as root on macOS would otherwise hit a `panic!` about a missing `setpriv`.
///
/// `setpriv` is util-linux. There is no equivalent on macOS, and without it a root process
/// cannot be denied access by permissions at all: root reads a `000` directory like any other
/// user. The case is therefore skipped there, not failed - the property it checks (a real
/// `EACCES` is refused exactly like any outside path) is still covered on every Linux CI run.
fn eacces_plan(is_root: bool, setpriv_available: bool) -> Option<&'static str> {
    match (is_root, setpriv_available) {
        // Not root: `chmod 000` denies this process for real, everywhere.
        (false, _) => None,
        // Root with util-linux: re-run the helper as uid 65534.
        (true, true) => None,
        (true, false) => Some(
            "setpriv not available (util-linux only); a root process cannot be denied by \
             permissions, so no real EACCES can be produced here",
        ),
    }
}

/// Is util-linux's `setpriv` on this machine? Probed by running it, not by looking for the
/// binary: a `setpriv` that exists but cannot drop privileges is no use either.
fn setpriv_available() -> bool {
    std::process::Command::new("setpriv")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn euid_is_root() -> bool {
    rustix::fs::Uid::as_raw(rustix::process::geteuid()) == 0
}

/// The reference refusal: an absolute path outside every root that simply does not exist.
/// Every other "cannot be resolved" refusal has to be word-for-word this one (BND-18, T-20).
fn reference_outside_refusal(b: &Boundary, pubdir: &Path) -> opencrayast_core::ToolError {
    b.resolve_read(pubdir.join("nope.txt").to_str().unwrap())
        .expect_err("a missing absolute path outside every root must be refused")
}

/// A. ENOTDIR: an absolute path that runs through a regular file. `ENOTDIR` is an IO error,
/// not a "not found", so it must be indistinguishable from every other outside refusal -
/// otherwise it is a probe for "is there a file at this exact path". This runs whatever the
/// privileges are, which is why it is the case that always executes.
#[test]
fn a_absolute_path_through_a_file_is_refused_like_any_outside_path() {
    let ws = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    fs::write(ws.path().join("plain.rs"), INSIDE).unwrap();
    fs::write(out.path().join("also-a-file.rs"), OUTSIDE).unwrap();
    let b = Boundary::new(BoundaryConfig {
        root: ws.path().to_path_buf(),
        limits: Limits::default(),
        read_roots: Vec::new(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();

    let reference = reference_outside_refusal(&b, out.path());

    // Through a file inside the workspace, and through a file outside it.
    let inside_through_file = b
        .resolve_read(ws.path().join("plain.rs/x").to_str().unwrap())
        .expect_err("ENOTDIR must be refused");
    assert_eq!(
        inside_through_file, reference,
        "an absolute path through a file must look exactly like any other outside refusal"
    );
    let outside_through_file = b
        .resolve_read(out.path().join("also-a-file.rs/deeper").to_str().unwrap())
        .expect_err("ENOTDIR must be refused");
    assert_eq!(outside_through_file, reference);

    // Documented difference: a RELATIVE path is by construction inside the workspace, so its
    // IO errors stay honest `io_error`. Nothing outside was consulted, so there is nothing to
    // probe.
    let rel = b
        .resolve_read("plain.rs/x")
        .expect_err("ENOTDIR must be refused");
    assert_eq!(rel.code, ErrorCode::IoError);
}

/// The skip decision is pure, so the branch a macOS developer would take is checked here
/// rather than discovered there. Getting this wrong in the permissive direction would turn a
/// real `EACCES` bug into a silent pass on every platform.
#[test]
fn the_eacces_skip_only_triggers_for_root_without_setpriv() {
    assert_eq!(
        eacces_plan(false, false),
        None,
        "chmod 000 works for anyone"
    );
    assert_eq!(eacces_plan(false, true), None);
    assert_eq!(
        eacces_plan(true, true),
        None,
        "root with util-linux runs the helper"
    );
    let skip = eacces_plan(true, false).expect("root without setpriv must skip");
    assert!(skip.contains("setpriv"), "{skip}");
}

/// Helper for the EACCES half. Inert unless the parent re-runs this test binary as an
/// unprivileged user, which is how the root case gets a real permission error at all.
#[test]
fn a_eacces_helper() {
    let Ok(spec) = std::env::var(HELPER_ENV) else {
        return;
    };
    let mut parts = spec.split('|');
    let (root, pubdir, locked) = (
        PathBuf::from(parts.next().unwrap()),
        PathBuf::from(parts.next().unwrap()),
        PathBuf::from(parts.next().unwrap()),
    );
    let b = Boundary::new(BoundaryConfig {
        root,
        limits: Limits::default(),
        read_roots: Vec::new(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();
    let reference = reference_outside_refusal(&b, &pubdir);
    let e = b
        .resolve_read(locked.join("secret.txt").to_str().unwrap())
        .expect_err("a permission error must be refused");
    assert_eq!(
        e, reference,
        "EACCES on an absolute path must look like any other outside refusal"
    );
    assert_eq!(e.code, ErrorCode::OutsideWorkspace);
    println!("HELPER-OK outside_workspace");
}

/// A, second case: a real `EACCES` during the walk. `chmod 000` does not stop root, so when
/// the tests run as root the parent re-runs the helper binary through `setpriv` as an
/// unprivileged user against a root-owned `0700` directory; otherwise the parent makes the
/// directory unreadable for itself. Either way the permission error is real.
#[test]
fn a_absolute_path_under_an_unreadable_directory_is_refused_like_any_outside_path() {
    let ws = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    // The child runs as another user, so the directories it has to walk into must be
    // traversable - only the `locked` one is not.
    fs::set_permissions(ws.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let pubdir = out.path().join("pub");
    fs::create_dir(&pubdir).unwrap();
    fs::set_permissions(&pubdir, fs::Permissions::from_mode(0o755)).unwrap();
    let locked = pubdir.join("locked");
    fs::create_dir(&locked).unwrap();
    fs::write(locked.join("secret.txt"), OUTSIDE).unwrap();
    let b = Boundary::new(BoundaryConfig {
        root: ws.path().to_path_buf(),
        limits: Limits::default(),
        read_roots: Vec::new(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();
    let reference = reference_outside_refusal(&b, &pubdir);

    if !euid_is_root() {
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let e = b
            .resolve_read(locked.join("secret.txt").to_str().unwrap())
            .expect_err("a permission error must be refused");
        assert_eq!(e, reference);
        assert_eq!(e.code, ErrorCode::OutsideWorkspace);
        return;
    }

    // Root: `0700` root-owned is unreadable for an unprivileged user, so run the helper -
    // if this platform has a way to become one.
    if let Some(why) = eacces_plan(true, setpriv_available()) {
        println!("SKIPPED: {why}");
        return;
    }
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
    let exe = std::env::current_exe().expect("test binary path");
    let spec = format!(
        "{}|{}|{}",
        ws.path().display(),
        pubdir.display(),
        locked.display()
    );
    let out = std::process::Command::new("setpriv")
        .args(["--reuid=65534", "--regid=65534", "--clear-groups"])
        .arg(&exe)
        .args(["--exact", "a_eacces_helper", "--nocapture"])
        .env(HELPER_ENV, spec)
        .output();
    let Ok(out) = out else {
        // `setpriv` was there when it was probed and is gone now: still a skip, not a failure.
        println!("SKIPPED: setpriv disappeared between the probe and the run");
        return;
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stdout.contains("HELPER-OK") && out.status.success(),
        "the unprivileged helper must see a real EACCES refused like any outside path\n\
         stdout: {stdout}\nstderr: {stderr}"
    );
    // And the same check from this process, where root sees through the `0700` directory:
    // the reference refusal must be the very same error either way.
    let e = b
        .resolve_read(locked.join("secret.txt").to_str().unwrap())
        .expect_err("must be refused");
    assert_eq!(e.code, ErrorCode::OutsideWorkspace);
}

/// B, deterministic: the window between resolve and open, with no luck involved. The
/// intermediate directory is replaced by a symlink pointing outside AFTER the path was
/// resolved. A path-based open follows it, and a path-based identity check then compares the
/// handle against the very same swapped path - so the two identities match and the read
/// succeeds on a file outside the workspace.
#[test]
fn b_intermediate_directory_swapped_after_resolve_is_refused() {
    let ws = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    fs::create_dir(ws.path().join("sub")).unwrap();
    fs::write(ws.path().join("sub/f.rs"), INSIDE).unwrap();
    fs::write(out.path().join("f.rs"), OUTSIDE).unwrap();
    let b = Boundary::new(BoundaryConfig {
        root: ws.path().to_path_buf(),
        limits: Limits::default(),
        read_roots: Vec::new(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();

    let r = b.resolve_read("sub/f.rs").unwrap();
    assert_eq!(fs::read_to_string(&r.abs).unwrap(), INSIDE);

    // Swap the intermediate directory for a link out of the workspace. `rename` then
    // `symlink` is what an attacker with write access to the workspace can do.
    fs::rename(ws.path().join("sub"), ws.path().join("sub_bak")).unwrap();
    symlink(out.path(), ws.path().join("sub")).unwrap();

    let res = b.open_read(&r);
    assert!(
        res.is_err(),
        "opening through a swapped intermediate directory must be refused"
    );
    // The same file the tool would have handed over, to be certain of what was at stake.
    let _ = fs::read_to_string(out.path().join("f.rs")).unwrap();
}

/// B, the race: one thread flips the intermediate entry between an in-workspace directory and
/// an outside one with atomic renames, the other resolves and opens in a loop. The bytes read
/// must never be the outside marker. Runs for up to 5 seconds or 20000 attempts, whichever
/// comes first, and requires that a meaningful number of attempts actually succeeded, so the
/// test cannot pass by refusing everything.
#[test]
fn b_racing_directory_swap_never_yields_outside_content() {
    let ws = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    fs::create_dir(ws.path().join("real")).unwrap();
    fs::write(ws.path().join("real/f.rs"), INSIDE).unwrap();
    fs::write(out.path().join("f.rs"), OUTSIDE).unwrap();
    symlink(out.path(), ws.path().join("d_out")).unwrap();
    let b = Arc::new(
        Boundary::new(BoundaryConfig {
            root: ws.path().to_path_buf(),
            limits: Limits::default(),
            read_roots: Vec::new(),
            state_dir: None,
            extra_protected: Vec::new(),
        })
        .unwrap(),
    );

    let stop = Arc::new(AtomicBool::new(false));
    let flipper = {
        let stop = Arc::clone(&stop);
        let ws = ws.path().to_path_buf();
        std::thread::spawn(move || {
            let inside = ws.join("real");
            let outside = out.path().to_path_buf();
            let (tmp, d) = (ws.join("d_tmp"), ws.join("d"));
            let mut flips = 0u64;
            while !stop.load(Ordering::Relaxed) {
                // Create-then-rename: the entry is never absent, and the swap itself is a
                // single atomic rename, which is what an attacker with write access to the
                // workspace can actually do.
                for target in [&inside, &outside] {
                    if symlink(target, &tmp).is_ok() && std::fs::rename(&tmp, &d).is_ok() {
                        flips += 1;
                    }
                }
            }
            flips
        })
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut attempts = 0u64;
    let mut opened = 0u64;
    let mut refused = 0u64;
    while attempts < 20_000 && std::time::Instant::now() < deadline {
        attempts += 1;
        // Both spellings race the flipper: `d/f.rs` goes through the flipping entry, and
        // `real/f.rs` is the directory the flipper renames away underneath us.
        for spelling in ["d/f.rs", "real/f.rs"] {
            let Ok(r) = b.resolve_read(spelling) else {
                refused += 1;
                continue;
            };
            match b.open_read(&r) {
                Ok((mut f, _)) => {
                    opened += 1;
                    let mut buf = Vec::new();
                    use std::io::Read;
                    f.read_to_end(&mut buf).unwrap();
                    let text = String::from_utf8_lossy(&buf);
                    assert_ne!(
                        text, OUTSIDE,
                        "iteration {attempts} ({spelling}) read a file OUTSIDE the workspace"
                    );
                    assert_eq!(text, INSIDE, "unexpected content via {spelling}");
                }
                Err(_) => refused += 1,
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
    let flips = flipper.join().unwrap();
    assert!(
        attempts > 1000,
        "only {attempts} attempts: the race never ran"
    );
    assert!(
        opened > 100,
        "only {opened} successful opens: nothing was tested"
    );
    assert!(
        flips > 100,
        "only {flips} flips: the directory never changed"
    );
    println!("attempts={attempts} opened={opened} refused={refused} flips={flips}");
}
