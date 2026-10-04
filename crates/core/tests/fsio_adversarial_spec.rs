//! Adversarial tests for `opencrayast_core::fsio` and the write path through `Boundary`.
//!
//! Since F-01b, `fsio::atomic_replace` is crate-private and takes a `&Boundary`; these tests reach
//! it only by building a real boundary over a temp workspace (`common::atomic::Ws`), never through a
//! bare path. The child-process helpers rebuild the same policy from the root passed in the
//! environment, because a boundary cannot cross a process boundary either.
//!
//! Tester only — no lasting edits under `crates/core/src`. Crash injection uses a
//! re-exec helper (`OPENCRAYAST_FSIO_ADV_HELPER`) killed with SIGKILL; no `unsafe`.
//!
//! ## Concurrent-replace observation (property 4)
//! Eight threads race `replace_file` on one path with distinct payloads. Since F-01b each call
//! observes the target's identity ITSELF rather than being handed one, so a racer that arrives
//! after another has already written sees the new file and legitimately succeeds: this is eight
//! independent atomic replaces, last writer wins, and MORE THAN ONE may succeed.
//!
//! What the tests here assert is therefore NOT "exactly one wins" (no longer true) but:
//! - the final content is exactly one complete payload, never a mixture — witnessed by
//!   `concurrent_payloads_are_distinguishable`, whose payloads are multi-byte patterns precisely so
//!   that a non-atomic write would produce content matching none of them; and
//! - no temp file is left behind.
//!
//! An earlier version of this file claimed the pre-rename recheck was pinned by
//! `fsio_harden_spec`; it was not, and CR F2 recorded the correction. The production-path claim is
//! now witnessed by `production_path_calls_the_pre_rename_recheck`.
#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]

use opencrayast_core::ErrorCode;
use opencrayast_core::limits::Limits;
mod common;

use common::atomic::Ws;
use opencrayast_core::fsio::fsync_dir;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::Duration;

const TEMP_MARK: &str = ".opencrayast-tmp-";
const HELPER_ENV: &str = "OPENCRAYAST_FSIO_ADV_HELPER";
const CRASH_ITERS: u32 = 200;
const SEED: u64 = 0xf510_c0de;

struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    fn next_usize(&mut self, n: usize) -> usize {
        (self.next_u64() as usize) % n
    }
}

fn temp_leftovers(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(TEMP_MARK))
        .collect()
}

fn all_names(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

fn assert_no_temps(dir: &Path) {
    let left = temp_leftovers(dir);
    assert!(
        left.is_empty(),
        "unexpected temp leftovers: {left:?} (all={:?})",
        all_names(dir)
    );
}

/// Property 1: random payloads round-trip; mode preserved; no temps.
#[test]
fn random_payloads_round_trip_preserve_mode_no_temps() {
    let mut rng = XorShift64::new(SEED);
    let cases: Vec<Vec<u8>> = vec![
        vec![],
        vec![0x42],
        b"a\0b\0c".to_vec(),
        "你好🌍 café".as_bytes().to_vec(),
        (0..8 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect(),
        // a few PRNG-sized blobs
        (0..rng.next_usize(4096) + 2)
            .map(|_| (rng.next_u64() & 0xff) as u8)
            .collect(),
    ];

    for (i, content) in cases.iter().enumerate() {
        let ws = Ws::new();
        let t = ws.abs("f.bin");
        ws.put("f.bin", b"OLD-PAYLOAD");
        fs::set_permissions(&t, fs::Permissions::from_mode(0o640)).unwrap();
        let before_mode = fs::metadata(&t).unwrap().mode() & 0o777;
        ws.replace("f.bin", content)
            .unwrap_or_else(|e| panic!("case {i}: {e}"));
        assert_eq!(fs::read(&t).unwrap(), *content, "case {i} bytes");
        assert_eq!(
            fs::metadata(&t).unwrap().mode() & 0o777,
            before_mode,
            "case {i} mode"
        );
        assert_no_temps(&ws.root);
    }
}

/// Property 2a: unwritable parent (skip when root/capability bypasses the mode bit).
#[test]
fn unwritable_parent_zero_residue() {
    let ws = Ws::new();
    let sub = ws.abs("ro");
    fs::create_dir(&sub).unwrap();
    let t = sub.join("f.txt");
    fs::write(&t, "old").unwrap();
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o555)).unwrap();
    let barrier_holds = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(sub.join(".opencrayast-probe"))
        .is_err();
    if !barrier_holds {
        eprintln!(
            "SKIP unwritable_parent: this uid bypasses directory write bits (often root); \
             cannot manufacture the failure here"
        );
        fs::set_permissions(&sub, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let result = ws.replace("ro/f.txt", b"new");
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(result.is_err(), "unwritable parent must fail");
    assert_eq!(fs::read_to_string(&t).unwrap(), "old");
    assert_no_temps(&sub);
}

mod helper_mode {
    pub const RLIMIT: &str = "rlimit_fsize";
    pub const CRASH: &str = "replace_and_sleep";
}

/// Property 2b: `RLIMIT_FSIZE` in a child forces a write failure; target unchanged, no temps.
#[test]
fn rlimit_fsize_child_fails_cleanly() {
    if std::env::var(HELPER_ENV).ok().as_deref() == Some(helper_mode::RLIMIT) {
        helper_rlimit_fsize();
        return;
    }

    let ws = Ws::new();
    let t = ws.abs("f.txt");
    ws.put("f.txt", b"keep-me");
    let exe = std::env::current_exe().unwrap();
    let out = Command::new(&exe)
        .env(HELPER_ENV, helper_mode::RLIMIT)
        .env("OPENCRAYAST_FSIO_ROOT", &ws.root)
        .args(["--exact", "rlimit_fsize_child_fails_cleanly", "--nocapture"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Linux may deliver SIGXFSZ. The libtest harness often surfaces that as exit
    // code 128+25=153 (`code()=Some(153)`, `signal()=None`) rather than a raw signal.
    let xfsz = out.status.signal() == Some(25) || out.status.code() == Some(153);
    // The child must have REACHED the workspace target before it died. Without this, a helper
    // pointed at a path that does not exist still "passes" — the SIGXFSZ kill or the early
    // `return`s hide it. `ws.root` is what the parent created the file in, so this is the
    // strongest available statement that the rlimit path was exercised on the real target.
    let touched_target =
        stdout.contains("HELPER_RLIMIT_TARGET") || stderr.contains("HELPER_RLIMIT_TARGET");
    assert!(
        touched_target,
        "the rlimit helper never reported reaching the target under `{}` — it resolved and \
         wrote somewhere else, or returned early. stdout={stdout} stderr={stderr}",
        ws.root.display()
    );
    assert!(
        !touched_target || stdout.contains("rel=f.txt") || stderr.contains("rel=f.txt"),
        "the helper reported a target that is not f.txt inside the workspace: stdout={stdout} stderr={stderr}"
    );
    assert!(
        out.status.success()
            || xfsz
            || stdout.contains("HELPER_RLIMIT_OK")
            || stderr.contains("HELPER_RLIMIT_OK")
            || stderr.contains("SKIP rlimit_fsize"),
        "rlimit helper failed: status={:?} code={:?} signal={:?} stdout={stdout} stderr={stderr}",
        out.status,
        out.status.code(),
        out.status.signal()
    );
    assert_eq!(fs::read_to_string(&t).unwrap(), "keep-me");
    // If the kernel raised SIGXFSZ, the process died inside `write_all` before the
    // `Err` cleanup ran — same residue class as crash injection. Conforming temps only.
    // The scan is over the WORKSPACE root, not the outer tempdir: the temp lives beside the
    // target it is replacing, and the outer tempdir also holds `state/`, so scanning it
    // proved nothing about this write. (CR: the helper read `dir.join("f.txt")` — a path
    // that never existed — and the residue scan inherited the same wrong level.)
    let temps = temp_leftovers(&ws.root);
    if xfsz {
        for name in &temps {
            assert!(
                name.starts_with(TEMP_MARK),
                "SIGXFSZ leftover must use the temp prefix: {name}"
            );
        }
        eprintln!(
            "rlimit_fsize: child hit SIGXFSZ (exit 153); target intact; \
             conforming temp residue count={}",
            temps.len()
        );
    } else {
        assert_no_temps(&ws.root);
    }
}

fn helper_rlimit_fsize() {
    let root = PathBuf::from(std::env::var("OPENCRAYAST_FSIO_ROOT").unwrap());
    // The target is the one the policy resolved, not a path rebuilt by hand: the earlier
    // `dir.join("f.txt")` pointed at the OUTER tempdir (which holds `ws/` and `state/`) and
    // therefore never existed, so the read-back assertion below was unreachable code.
    // Tiny file-size ceiling: writing a few KB of replacement must hit EFBIG.
    let lim = rustix::process::Rlimit {
        current: Some(64),
        maximum: Some(64),
    };
    if let Err(e) = rustix::process::setrlimit(rustix::process::Resource::Fsize, lim) {
        eprintln!("SKIP rlimit_fsize: setrlimit failed: {e:?}");
        println!("HELPER_RLIMIT_OK");
        return;
    }
    let big = vec![b'X'; 4096];
    // F-01b: the child rebuilds the policy rather than calling the primitive with a bare path.
    let Ok(boundary) = opencrayast_core::boundary::Boundary::new(
        opencrayast_core::boundary::BoundaryConfig::new(root.clone(), Limits::default()),
    ) else {
        return;
    };
    let Ok(resolved) = boundary.resolve_write("f.txt") else {
        return;
    };
    // Observable trace: the parent asserts on this line, so a helper that silently wrote
    // somewhere else (or never reached the target) turns the test red instead of passing by
    // accident. Everything after the trace can die on SIGXFSZ; this line is before the write.
    eprintln!(
        "HELPER_RLIMIT_TARGET rel={} abs={}",
        resolved.rel,
        resolved.abs.display()
    );
    let err = boundary
        .replace_file(&resolved, &big)
        .expect_err("rlimit must force failure");
    assert_eq!(err.code, ErrorCode::IoError, "{err}");
    // The read-back must be of the path the policy resolved. Comparing against the resolved
    // `abs` rather than a hand-built one is what makes this assertion mean something.
    assert_eq!(
        fs::read_to_string(&resolved.abs).unwrap(),
        "keep-me",
        "the target under RLIMIT_FSIZE must be untouched: {}",
        resolved.abs.display()
    );
    assert!(
        temp_leftovers(&root).is_empty(),
        "{:?}",
        temp_leftovers(&root)
    );
    println!("HELPER_RLIMIT_OK");
}

/// Property 3: identity / race refusals leave content alone and leave no temps.
#[test]
fn identity_and_race_refusals() {
    // stale identity
    {
        let ws = Ws::new();
        ws.put("f.txt", b"old");
        let aside = ws.abs("aside.txt");
        fs::rename(ws.abs("f.txt"), &aside).unwrap();
        ws.put("f.txt", b"other");
        // F-01b: the identity is observed inside the call now, so "stale identity" as a caller-
        // supplied value is no longer expressible. What remains is that the content on disk is the
        // one that is written — the foreign content is replaced deliberately, never silently lost
        // by writing to a path the caller misremembered.
        ws.replace_ok("f.txt", b"mine");
        assert_eq!(ws.read("f.txt"), b"mine");
        assert!(ws.temp_leftovers().is_empty());
    }
    // target renamed away (deleted from path)
    {
        let ws = Ws::new();
        ws.put("f.txt", b"old");
        fs::remove_file(ws.abs("f.txt")).unwrap();
        // The path no longer resolves for a write, so the policy refuses before the primitive.
        let e = ws.replace("f.txt", b"new").unwrap_err();
        assert!(
            e.code == ErrorCode::NotFound || e.code == ErrorCode::OutsideWorkspace,
            "{e}"
        );
        assert!(!ws.abs("f.txt").exists());
        assert!(ws.temp_leftovers().is_empty());
    }
    // becomes hard link
    {
        let ws = Ws::new();
        ws.put("f.txt", b"old");
        fs::hard_link(ws.abs("f.txt"), ws.abs("h.txt")).unwrap();
        let e = ws.replace("f.txt", b"new").unwrap_err();
        assert_eq!(e.code, ErrorCode::UnsupportedTarget, "{e}");
        assert_eq!(ws.read("f.txt"), b"old");
        assert!(ws.temp_leftovers().is_empty());
    }
    // becomes symlink
    {
        let ws = Ws::new();
        ws.put("real.txt", b"payload");
        ws.put("f.txt", b"old");
        fs::remove_file(ws.abs("f.txt")).unwrap();
        symlink(ws.abs("real.txt"), ws.abs("f.txt")).unwrap();
        assert!(ws.replace("f.txt", b"new").is_err());
        assert_eq!(
            ws.read("real.txt"),
            b"payload",
            "the real file is untouched"
        );
        assert!(ws.temp_leftovers().is_empty());
    }
}

/// Property 4: eight concurrent replaces of one path.
///
/// CTO review: this test used to be called `concurrent_eight_threads_one_winner_intact` and to
/// assert `fails >= 1 && wins.len() < 8`. That assertion was a function of SCHEDULING, not of
/// correctness — the same binary passed 11/11 unloaded (2–6 successes) and failed once under a
/// full-workspace load (8 successes). A test that goes red when the machine is busy makes main's
/// green light untrustworthy, so the load-dependent assertion is GONE rather than retuned.
///
/// What replaces it:
/// - "the file is never a mixture" is asserted deterministically by
///   `concurrent_payloads_are_distinguishable`, whose payloads are multi-byte patterns so a
///   non-atomic write cannot match any of them;
/// - "the pre-rename recheck really runs in the production path" is asserted deterministically by
///   `production_path_calls_the_pre_rename_recheck`, which uses the crate-private seam rather than
///   a race. That is the guarantee the old assertion was reaching for.
///
/// So the recheck is KEPT (CR R1 option (a)) and this test now only checks what concurrency itself
/// must guarantee: no panic, no temp residue, and a whole-file result.
#[test]
fn concurrent_eight_threads_leave_one_whole_payload() {
    let ws = Arc::new(Ws::new());
    ws.put("f.txt", b"BASE");
    let barrier = Arc::new(Barrier::new(8));
    let results = Arc::new(Mutex::new(Vec::new()));
    let mut handles = Vec::new();
    for i in 0..8u8 {
        let ws = Arc::clone(&ws);
        let barrier = Arc::clone(&barrier);
        let results = Arc::clone(&results);
        let payload = vec![b'A' + i; 256];
        handles.push(thread::spawn(move || {
            barrier.wait();
            let r = ws.replace("f.txt", &payload).map_err(|e| e.to_string());
            results.lock().unwrap().push((i, r.is_ok(), payload));
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let rows = results.lock().unwrap().clone();
    let wins: Vec<_> = rows.iter().filter(|(_, ok, _)| *ok).collect();
    let fails = rows.len() - wins.len();
    assert!(
        !wins.is_empty(),
        "expected at least one success; rows={rows:?}"
    );
    // CR F2/F3, honestly restated. The previous version of this comment claimed the pre-rename
    // recheck was "pinned directly in fsio_harden_spec ... so dropping it is still detectable".
    // That was FALSE, and the reviewer proved it: removing the `recheck_target(...)` call from
    // `write_and_replace` left every test green, because `fsio_harden_spec` calls `recheck_target`
    // DIRECTLY and so pins the function's behaviour, never whether the production path calls it.
    //
    // Two separate claims, each with its own witness:
    //
    // 1. "some racers must fail" is NO LONGER TRUE and is not asserted. Before F-01b every racer was
    //    handed one shared identity, so a late racer compared a stale value and lost. Each call now
    //    observes the identity itself, so a racer entering after another has written legitimately
    //    sees the new file and succeeds: eight independent atomic replaces, last writer wins. That
    //    is correct semantics for independent callers.
    // 2. "the file is never a mixture" IS asserted, by `concurrent_payloads_are_distinguishable`:
    //    that test's payloads are many-byte patterns, so a non-atomic write (copy, truncate+write)
    //    produces content that is not one of them and is caught. A 256-byte single-byte payload
    //    cannot witness that — which is why the old assertion here was unwitnessed (CR F3).
    let _ = fails;
    eprintln!(
        "concurrent_eight: successes={} failures={} (per-call identity ⇒ all may succeed)",
        wins.len(),
        fails
    );
    let final_bytes = ws.read("f.txt");
    assert!(
        rows.iter().any(|(_, ok, p)| *ok && p == &final_bytes)
            || rows.iter().any(|(_, _, p)| p == &final_bytes),
        "final content must equal one complete payload; got len={}",
        final_bytes.len()
    );
    // Prefer the stronger statement when we have recorded winners:
    if !wins.is_empty() {
        assert!(
            wins.iter().any(|(_, _, p)| p == &final_bytes),
            "final content must be one of the successful payloads"
        );
    }
    assert_no_temps(&ws.root);
}

/// Property 5: crash / SIGKILL during replace — target is full old or full new only.
#[test]
fn crash_sigkill_during_replace_is_atomic() {
    if std::env::var(HELPER_ENV).ok().as_deref() == Some(helper_mode::CRASH) {
        helper_replace_and_sleep();
        return;
    }

    let mut rng = XorShift64::new(SEED ^ 0x9e37);
    let exe = std::env::current_exe().unwrap();
    let mut temp_residue_total = 0usize;
    let mut saw_old = 0u32;
    let mut saw_new = 0u32;

    for i in 0..CRASH_ITERS {
        let ws = Ws::new();
        let d = ws.dir.path().to_path_buf();
        let t = ws.abs("f.txt");
        let old = format!("OLD-{i}-{}", "x".repeat(64));
        let new = format!("NEW-{i}-{}", "y".repeat(64));
        ws.put("f.txt", old.as_bytes());
        fs::write(d.join("new.bin"), &new).unwrap();

        let mut child = Command::new(&exe)
            .env(HELPER_ENV, helper_mode::CRASH)
            .env("OPENCRAYAST_FSIO_DIR", &d)
            .env("OPENCRAYAST_FSIO_ROOT", &ws.root)
            .env("OPENCRAYAST_FSIO_REL", "f.txt")
            .env("OPENCRAYAST_FSIO_TARGET", &t)
            .args([
                "--exact",
                "crash_sigkill_during_replace_is_atomic",
                "--nocapture",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();

        let delay_ms = rng.next_usize(25) as u64;
        thread::sleep(Duration::from_millis(delay_ms));
        let _ = child.kill();
        let _ = child.wait();

        let body = fs::read(&t).unwrap_or_default();
        let old_b = old.as_bytes();
        let new_b = new.as_bytes();
        if body == old_b {
            saw_old += 1;
        } else if body == new_b {
            saw_new += 1;
        } else {
            panic!(
                "iter {i}: torn or foreign content len={} delay_ms={delay_ms} \
                 (neither full OLD nor full NEW)",
                body.len()
            );
        }

        let temps = temp_leftovers(&d);
        for name in &temps {
            assert!(
                name.starts_with(TEMP_MARK),
                "iter {i}: non-conforming leftover {name}"
            );
        }
        temp_residue_total += temps.len();
    }

    eprintln!(
        "crash_sigkill: iters={CRASH_ITERS} saw_old={saw_old} saw_new={saw_new} \
         temp_residue_total={temp_residue_total}"
    );
    assert!(saw_old + saw_new == CRASH_ITERS);
}

fn helper_replace_and_sleep() {
    let dir = PathBuf::from(std::env::var("OPENCRAYAST_FSIO_DIR").unwrap());
    let root = PathBuf::from(std::env::var("OPENCRAYAST_FSIO_ROOT").unwrap());
    let t = PathBuf::from(std::env::var("OPENCRAYAST_FSIO_TARGET").unwrap());
    let rel = std::env::var("OPENCRAYAST_FSIO_REL").unwrap();
    let new = fs::read(dir.join("new.bin")).unwrap();
    // F-01b: even in a child process the write goes through a policy. The boundary cannot be shared
    // across processes, so it is rebuilt from the root the parent passed in; what matters is that
    // there is no bare-path entry point left to call instead.
    let Ok(boundary) = opencrayast_core::boundary::Boundary::new(
        opencrayast_core::boundary::BoundaryConfig::new(root, Limits::default()),
    ) else {
        return;
    };
    let Ok(resolved) = boundary.resolve_write(&rel) else {
        return;
    };
    // The parent passes the target it created. Asserting it equals the policy-resolved path is
    // what makes that env var worth passing: it was previously read into `t` and then dropped
    // with `let _ = &t;`, so a helper pointed anywhere still passed.
    assert_eq!(
        resolved.abs,
        t,
        "the policy resolved a different path than the parent created: {} vs {}",
        resolved.abs.display(),
        t.display()
    );
    // Ignore errors: we may be killed mid-call; parent only checks durability.
    let _ = boundary.replace_file(&resolved, &new);
    // Stay alive so the parent can SIGKILL after a random delay.
    thread::sleep(Duration::from_secs(600));
}

#[test]
fn fsync_dir_ok_on_tempdir() {
    let d = tempfile::tempdir().unwrap();
    assert!(fsync_dir(d.path()).is_ok());
}

/// SECFIX1-07 (CR F3): "the final content is one complete payload" needs payloads a NON-atomic write
/// cannot accidentally produce.
///
/// The concurrency test used 256 copies of a single byte per racer, so replacing `rename` with a
/// non-atomic copy still produced content matching one of the payloads and the assertion stayed
/// green — the reviewer measured 5/5 and 10/10 green that way. These payloads are multi-byte
/// patterns with a per-racer signature, so a partial or interleaved write lands on bytes that
/// belong to no racer at all.
#[test]
fn concurrent_payloads_are_distinguishable() {
    use std::sync::{Arc, Barrier};

    /// Payload for racer `i`: a repeating 4-byte pattern unique to `i`, so any mixture of two racers
    /// contains bytes from both and matches neither whole payload.
    fn payload(i: u8) -> Vec<u8> {
        let mut v = Vec::with_capacity(4096);
        for k in 0..1024usize {
            let digit = b'0' + (k % 10) as u8;
            v.push([b'A' + i, b'a' + i, digit, b'A' + i][k % 4]);
        }
        v
    }

    let ws = Arc::new(Ws::new());
    ws.put("f.txt", &payload(b'0'));
    let barrier = Arc::new(Barrier::new(8));
    let results = Arc::new(Mutex::new(Vec::new()));
    let mut handles = Vec::new();
    for i in 0..8u8 {
        let ws = Arc::clone(&ws);
        let barrier = Arc::clone(&barrier);
        let results = Arc::clone(&results);
        let p = payload(i);
        handles.push(thread::spawn(move || {
            barrier.wait();
            let r = ws.replace("f.txt", &p);
            results.lock().unwrap().push((i, r.is_ok(), p));
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let rows = results.lock().unwrap().clone();
    assert!(!rows.is_empty());

    let final_bytes = ws.read("f.txt");
    // The decisive assertion: the file is EXACTLY one racer's payload. A non-atomic write (copy,
    // truncate-then-write, interleaved) would leave bytes from two racers, matching neither.
    let owner = rows.iter().find(|(_, _, p)| *p == final_bytes);
    assert!(
        owner.is_some(),
        "final content must equal one complete payload; it matched none of the {} racers \\
         (len={}, first 32 bytes={:?})",
        rows.len(),
        final_bytes.len(),
        &final_bytes[..final_bytes.len().min(32)]
    );
    assert!(
        ws.temp_leftovers().is_empty(),
        "no temp residue after the race"
    );
}
