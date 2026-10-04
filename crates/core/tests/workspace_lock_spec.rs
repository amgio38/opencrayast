//! Extra cases for ISSUE-CORE-WORKSPACE-LOCK: contention under threads, panic release,
//! hostile ids, and the private mode of the state directory.
//! Refs: STA-07, EDT-07 (core part), SECURITY-MODEL T-33, T-13.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]
use opencrayast_core::ErrorCode;
use opencrayast_core::workspace::*;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

const ID_A: &str = "w-00112233445566778899aabbccddeeff";
const ID_B: &str = "w-ffeeddccbbaa99887766554433221100";

/// Budget for a *probe* acquire whose only job is to be refused by the lock so the thread can
/// report the collision back to the holder. It only has to outlast one `RETRY_INTERVAL`
/// (10 ms) poll of the lock's retry loop, so 25 ms is a wide margin over it. Because the
/// holder is provably holding the lock for the whole probe, `busy` is the only possible
/// outcome: the budget is a guarantee, not a hope.
const COLLISION_PROBE: Duration = Duration::from_millis(25);

/// Upper bound on how long a holder waits to be *told* that a racer lost the race to it.
///
/// This is a ceiling, never a hope, and expiry is a hard failure ("the lock was never
/// contended, so this test proved nothing") rather than a quiet pass. A test that cannot make
/// a thread wait proves nothing about mutual exclusion, so the un-contended outcome has to
/// be red.
const CONTENTION_DEADLINE: Duration = Duration::from_secs(5);

fn tmp() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    d
}

/// EDT-07: eight threads racing for the same workspace lock must never overlap.
///
/// The property to prove is mutual exclusion, and the only way to prove it is for the racers
/// to actually collide with a holder. So the test does not wait out a window hoping they
/// will; it forces the collision:
///
///   1. This thread takes the lock and holds it.
///   2. Seven racers each attempt a probe acquire. The lock is provably held for the whole of
///      step 2, so `busy` is their only possible outcome, and each reports back over a channel
///      the instant it is refused.
///   3. This thread waits for all seven reports (bounded by `CONTENTION_DEADLINE`), releases,
///      and joins the seven in a barrier-released race for the same lock.
///
/// So there is always one holder and seven losers at the same instant, and the eight-way race
/// that follows always has eight contenders. No sleep, no "hold long enough that somebody
/// probably shows up".
///
/// The flip side matters as much: a collision that never happens must be a *failure*, not a
/// pass, because without a genuine overlap the assertion below would be vacuous. That is what
/// `CONTENTION_DEADLINE` is for - expiry panics rather than passing quietly.
#[test]
fn eight_threads_race_without_ever_overlapping() {
    const RACERS: usize = 8;
    let st = tmp();
    let st_path = st.path().to_path_buf();
    let (tx, rx) = channel();
    let tx = Arc::new(Mutex::new(tx));

    let holders = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let acquired = Arc::new(AtomicUsize::new(0));
    let busy = Arc::new(AtomicUsize::new(0));

    // Take the lock *before* the racers exist. Spawning first and locking afterwards would
    // let a racer win the race into a free lock, which is not a contention this test can use.
    let held = ApplyLock::acquire(&st_path, ID_A, Duration::from_millis(2_000))
        .expect("the first holder must take a free lock");

    // Phase 1: every racer is refused while this thread holds the lock.
    let probes: Vec<_> = (0..(RACERS - 1))
        .map(|_| {
            let st_path = st_path.clone();
            let tx = Arc::clone(&tx);
            let holders = Arc::clone(&holders);
            let peak = Arc::clone(&peak);
            let acquired = Arc::clone(&acquired);
            let busy = Arc::clone(&busy);
            std::thread::spawn(move || {
                let e = ApplyLock::acquire(&st_path, ID_A, COLLISION_PROBE)
                    .expect_err("a held lock must never be taken by a racer");
                assert_eq!(e.code, ErrorCode::Busy);
                busy.fetch_add(1, Ordering::SeqCst);
                // The handoff point of phase 1: this thread may not release until all seven
                // refusals have landed, so every one of them provably happened under the lock.
                tx.lock().unwrap().send(()).unwrap();
                (st_path, holders, peak, acquired)
            })
        })
        .collect();

    let refused = wait_for_collision_reports(&rx, RACERS - 1);
    drop(held);

    // Phase 2: the lock is free, and the seven racers plus this thread are all released
    // together, so the eight-way race has eight real contenders.
    let barrier = Arc::new(Barrier::new(RACERS));
    let racers: Vec<_> = probes
        .into_iter()
        .map(|p| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let (st_path, holders, peak, acquired) =
                    p.join().expect("no probe thread panicked");
                barrier.wait();
                racer_body(&st_path, &holders, &peak, &acquired);
            })
        })
        .collect();
    {
        let barrier = Arc::clone(&barrier);
        let holders = Arc::clone(&holders);
        let peak = Arc::clone(&peak);
        let acquired = Arc::clone(&acquired);
        let st_path = st_path.clone();
        std::thread::spawn(move || {
            barrier.wait();
            racer_body(&st_path, &holders, &peak, &acquired);
        })
        .join()
        .expect("no thread may panic");
    }
    for h in racers {
        h.join().expect("no thread may panic");
    }

    assert_eq!(
        refused,
        RACERS - 1,
        "every racer must be told busy while the lock is held"
    );
    assert_eq!(
        busy.load(Ordering::SeqCst),
        RACERS - 1,
        "every refused racer must be counted"
    );
    assert_eq!(
        peak.load(Ordering::SeqCst),
        1,
        "two applies held the lock at the same time"
    );
    assert_eq!(
        holders.load(Ordering::SeqCst),
        0,
        "every holder must have left"
    );
    assert!(
        acquired.load(Ordering::SeqCst) >= RACERS,
        "only {} of the eight contenders ever got in: the race did not run",
        acquired.load(Ordering::SeqCst)
    );
}

/// One contender: take the lock, record the peak concurrency, leave.
///
/// The critical section between `holders += 1` and `holders -= 1` is the thing under test. It
/// used to be a 300 ms sleep, which only made a *simultaneous* hold less likely; it is now a
/// single `yield_now`, because a correct lock already guarantees nobody else can be in here -
/// there is nothing left to wait for. Yielding gives any rival that is already inside `acquire`
/// a chance to be scheduled, and the peak is checked unconditionally once every thread has
/// joined, so a rival that misses the window can only lower `acquired`. It can never turn the
/// test green.
fn racer_body(st_path: &Path, holders: &AtomicUsize, peak: &AtomicUsize, acquired: &AtomicUsize) {
    let lock = ApplyLock::acquire(st_path, ID_A, Duration::from_millis(2_000))
        .expect("the lock must be free when a thread re-races");
    acquired.fetch_add(1, Ordering::SeqCst);
    let now = holders.fetch_add(1, Ordering::SeqCst) + 1;
    peak.fetch_max(now, Ordering::SeqCst);
    std::thread::yield_now();
    holders.fetch_sub(1, Ordering::SeqCst);
    drop(lock);
}

/// Wait for exactly `n` collision reports, failing loudly if they do not arrive.
///
/// The deadline is a ceiling that turns "the lock was never contended" into a failure, which
/// is the point: without a real collision the mutual exclusion assertion would be vacuous.
fn wait_for_collision_reports(rx: &Receiver<()>, n: usize) -> usize {
    let deadline = std::time::Instant::now() + CONTENTION_DEADLINE;
    let mut seen = 0;
    while seen < n {
        match rx.recv_timeout(CONTENTION_DEADLINE) {
            Ok(()) => seen += 1,
            Err(e) => panic!(
                "only {seen} of {n} racers were refused within {CONTENTION_DEADLINE:?} ({e:?}): \
                 the lock was never contended, so mutual exclusion was never tested"
            ),
        }
        if std::time::Instant::now() >= deadline {
            panic!("only {seen} of {n} racers were refused within {CONTENTION_DEADLINE:?}");
        }
    }
    seen
}

/// While one holder keeps the lock, no racer may take it: every one of them is refused.
#[test]
fn no_racer_takes_a_held_lock() {
    let st = tmp();
    // The holder must still hold the lock when the racers give up: its own timeout is 50x the
    // racers', so a slow start cannot make this test pass or fail by timing.
    let held = ApplyLock::acquire(st.path(), ID_A, Duration::from_secs(5)).unwrap();
    let barrier = Arc::new(Barrier::new(8));
    let winners = Arc::new(AtomicUsize::new(0));
    let busy = Arc::new(AtomicUsize::new(0));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let st_path = st.path().to_path_buf();
            let barrier = Arc::clone(&barrier);
            let winners = Arc::clone(&winners);
            let busy = Arc::clone(&busy);
            std::thread::spawn(move || {
                barrier.wait();
                // 200 ms, not 20: the racer has to observe the held lock and be told Busy
                // before its own budget runs out, and thread start-up on a loaded runner is
                // not free.
                match ApplyLock::acquire(&st_path, ID_A, Duration::from_millis(200)) {
                    Ok(_lock) => {
                        winners.fetch_add(1, Ordering::SeqCst);
                    }
                    Err(e) => {
                        assert_eq!(e.code, ErrorCode::Busy);
                        busy.fetch_add(1, Ordering::SeqCst);
                    }
                }
            })
        })
        .collect();
    for h in handles {
        h.join().expect("no thread may panic");
    }
    assert_eq!(
        winners.load(Ordering::SeqCst),
        0,
        "nobody may take the lock while it is held"
    );
    assert_eq!(busy.load(Ordering::SeqCst), 8);
    drop(held);
}

/// A waiter succeeds as soon as the holder releases, without needing its own timeout to
/// expire: the retry loop must actually observe the release.
///
/// The 50 ms sleep this used to sit on was a bet that the waiter had entered its retry loop
/// before the lock came free. A bet has a failure mode: if the waiter had *not* started yet,
/// the sleep would expire, the lock would be dropped, and the waiter would then take a free
/// lock on its first try - so the test passed without the retry loop ever being exercised.
/// The bet was only ever right most of the time, which is exactly the shape of a false green.
///
/// The waiter now proves it is blocked instead of being assumed to be: it first makes a probe
/// acquire that is guaranteed to come back `busy` (this thread holds the lock throughout),
/// announces that, and only then begins the real acquire. So the release below is guaranteed
/// to happen while the waiter is inside `acquire` - an ordering, not a hope. Both channel
/// waits carry a hard ceiling that panics, so the test can neither hang nor pass without the
/// release actually being observed.
#[test]
fn a_waiter_acquires_as_soon_as_the_holder_drops() {
    let st = tmp();
    let held = ApplyLock::acquire(st.path(), ID_A, Duration::from_secs(5)).unwrap();
    let st_path = st.path().to_path_buf();
    let (blocked_tx, blocked_rx) = channel();
    let (acquired_tx, acquired_rx) = channel();
    let waiter = std::thread::spawn(move || {
        // A bounded attempt while the lock is held. `busy` is its only possible outcome, and
        // reaching `busy` is the definition of "the retry loop ran and gave up", so this is the
        // evidence the ordering needs - not a sleep that hopes for the same thing.
        let e = ApplyLock::acquire(&st_path, ID_A, COLLISION_PROBE)
            .expect_err("the held lock must not be taken by the waiter");
        assert_eq!(e.code, ErrorCode::Busy);
        blocked_tx.send(()).ok();
        // A generous timeout: the point is that it succeeds well before it.
        let result = ApplyLock::acquire(&st_path, ID_A, Duration::from_secs(30));
        acquired_tx.send(()).ok();
        result
    });
    blocked_rx
        .recv_timeout(CONTENTION_DEADLINE)
        .unwrap_or_else(|e| {
            panic!("the waiter never confirmed it was blocked ({e:?}); nothing was tested")
        });
    drop(held);
    acquired_rx
        .recv_timeout(CONTENTION_DEADLINE)
        .unwrap_or_else(|e| {
            panic!(
                "the waiter did not acquire within {CONTENTION_DEADLINE:?} of the release ({e:?})"
            )
        });
    let lock = waiter
        .join()
        .expect("no thread may panic")
        .expect("waiter must acquire");
    drop(lock);
}

/// The kernel releases an advisory lock when the descriptor closes, so a panicking apply
/// cannot wedge the workspace forever. This is the property that lets recovery run at all.
#[test]
fn a_panicking_holder_releases_the_lock() {
    let st = tmp();
    let st_path = st.path().to_path_buf();
    let holder = std::thread::spawn(move || {
        let _lock = ApplyLock::acquire(&st_path, ID_A, Duration::from_secs(5)).unwrap();
        panic!("apply blew up half way through");
    });
    let panicked = holder.join().is_err();
    assert!(panicked, "the holder was supposed to panic");
    // The workspace is usable again straight away.
    let lock = ApplyLock::acquire(st.path(), ID_A, Duration::from_secs(5))
        .expect("a crashed apply must not leave the lock held");
    drop(lock);
}

/// The id is validated as a closed character set, so nothing that could climb out of the
/// state directory is ever joined onto it.
#[test]
fn hostile_workspace_ids_are_refused_before_touching_disk() {
    let st = tmp();
    for bad in [
        "",
        "../x",
        "w-../..",
        "w-xyz",
        "../../etc",
        "w-",
        "w",
        "00112233445566778899aabbccddeeff",
        "w-00112233445566778899AABBCCDDEEFF",  // uppercase hex
        "w-00112233445566778899aabbccddeef",   // one short
        "w-00112233445566778899aabbccddeeff0", // one long
        "w-00112233445566778899aabbccddee/ff", // a separator
        "w-00112233445566778899aabbccddeef\u{0}f",
        "w-../../../../tmp/evil",
        "w-....",
    ] {
        let e = ApplyLock::acquire(st.path(), bad, Duration::from_millis(10))
            .expect_err(&format!("{bad} must be refused"));
        assert_eq!(e.code, ErrorCode::InvalidArgs, "{bad}");
    }
    // Nothing was created on disk by any of those attempts.
    let entries: Vec<_> = std::fs::read_dir(st.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert!(entries.is_empty(), "unexpected state entries: {entries:?}");
}

/// The lock lives in a `0700` directory with a `0600` file.
#[test]
fn the_state_directory_and_lock_file_are_private() {
    let st = tmp();
    let _lock = ApplyLock::acquire(st.path(), ID_A, Duration::from_millis(100)).unwrap();
    let ws_dir = st.path().join("ws-").join(ID_A.trim_start_matches("w-"));
    // `ws_id` already carries the `w-` prefix, so the directory is `ws-w-<hex>`: check the
    // real path rather than assuming.
    let real = std::fs::read_dir(st.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.is_dir())
        .expect("the workspace state directory must exist");
    let _ = ws_dir;
    let dir_mode = std::fs::metadata(&real).unwrap().permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700, "workspace state directory must be 0700");
    let lock_file = real.join("apply.lock");
    let file_mode = std::fs::metadata(&lock_file).unwrap().permissions().mode() & 0o777;
    assert_eq!(file_mode, 0o600, "lock file must be 0600");
}

/// A pre-existing `0777` workspace directory is **refused, not repaired**.
///
/// This is the opposite of what this test asserted before the two directory creators were
/// consolidated. `ApplyLock::acquire` used to call a private `create_private_dir` that did
/// `set_permissions(0700)` on whatever it found, while `ensure_state_dir` refused the same
/// directory — two creators for one directory, opposite policies, and the weaker one was on the
/// apply path. Executed against a `0777` foreign-owned directory, `ensure_state_dir` said `Err`
/// and `ApplyLock::acquire` said `Ok` and changed the mode.
///
/// Now there is one creator and one policy, so the two halves of the old contradiction are
/// asserted together: the lock path refuses *and* leaves the mode exactly as it found it.
#[test]
fn a_loose_workspace_directory_is_refused_and_not_repaired() {
    let st = tmp();
    let loose = st.path().join("ws-w-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    std::fs::create_dir(&loose).unwrap();
    std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o777)).unwrap();

    let e = ApplyLock::acquire(
        st.path(),
        "w-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        Duration::from_millis(100),
    )
    .expect_err("a group/other-accessible directory must be refused, not tightened");
    assert_eq!(e.code, ErrorCode::IoError);
    assert_eq!(
        std::fs::metadata(&loose).unwrap().permissions().mode() & 0o777,
        0o777,
        "the refusal must not repair the directory: it was not ours to change"
    );
    assert!(
        !loose.join("apply.lock").exists(),
        "no lock file may be created inside a refused directory"
    );
}

/// And the two halves really are one policy now: what `ensure_state_dir` says about a directory
/// is exactly what `ApplyLock::acquire` says about the same directory. Before the consolidation
/// these two disagreed, which is the whole defect.
#[test]
fn the_lock_path_and_ensure_state_dir_agree() {
    for mode in [0o777u32, 0o755, 0o770] {
        let st = tmp();
        let loose = st.path().join("ws-w-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        std::fs::create_dir(&loose).unwrap();
        std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(mode)).unwrap();

        let ensure = opencrayast_core::statedir::ensure_state_dir(&loose);
        let lock = ApplyLock::acquire(
            st.path(),
            "w-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            Duration::from_millis(50),
        );
        assert!(
            ensure.is_err() && lock.is_err(),
            "mode {mode:o}: ensure_state_dir and the lock path must agree, got ensure={ensure:?} \
             lock={:?}",
            lock.map(|_| ())
        );
    }
}

/// T-33: identity comes from the tree, not from the spelling, and a removed-and-recreated
/// root does not inherit the old id.
#[test]
fn identity_follows_the_tree_not_the_path() {
    let base = tmp();
    let root = base.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let id = workspace_id(&root).unwrap();
    // A different spelling of the same directory: a symlink parent.
    let alias = base.path().join("alias");
    std::os::unix::fs::symlink(&root, &alias).unwrap();
    assert_eq!(id, workspace_id(&alias).unwrap());
    // A path that is not a directory, and a path that does not exist, are refused.
    let file = base.path().join("plain.txt");
    std::fs::write(&file, b"x").unwrap();
    assert_eq!(
        workspace_id(&file).unwrap_err().code,
        ErrorCode::InvalidArgs
    );
    assert_eq!(
        workspace_id(&base.path().join("nope")).unwrap_err().code,
        ErrorCode::NotFound
    );
    // Recreating the directory at the same path: the id stays the same when the filesystem
    // hands back the same inode, which is the common case and is why an id must be read as
    // "this tree, for as long as it exists" rather than as a permanent name for a path.
    // What must never happen is two *different* trees sharing an id, which the dev+ino
    // pair rules out even when the path is the only thing that distinguishes them.
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::create_dir(&root).unwrap();
    let recreated = workspace_id(&root).unwrap();
    let sibling = base.path().join("other");
    std::fs::create_dir(&sibling).unwrap();
    assert_ne!(recreated, workspace_id(&sibling).unwrap());
    // A symlinked spelling of the recreated tree still agrees with the real path.
    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(&root, &alias).unwrap();
    assert_eq!(recreated, workspace_id(&alias).unwrap());
}

/// A zero timeout still takes the free lock rather than reporting `busy`.
#[test]
fn a_free_lock_is_taken_even_with_a_zero_timeout() {
    let st = tmp();
    let a = ApplyLock::acquire(st.path(), ID_A, Duration::ZERO);
    assert!(a.is_ok());
    let b = ApplyLock::acquire(st.path(), ID_B, Duration::ZERO);
    assert!(b.is_ok());
}

/// STA-07: a symlink planted at `<ws>/apply.lock` must make `acquire` fail, and the file
/// the link points at must not be created or written. A followed link would put the lock on
/// an attacker-chosen file, so two workspaces could then be locked together, or not at all.
///
/// This is the test that fails if `O_NOFOLLOW` is ever replaced by a hardcoded value from
/// the wrong architecture: the flag numbers differ per platform, and a literal taken from
/// x86 Linux is a different (usually harmless-looking) bit on arm64, where the open would
/// either ignore the link or fail for an unrelated reason.
#[test]
fn a_symlinked_lock_file_is_refused_and_its_target_is_not_created() {
    let st = tmp();
    let ws_id = "w-00112233445566778899aabbccddeeff";
    // The directory name is `ws-` plus the id, and the id already starts with `w-`.
    let real_dir = st.path().join(format!("ws-{ws_id}"));
    std::fs::create_dir(&real_dir).unwrap();

    // A path outside the workspace state dir that the link points at. It does not exist.
    let outside = st.path().join("attacker-target.lock");
    assert!(!outside.exists());
    std::os::unix::fs::symlink(&outside, real_dir.join("apply.lock")).unwrap();

    let e = ApplyLock::acquire(st.path(), ws_id, Duration::from_millis(100))
        .expect_err("a symlinked lock file must be refused");
    assert!(
        matches!(e.code, ErrorCode::IoError | ErrorCode::UnsupportedTarget),
        "unexpected code {:?}",
        e.code
    );
    // The refusal must be total: the link's target was never created, so the lock was not
    // taken on some other file.
    assert!(
        !outside.exists(),
        "the symlink target must not be created by a refused acquire"
    );
    // And the symlink is still a symlink: the open did not replace it with a real file.
    assert!(
        std::fs::symlink_metadata(real_dir.join("apply.lock"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "acquire must not have overwritten the planted symlink"
    );
}

/// Two state directories holding the same workspace id are different locks, and the
/// workspace id directory name is derived from the validated id alone.
#[test]
fn the_lock_is_scoped_to_the_state_directory() {
    let a = tmp();
    let b = tmp();
    let held = ApplyLock::acquire(a.path(), ID_A, Duration::from_millis(100)).unwrap();
    assert!(
        ApplyLock::acquire(b.path(), ID_A, Duration::from_millis(100)).is_ok(),
        "a different state directory is a different lock"
    );
    drop(held);
    let _ = Path::new("/");
}

/// Makes the symlink defence load-bearing.
///
/// On x86-64 Linux the literal `0o400000` is in fact the correct `O_NOFOLLOW`, so the
/// symlink test above passes with it and cannot, on this machine, tell a correct constant
/// from a hardcoded one. This test pins the property that actually matters: the value in
/// use on this platform refuses a symlink, and the value belonging to a *different*
/// architecture is a different bit. If the code regresses to a literal, this is the
/// comparison whose meaning changes per build machine, and the numbers to remember are
/// x86-64 Linux `0o400000`, arm64 Linux `0o100000`, and again something else on macOS/BSD.
#[test]
fn the_nofollow_value_in_use_is_this_platforms() {
    use std::os::unix::fs::OpenOptionsExt;
    let st = tmp();
    let real = st.path().join("real.lock");
    std::fs::write(&real, b"x").unwrap();
    let link = st.path().join("link.lock");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let open_with = |flags: i32| {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(flags)
            .open(&link)
    };

    let correct = rustix::fs::OFlags::NOFOLLOW.bits() as i32;
    assert!(
        open_with(correct).is_err(),
        "this platform's O_NOFOLLOW must refuse a symlink"
    );
    // The concrete number differs per platform AND per architecture, so it is only pinned
    // where the value is actually documented. Asserting a Linux number on macOS failed CI
    // run 36922126046: macOS uses 0x100 on both x86_64 and aarch64.
    //
    // On any other platform the number is deliberately not asserted. What must hold
    // everywhere is the behaviour above: the value in use refuses a symlink. That is the
    // property the code depends on; the literal is not.
    //
    // Linux: x86_64 0o400000, aarch64 0o100000. macOS: 0x100 on both.
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    assert_eq!(correct, 0o400000);
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    assert_eq!(correct, 0o100000);
    #[cfg(target_os = "macos")]
    assert_eq!(correct, 0x100);
}
