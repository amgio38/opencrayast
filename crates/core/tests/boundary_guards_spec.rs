//! Boundary guards that a mutation sweep found individually removable with the whole suite
//! green.
//!
//! A guard no test can fail is a comment. Every test here names the guard it pins, and every
//! one was proved by re-applying that guard's disable mutation and watching *this* test go
//! red. **Four guards from that sweep produced no test, because four mutations stayed green
//! against every assertion that could reasonably be written for them** — the file documents
//! each in `UNREACHABLE_*` below. A test that survives its own mutation is worse than no test,
//! so those were dropped rather than kept in a weakened form.
//!
//! Two shapes recur among the ones that could be pinned:
//!
//! * A guard whose *effect* is also produced by a second layer (B-01, B-01b, B-04, B-07). The
//!   refusal still happens when it is deleted, but with different wording or from a different
//!   place. Those tests assert the wording or the place, never merely `is_err()`.
//! * A guard nothing else stands in for (B-09, B-18, B-19). There the mutation removes the
//!   behaviour outright, so the assertion can be about the behaviour.
//!
//! The B-09 guard was dead and was made reachable by a one-line change in `resolve_write`; it
//! is the only source change in scope here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]

use opencrayast_core::ErrorCode;
use opencrayast_core::ToolError;
use opencrayast_core::boundary::*;
use opencrayast_core::limits::Limits;
use std::fs;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

// The content these tests write. Never empty, so no assertion can pass on an empty file.
const CONTENT: &str = "boundary-guard-content";

// A boundary over `root`: no read roots, no state dir, default limits.
fn boundary(root: &Path) -> Boundary {
    Boundary::new(BoundaryConfig::new(root, Limits::default())).expect("a valid workspace root")
}

// Every spelling that normalises to the workspace root itself.
fn root_spellings(root: &Path) -> Vec<String> {
    vec![
        ".".to_string(),
        "./".to_string(),
        ".//".to_string(),
        "a/..".to_string(),
        root.to_str().unwrap().to_string(),
        format!("{}/.", root.display()),
    ]
}

// ── configuration guards: `check_root` and `Boundary::new` ────────────────────────────────

// B-01 — an empty workspace root is refused, by the check that names it.
//
// `Boundary::new` has an empty check for the workspace root; `check_root` has a second one
// for every root. They differ by a word — "the workspace root" against "a root directory" —
// and if the workspace one is deleted an empty root is *still* refused, by the other one, with
// the other message. `is_err()` would pass either way; the wording pins this guard.
#[test]
fn b01_empty_workspace_root_is_refused_with_the_workspace_wording() {
    let e = Boundary::new(BoundaryConfig::new("", Limits::default()))
        .expect_err("an empty workspace root must be refused");
    assert_eq!(e.code, ErrorCode::InvalidArgs);
    assert_eq!(e.message, "The workspace root is not set.");
}

// B-01b — an empty READ root is refused by `check_root`, before it is canonicalised.
//
// The other direction of the same pair. Delete `check_root`'s empty check and an empty read
// root is still refused — the `canonicalize` fails first — but as "does not exist or cannot be
// read", which tells an operator something different and wrong about their configuration.
#[test]
fn b01b_empty_read_root_is_refused_before_it_is_canonicalised() {
    let ws = tempfile::tempdir().unwrap();
    let e = Boundary::new(BoundaryConfig {
        root: ws.path().to_path_buf(),
        limits: Limits::default(),
        read_roots: vec![PathBuf::new()],
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .expect_err("an empty read root must be refused");
    assert_eq!(e.code, ErrorCode::InvalidArgs);
    assert_eq!(e.message, "A root directory is not set.");
}

// B-04 — a root that is not a directory is refused, as the workspace root and as a read root.
//
// `Boundary::new` opens every root with `O_DIRECTORY` right after validating it, so deleting
// the `is_dir` check does not let a file become a root: the open fails instead, and the
// operator is told "a root directory could not be opened" rather than "a root is not a
// directory". Still a refusal, so the test has to say which refusal it is pinning.
#[test]
fn b04_a_root_that_is_not_a_directory_is_refused_with_the_directory_wording() {
    let ws = tempfile::tempdir().unwrap();
    let file = ws.path().join("a-file");
    fs::write(&file, CONTENT).unwrap();

    let e = Boundary::new(BoundaryConfig::new(file.clone(), Limits::default()))
        .expect_err("a file must be refused as the workspace root");
    assert_eq!(e.code, ErrorCode::InvalidArgs);
    assert_eq!(e.message, "A root is not a directory.");

    let real_root = ws.path().join("root");
    fs::create_dir(&real_root).unwrap();
    let e = Boundary::new(BoundaryConfig {
        root: real_root,
        limits: Limits::default(),
        read_roots: vec![file],
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .expect_err("a file must be refused as a read root too");
    assert_eq!(e.code, ErrorCode::InvalidArgs);
    assert_eq!(e.message, "A root is not a directory.");
}

// Env var the parent sets when it re-runs this binary with `HOME` pointed at a fixture.
const HOME_HELPER_ENV: &str = "OPENCRAYAST_BND_HOME_HELPER";

// The credential directories `check_root` refuses as read roots (T-32 / CFG-07).
const CREDENTIAL_DIR_NAMES: &[&str] = &[".ssh", ".gnupg", ".aws", ".config/gcloud"];

// Helper for B-07. Inert unless the parent re-runs this binary with a fixture `HOME`.
//
// A separate process because the rule is `same_dir(candidate, home.join(name))` and `home` is
// `HOME`, a process-wide variable. Setting it from a test thread would be a data race with
// every other boundary test in this binary — and `std::env::set_var` is `unsafe` in edition
// 2024 for exactly this reason. The fixture home is established by the child alone.
#[test]
fn b07_helper() {
    let Ok(case) = std::env::var(HOME_HELPER_ENV) else {
        return;
    };
    let mut parts = case.split('|');
    let ws = PathBuf::from(parts.next().unwrap());
    let home = PathBuf::from(parts.next().unwrap());
    let name = parts.next().unwrap();
    let built = Boundary::new(BoundaryConfig {
        root: ws,
        limits: Limits::default(),
        read_roots: vec![home.join(name)],
        state_dir: None,
        extra_protected: Vec::new(),
    });
    match built {
        Ok(_) => println!("HELPER-ACCEPTED"),
        Err(e) => println!("HELPER-REFUSED {e:?}"),
    }
}

// Run the helper with `HOME` pointed at `home`; return what it printed.
fn run_with_home(ws: &Path, home: &Path, name: &str) -> String {
    let exe = std::env::current_exe().expect("test binary path");
    let out = std::process::Command::new(exe)
        .args(["--exact", "b07_helper", "--nocapture"])
        .env("HOME", home)
        .env(
            HOME_HELPER_ENV,
            format!("{}|{}|{}", ws.display(), home.display(), name),
        )
        .output()
        .expect("the helper re-run must start");
    assert!(
        out.status.success(),
        "helper failed for {name}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// B-07 — `.ssh`, `.gnupg`, `.aws` and gcloud credentials are never accepted as read roots.
//
// Nothing else in the suite ever configures one, so the whole T-32 rule was unwatched. The
// control case is what gives the four refusals meaning: an ordinary directory under the very
// same fixture home is still accepted, so what is being refused is the credential rule and not
// "this home is not real".
#[test]
fn b07_credential_directories_are_refused_as_read_roots() {
    let ws = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    for name in CREDENTIAL_DIR_NAMES {
        fs::create_dir_all(home.path().join(name)).unwrap();
    }
    fs::create_dir(home.path().join("ordinary-project")).unwrap();

    for name in CREDENTIAL_DIR_NAMES {
        let out = run_with_home(ws.path(), home.path(), name);
        assert!(
            out.contains("HELPER-REFUSED"),
            "{name} must be refused as a read root, got: {out}"
        );
        assert!(
            out.contains("credential directory"),
            "{name} must be refused by the credential rule itself, got: {out}"
        );
        assert!(
            out.contains("InvalidArgs"),
            "a configuration refusal, not an I/O one: {out}"
        );
    }

    let control = run_with_home(ws.path(), home.path(), "ordinary-project");
    assert!(
        control.contains("HELPER-ACCEPTED"),
        "an ordinary directory under the same home is not a credential directory and must stay a \
         legal read root, or these four refusals prove nothing: {control}"
    );
}

// ── B-09: the write policy ────────────────────────────────────────────────────────────────

// B-09 — writing the workspace root is refused by the rule that names it.
//
// This guard was **dead**, and the sweep was right to call it that. It tested
// `r.lexical.as_os_str().is_empty()`, but `lexical` is built from the root (or `/`) with the
// input's components appended, so it is never empty — no spelling of the root could ever
// satisfy it, and every one of them fell through to the regular-file check and was reported as
// "must be a regular file, not a directory". True of the root, and no answer to the operator's
// actual question.
//
// **Made reachable, not deleted.** `resolve_write` now judges `r.canonical == self.root`
// instead — one line, in the same place, and it is the form that actually names the root
// rather than a path that can never be empty. Deleting the guard was the other option and it
// is worse: it leaves `.` misreported and throws away a rule that is correct.
//
// This is the only source change in this file's scope, and it is a behaviour change: the
// refusal `resolve_write(".")` produces is now this wording. That is the point of pinning it,
// and the mutation proof below is that with the new comparison removed the caller is back to
// the directory message and this test fails.
#[test]
fn b09_writing_the_workspace_root_is_refused_by_its_own_rule() {
    let ws = tempfile::tempdir().unwrap();
    let b = boundary(ws.path());
    for spelling in root_spellings(ws.path()) {
        let e = b.resolve_write(&spelling).err().unwrap_or_else(|| {
            panic!("{spelling} resolved, but the workspace root is not a write target")
        });
        assert_eq!(
            e.message, "A write target must be a file, not the workspace root.",
            "{spelling} must be refused by B-09's own rule, not by the regular-file check"
        );
        assert_eq!(e.code, ErrorCode::UnsupportedTarget);
    }
    // The rule is about writing. Reading the root stays legal and stays labelled `.`.
    assert_eq!(b.resolve_read(".").unwrap().rel, ".");
}

// ── B-18 / B-19: the handle `open_read` returns ────────────────────────────────────────────

// B-18 — the identity `open_read` returns is the identity the filesystem reports for the
// canonical path.
//
// BND-07 has two halves. The refusal half — a mismatched identity is rejected — is what the
// existing suite pins. The *comparison* itself was unobserved: it could be inverted, have one
// side zeroed, or compare a constant against itself, and nothing would notice. So this pins
// the positive case, and the second half matters more than the first: a mutation that made
// `open_read` return a fixed identity would pass a single-file assertion only by accident,
// and fails immediately once two files must disagree.
#[test]
fn b18_the_identity_returned_by_open_read_is_the_canonical_path_identity() {
    let ws = tempfile::tempdir().unwrap();
    fs::write(ws.path().join("f.txt"), CONTENT).unwrap();
    fs::write(ws.path().join("g.txt"), CONTENT).unwrap();
    let b = boundary(ws.path());

    let first = b.resolve_read("f.txt").unwrap();
    let (_f1, id1) = b.open_read(&first).expect("a regular file must open");
    use std::os::unix::fs::MetadataExt;
    let on_disk = fs::metadata(&first.abs).expect("metadata of the canonical path");
    assert_eq!(id1.dev, on_disk.dev(), "device from the opened handle");
    assert_eq!(id1.ino, on_disk.ino(), "inode from the opened handle");

    let second = b.resolve_read("g.txt").unwrap();
    let (_f2, id2) = b.open_read(&second).expect("a regular file must open");
    assert_ne!(
        (id2.dev, id2.ino),
        (id1.dev, id1.ino),
        "two distinct files must not report one identity: the comparison being pinned is a \
         comparison, not a constant"
    );
}

// How long an open on a FIFO may take before it counts as a hang rather than a slow refusal.
const OPEN_DEADLINE: Duration = Duration::from_secs(10);

// B-19 — `O_NONBLOCK` is cleared on the descriptor `open_read` hands back.
//
// The flag is requested on every open (`last_component_flags`) so that a FIFO turns into a
// refusal instead of a wait, and without being cleared it would stay set on the returned `File`
// for the rest of the caller's life, leaking into anything that later polls the descriptor.
// Regular files ignore it, so nothing reads it back — only the descriptor's own flags show
// whether it was cleared, and that is what this reads.
//
// The FIFO half is the deadline: if the `O_NONBLOCK` request ever stops being sent, an open on a
// FIFO waits for a writer forever. Without a bound that hangs the suite; with one it fails
// this test. B-43 is already load-bearing for the same property and is not duplicated here —
// this asserts the *clear*, which B-43's mutation cannot touch.
#[test]
fn b19_open_read_clears_nonblock_on_the_handle_it_returns() {
    let ws = tempfile::tempdir().unwrap();
    fs::write(ws.path().join("f.txt"), CONTENT).unwrap();
    let b = boundary(ws.path());
    let resolved = b.resolve_read("f.txt").unwrap();

    let (file, _identity) = b.open_read(&resolved).expect("a regular file must open");
    use std::os::fd::AsFd;
    let flags = rustix::fs::fcntl_getfl(file.as_fd()).expect("fcntl_getfl on an open file");
    assert!(
        !flags.contains(rustix::fs::OFlags::NONBLOCK),
        "the descriptor handed back must not still be non-blocking: {flags:?}"
    );

    // The flag really is requested on the way in, so this is not a flag that was never set:
    // a FIFO through the same path refuses promptly instead of blocking (BND-22).
    let fifo = ws.path().join("pipe");
    if make_fifo(&fifo) {
        let r = b.resolve_read("pipe").unwrap();
        let (e, took) = refused_within(b.open_read(&r), "a FIFO must be refused, not opened");
        assert_eq!(e.code, ErrorCode::IoError);
        assert!(e.message.contains("special file"), "{}", e.message);
        println!("FIFO refused in {took:?}");
    }
}

// Create a FIFO, or report why the case must be dropped. Never opens it: an open blocks.
fn make_fifo(path: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        let Ok(c) = std::ffi::CString::new(path.to_str().unwrap()) else {
            println!("SKIPPED: the FIFO path is not representable");
            return false;
        };
        if rustix::fs::mknodat(
            rustix::fs::CWD,
            c.as_c_str(),
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )
        .is_err()
        {
            println!("SKIPPED: this host cannot create a FIFO");
            return false;
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let Ok(s) = std::process::Command::new("mkfifo").arg(path).status() else {
            println!("SKIPPED: mkfifo is unavailable");
            return false;
        };
        if !s.success() {
            println!("SKIPPED: mkfifo failed");
            return false;
        }
    }
    use std::os::unix::fs::FileTypeExt;
    match fs::symlink_metadata(path) {
        Ok(md) if md.file_type().is_fifo() => true,
        _ => {
            println!("SKIPPED: the node is not a FIFO here");
            false
        }
    }
}

// The refusal from `result`, plus how long it took — and a named failure instead of a hang if
// it never comes back.
fn refused_within<T: Send + 'static>(
    result: Result<T, ToolError>,
    what: &str,
) -> (ToolError, Duration) {
    let started = Instant::now();
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), ToolError>>();
    std::thread::spawn(move || {
        let _ = tx.send(result.map(|_| ()));
    });
    let outcome: Result<(), ToolError> = rx.recv_timeout(OPEN_DEADLINE).unwrap_or_else(|_| {
        panic!("{what}: no answer within {OPEN_DEADLINE:?} — that is a hang, not a slow refusal")
    });
    let took = started.elapsed();
    match outcome {
        Ok(()) => panic!("{what}: the call succeeded, which is the bug this pins"),
        Err(e) => (e, took),
    }
}

// ── guards that no test here pins, and why ─────────────────────────────────────────────────

// **B-02 is unobservable, not unwatched.** The sweep's reading was that dropping the read-root
// dedup would let the workspace appear in the root list twice and relabel every path under it
// `@root1/...`. Measured, it does not: `resolve_existing` finds the owning root with
// `(0..=self.read_roots.len()).find(...)`, which visits index 0 — the workspace — before any
// read root, so a duplicate that sits at index 1 is never selected and the label never moves.
// Removing the dedup leaves the workspace's label intact for every path, the control case
// unchanged, and this suite green. The dedup is worth keeping (it saves a descriptor and stops
// a redundant root from being validated), but its removal is not observable, so no test can
// witness it. What *would* witness it is a unit-level assertion on the stored root list, which
// `read_roots` is private to the crate and this file is not; an in-crate test could assert the
// length, and that is the honest recommendation rather than a fake assertion here.
//
//
// **B-40 / B-41 / B-42 are unreachable, not unwatched.** The sweep's route was "open a
// symlinked path without going through `resolve_read` — a hand-built `ResolvedPath`". Measured,
// that does not reach the flags either. `open_read` re-proves containment from `p.abs` by
// canonicalising it and opening the *canonical* relative path, so by the time any flag is
// consulted the link is already expanded and `O_NOFOLLOW` / `NO_SYMLINKS` / `BENEATH` have
// nothing left to refuse: a hand-built path through an in-workspace directory symlink opens
// successfully, and a 200 000-attempt race flipping that link in and out while `open_read` runs
// recorded 199 988 successful opens and zero symlink refusals. The three removals are safe only
// because B-21's re-proof runs first, and that ordering is why the whole set stays green.
//
// The flags remain correct — they are defence against a window narrower than this suite can
// schedule, and `walk_components` shows the guarantee is deliberate rather than accidental. But
// a test for them can only be a race that never wins, which is not a test. The property worth
// pinning is the one that makes the window narrow, and B-21 already is. If B-21's re-proof is
// ever removed, these three stop being unreachable and they will need tests then.
