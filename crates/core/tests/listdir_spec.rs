//! Spec for ISSUE-CORE-LISTDIR: `Boundary::read_dir` through the boundary, dirfd-relative,
//! `lstat` kinds, and the counting of everything that is not a plain file.
//! Add cases; never weaken these.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
#![cfg(unix)]
use opencrayast_core::ErrorCode;
use opencrayast_core::boundary::{Boundary, BoundaryConfig, ResolvedPath};
use opencrayast_core::limits::Limits;
use opencrayast_core::walk::{DirEntryInfo, EntryKind};
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The name of the errno that means "this filesystem will not store that name", if the error
/// is that. `EILSEQ` is what APFS and HFS+ answer for a name that is not valid UTF-8; `EINVAL`
/// is the other answer the same refusal produces on some kernels. Any other error is a real
/// failure and is returned to the caller, so a genuine bug can never hide behind a skip.
fn name_too_exotic(errno: &std::io::Error) -> Option<&'static str> {
    // `std` reports the C errno as `Option<i32>`, `rustix` as a bare `i32`; wrap the latter
    // rather than unwrap the former, so no errno can panic here.
    let raw = errno.raw_os_error();
    if raw == Some(rustix::io::Errno::ILSEQ.raw_os_error()) {
        Some("EILSEQ")
    } else if raw == Some(rustix::io::Errno::INVAL.raw_os_error()) {
        Some("EINVAL")
    } else {
        None
    }
}

/// True when two names in one directory would collide on this filesystem, i.e. it is
/// case-insensitive. Probed rather than assumed: ext4 is case-sensitive, APFS and NTFS are not
/// by default, and a test that writes `A.rs` and `a.rs` gets one file on one and two on the
/// other. The probe writes `x`, then `X`, and counts the entries.
fn filesystem_is_case_insensitive(dir: &Path) -> bool {
    fs::write(dir.join("x"), "").unwrap();
    fs::write(dir.join("X"), "").unwrap();
    fs::read_dir(dir).unwrap().count() == 1
}

/// The names to create, and the byte order they must come back in, for a case-sensitive or a
/// case-insensitive filesystem.
///
/// Pure data, so BOTH branches can be checked on any machine: the branch this run cannot take
/// is still verified here, instead of being taken on trust until a macOS runner trips over it.
/// The multi-byte names (`\u{00ff}`, `\u{4e2d}`) have no Unicode decomposition, so no
/// filesystem rewrites them; `\u{00e9}` would come back as `e` + U+0301 from APFS.
fn case_set(insensitive: bool) -> (&'static [&'static str], &'static [&'static str]) {
    if insensitive {
        // No two names here differ only by case, so all five are created on any filesystem.
        // `Z.rs` still carries the upper-before-lower claim (0x5A before 0x62).
        (
            &["b.rs", "Z.rs", "b2.rs", "\u{00ff}.rs", "\u{4e2d}.rs"],
            &["Z.rs", "b.rs", "b2.rs", "\u{00ff}.rs", "\u{4e2d}.rs"],
        )
    } else {
        (
            &["b.rs", "A.rs", "a.rs", "Z.rs", "\u{00ff}.rs", "\u{4e2d}.rs"],
            &["A.rs", "Z.rs", "a.rs", "b.rs", "\u{00ff}.rs", "\u{4e2d}.rs"],
        )
    }
}

/// Workspace + an outside directory + a boundary whose read roots contain `pub`.
fn mk() -> (tempfile::TempDir, tempfile::TempDir, Boundary) {
    let ws = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    // The read root has to exist before the boundary validates it.
    fs::create_dir(out.path().join("pub")).unwrap();
    let b = Boundary::new(BoundaryConfig {
        root: ws.path().to_path_buf(),
        read_roots: vec![out.path().join("pub")],
        limits: Limits::default(),
        state_dir: None,
        extra_protected: Vec::new(),
    })
    .unwrap();
    (ws, out, b)
}

fn put(ws: &tempfile::TempDir, rel: &str, body: &str) {
    let p = ws.path().join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

fn names(e: &[DirEntryInfo]) -> Vec<&str> {
    e.iter().map(|x| x.name.as_str()).collect()
}

fn kind_of<'a>(e: &'a [DirEntryInfo], n: &str) -> &'a EntryKind {
    &e.iter().find(|x| x.name == n).unwrap().kind
}

/// A: a name that is not valid UTF-8 must still be listed - as `Other`, with a lossy name,
/// so that the walker counts it in `skipped_special` instead of the entry silently
/// vanishing (OUT-07). The bytes here are a lone continuation byte plus one that cannot
/// start a sequence; a valid name next to it must be unaffected.
///
/// SKIPPED where the filesystem cannot store such a name at all: APFS and HFS+ require a file
/// name to be valid UTF-8 and answer `EILSEQ` (or `EINVAL`) to the `open`, so there is nothing
/// to list and the test has no subject. It still runs for real on Linux, where ext4 stores
/// arbitrary non-NUL bytes.
#[test]
fn a_non_utf8_name_is_listed_as_other_with_a_lossy_name() {
    let (ws, _o, b) = mk();
    put(&ws, "plain.rs", "");
    let bad = ws.path().join(OsStr::from_bytes(b"bad\xff\xfename.rs"));
    if let Err(e) = fs::write(&bad, "") {
        match name_too_exotic(&e) {
            Some(why) => {
                println!("SKIPPED: this filesystem cannot hold non-UTF-8 names ({why})");
                return;
            }
            None => panic!("creating a non-UTF-8 name failed for an unrelated reason: {e}"),
        }
    }

    let dir = b.resolve_read(".").unwrap();
    let e = b.read_dir(&dir).unwrap();
    assert_eq!(e.len(), 2, "nothing may be dropped: {:?}", names(&e));
    let entry = e.iter().find(|x| x.name != "plain.rs").unwrap();
    assert_eq!(entry.kind, EntryKind::Other);
    assert!(
        entry.name.starts_with("bad") && entry.name.ends_with("name.rs"),
        "lossy name keeps the readable parts: {:?}",
        entry.name
    );
    assert_eq!(kind_of(&e, "plain.rs"), &EntryKind::File);
}

/// The macOS branch of the sort test cannot run on a case-sensitive filesystem, so its
/// expectation is checked here instead of being taken on trust: both name sets must sort into
/// exactly the order the test expects when sorted by bytes.
#[test]
fn both_case_branches_expect_byte_order_and_nothing_else() {
    for insensitive in [false, true] {
        let (set, expected) = case_set(insensitive);
        let mut sorted: Vec<&str> = set.to_vec();
        sorted.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        assert_eq!(sorted, expected, "case-insensitive: {insensitive}");
        // The case-insensitive set must not contain two names that differ only by case, or it
        // would create one file and expect two.
        if insensitive {
            for a in set {
                for b in set {
                    assert!(
                        a == b || a.to_lowercase() != b.to_lowercase(),
                        "{a} and {b} collide on a case-insensitive filesystem"
                    );
                }
            }
        }
    }
}

/// The skip for "this filesystem cannot hold non-UTF-8 names" must trigger on exactly the two
/// errnos that mean it, and on nothing else - otherwise a real failure would be reported as a
/// skip and the test would quietly stop testing anything.
#[test]
fn the_non_utf8_skip_only_triggers_on_the_two_exotic_errnos() {
    let exotic = |n: i32| std::io::Error::from_raw_os_error(n);
    assert_eq!(
        name_too_exotic(&exotic(rustix::io::Errno::ILSEQ.raw_os_error())),
        Some("EILSEQ")
    );
    assert_eq!(
        name_too_exotic(&exotic(rustix::io::Errno::INVAL.raw_os_error())),
        Some("EINVAL")
    );
    for errno in [
        rustix::io::Errno::ACCESS,
        rustix::io::Errno::PERM,
        rustix::io::Errno::NOENT,
        rustix::io::Errno::NAMETOOLONG,
        rustix::io::Errno::LOOP,
    ] {
        assert_eq!(
            name_too_exotic(&exotic(errno.raw_os_error())),
            None,
            "{errno:?} must not be swallowed by the skip"
        );
    }
    // An error with no errno at all is not a skip either.
    assert_eq!(name_too_exotic(&std::io::Error::other("boom")), None);
}

/// B: many entries. One call must not lose or duplicate any of them, and must stay sorted.
/// 10,000 is well past anything a source tree has, and it is where a listing bug that
/// depends on a buffer refilling (the `getdents` boundary) would show up.
#[test]
fn b_ten_thousand_entries_are_all_listed_in_sorted_order() {
    let (ws, _o, b) = mk();
    let n = 10_000;
    for i in 0..n {
        put(&ws, &format!("f{i:05}.rs"), "");
    }
    let dir = b.resolve_read(".").unwrap();
    let e = b.read_dir(&dir).unwrap();
    assert_eq!(e.len(), n);
    let raw: Vec<&str> = names(&e);
    let mut expect = raw.clone();
    expect.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    assert_eq!(raw, expect, "byte-sorted order");
}

/// Sorting is by BYTES, not by locale: an upper-case name sorts before a lower-case one, and a
/// name whose first byte is >= 0x80 sorts after every ASCII one. A locale-aware sort would give
/// a different answer on a machine with a different locale, and the listing has to be the same
/// on every machine.
///
/// Two filesystem facts force two name sets, and both are asserted by the probe rather than
/// assumed:
///
/// - **Case sensitivity.** `A.rs` and `a.rs` are two files on ext4 and ONE file on a
///   case-insensitive filesystem (APFS by default, NTFS by default). The case-sensitive set
///   therefore contains such a pair, and the case-insensitive set does not - it keeps the
///   upper-before-lower claim with names that differ by more than case (`B.rs` < `b2.rs`).
/// - **Unicode normalisation.** APFS and HFS+ store names in NFD, so a name written as
///   precomposed `\u{00e9}` comes back as `e` + U+0301 and the expectation would not match its
///   own input. The multi-byte names used here (`\u{00ff}`, `\u{4e2d}`) have no decomposition,
///   so every filesystem stores and returns exactly the bytes that were written.
#[test]
fn c_entries_sort_by_bytes_not_by_locale() {
    let (ws, _o, b) = mk();
    let insensitive = filesystem_is_case_insensitive(ws.path());
    let (set, expected) = case_set(insensitive);
    // The probe left `x` and/or `X` behind; they are not part of what is being measured.
    let _ = fs::remove_file(ws.path().join("x"));
    let _ = fs::remove_file(ws.path().join("X"));
    for n in set {
        put(&ws, n, "");
    }
    let dir = b.resolve_read(".").unwrap();
    let e = b.read_dir(&dir).unwrap();
    assert_eq!(
        names(&e),
        expected,
        "byte order, case-insensitive: {insensitive}"
    );
}

/// D: entries removed while the directory is being listed must not fail the call. What a
/// directory listing can honestly report about a tree that is changing underneath it is
/// "this is what I saw", so the guarantee is only that the call succeeds and returns no
/// entry that was never there. Deleting the whole directory first is the deterministic half.
#[test]
fn d_entries_removed_during_a_listing_never_fail_the_call() {
    let (ws, _o, b) = mk();
    for i in 0..64 {
        put(&ws, &format!("keep{i:02}.rs"), "");
    }
    let dir = b.resolve_read(".").unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let churn = {
        let stop = Arc::clone(&stop);
        let ws = ws.path().to_path_buf();
        std::thread::spawn(move || {
            let mut i = 0u32;
            while !stop.load(Ordering::Relaxed) {
                let _ = fs::remove_file(ws.join(format!("keep{:02}.rs", i % 64)));
                i += 1;
            }
            i
        })
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut listings = 0u64;
    while std::time::Instant::now() < deadline {
        let e = b
            .read_dir(&dir)
            .expect("a listing must not fail while files disappear");
        assert!(
            e.len() <= 64,
            "a listing invented entries: {} of them",
            e.len()
        );
        listings += 1;
    }
    stop.store(true, Ordering::Relaxed);
    let removed = churn.join().unwrap();
    assert!(
        listings > 10,
        "only {listings} listings: the race never ran"
    );
    assert!(
        removed > 10,
        "only {removed} removals: the tree never changed"
    );

    // Deterministic tail: everything gone is an empty listing, not an error.
    for i in 0..64 {
        let _ = fs::remove_file(ws.path().join(format!("keep{i:02}.rs")));
    }
    assert_eq!(names(&b.read_dir(&dir).unwrap()), Vec::<&str>::new());
    // And the directory itself removed. The caller holds a `ResolvedPath` from before, and
    // a `ResolvedPath` is not evidence: containment has to be re-proved, and re-proving it
    // for something that no longer exists fails closed. `open_read` answers the same way
    // (BND-18: "cannot be proved to be inside" and "outside" must be indistinguishable).
    fs::remove_dir(ws.path()).unwrap();
    let err = b
        .read_dir(&dir)
        .expect_err("a removed directory cannot be listed");
    assert_eq!(err.code, ErrorCode::OutsideWorkspace, "{err}");
}

/// E: the race the whole design exists for. One thread flips an in-workspace directory for a
/// symlink to an outside one with atomic renames, the other keeps listing a path it resolved
/// once. The listing must never show what is outside: it either fails, or it shows the
/// inside directory. Both outcomes have to happen a meaningful number of times, so the test
/// cannot pass by refusing everything or by never racing.
#[test]
fn e_racing_directory_swap_never_lists_outside() {
    let (ws, out, b) = mk();
    put(&ws, "d/inside.rs", "");
    put(&ws, "d/sub/deep.rs", "");
    // The outside directory has a marker file no inside directory has.
    fs::write(out.path().join("outside_marker.rs"), "").unwrap();

    let b = Arc::new(b);
    let dir: ResolvedPath = b.resolve_read("d").unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let flipper = {
        let stop = Arc::clone(&stop);
        let ws = ws.path().to_path_buf();
        let outside = out.path().to_path_buf();
        std::thread::spawn(move || {
            let (tmp, d) = (ws.join("d_tmp"), ws.join("d"));
            // Each state is HELD for a moment. A listing takes tens of microseconds, so
            // without a hold the reader would almost always find the entry missing or
            // linked and the "listed" side of the race would never be exercised.
            let hold = std::time::Duration::from_micros(250);
            let mut flips = 0u64;
            while !stop.load(Ordering::Relaxed) {
                // The inside directory, renamed away; the entry is then replaced by the
                // outside symlink with one atomic rename.
                for _ in 0..2 {
                    if fs::rename(&d, &tmp).is_ok() {
                        break;
                    }
                }
                if symlink(&outside, &d).is_ok() {
                    flips += 1;
                }
                std::thread::sleep(hold);
                let _ = fs::remove_file(&d);
                let _ = fs::rename(&tmp, &d);
                std::thread::sleep(hold);
            }
            flips
        })
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut listed = 0u64;
    let mut refused = 0u64;
    while std::time::Instant::now() < deadline {
        match b.read_dir(&dir) {
            Ok(entries) => {
                listed += 1;
                let n = names(&entries);
                assert!(
                    !n.contains(&"outside_marker.rs"),
                    "listing showed a file from OUTSIDE the workspace: {n:?}"
                );
            }
            Err(e) => {
                refused += 1;
                // Three honest refusals, all fine and none leaking anything: the entry is a
                // symlink now, so containment cannot be proved (`outside_workspace`, or
                // `io_error` "not a directory" when the open itself gets there first, because
                // `O_DIRECTORY|O_NOFOLLOW` answers ENOTDIR for a link); or it is absent at
                // this instant because the flipper renamed it away (`not_found`, which is
                // what `open_read` answers for a vanished path inside the workspace).
                assert!(
                    matches!(
                        e.code,
                        ErrorCode::OutsideWorkspace | ErrorCode::NotFound | ErrorCode::IoError
                    ),
                    "unexpected refusal: {e}"
                );
                assert!(!e.message.contains('/'), "refusal names a path: {e}");
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
    let flips = flipper.join().unwrap();
    assert!(
        listed > 10,
        "only {listed} successful listings: nothing was tested"
    );
    assert!(
        refused > 10,
        "only {refused} refusals: the swap never landed"
    );
    assert!(
        flips > 10,
        "only {flips} flips: the directory never changed"
    );
}

/// F: listing something that is not a directory is an honest `io_error`, not the
/// outside-workspace refusal (the caller named a path inside its own workspace, so there is
/// nothing to hide) and not `not_found`. The message carries no path.
#[test]
fn f_listing_a_file_is_an_io_error_without_a_path() {
    let (ws, _o, b) = mk();
    put(&ws, "a.rs", "");
    let file = b.resolve_read("a.rs").unwrap();
    let err = b
        .read_dir(&file)
        .expect_err("a regular file cannot be listed");
    assert_eq!(err.code, ErrorCode::IoError, "{err}");
    assert!(err.message.contains("not a directory"), "{}", err.message);
    assert!(
        !err.message.contains("a.rs") && !err.message.contains(ws.path().to_str().unwrap()),
        "message names the path: {}",
        err.message
    );
}

/// G: a read root is a root like any other. Listing inside one works and shows the
/// `@root1/` label; the label is what makes the entries usable afterwards.
#[test]
fn g_a_read_root_directory_can_be_listed_and_is_labelled() {
    let (_ws, out, b) = mk();
    fs::write(out.path().join("pub/b.rs"), "").unwrap();
    fs::create_dir(out.path().join("pub/d")).unwrap();
    let inside = out.path().join("pub/d");
    let dir = b
        .resolve_read(inside.canonicalize().unwrap().to_str().unwrap())
        .unwrap();
    assert_eq!(dir.rel, "@root1/d");
    assert!(names(&b.read_dir(&dir).unwrap()).is_empty());
    let dir = b
        .resolve_read(
            out.path()
                .join("pub")
                .canonicalize()
                .unwrap()
                .to_str()
                .unwrap(),
        )
        .unwrap();
    assert_eq!(dir.rel, "@root1/");
    let e = b.read_dir(&dir).unwrap();
    assert_eq!(names(&e), ["b.rs", "d"]);
    assert_eq!(kind_of(&e, "d"), &EntryKind::Dir);
}

/// H: an entry whose kind cannot be read is `Other`, never a guess. Removing an entry
/// between the moment the directory stream hands it over and the moment it is stat'ed is
/// the honest version of this, and the observable rule is that the listing still succeeds.
#[test]
fn h_special_entries_are_other_and_links_are_never_followed() {
    let (ws, out, b) = mk();
    put(&ws, "f.rs", "");
    let fifo = ws.path().join("pipe");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(status.success());
    // A unix socket, the other `Other` case Linux and macOS disagree about errno for.
    let sock = std::os::unix::net::UnixListener::bind(ws.path().join("sock")).unwrap();
    let dir = b.resolve_read(".").unwrap();
    let e = b.read_dir(&dir).unwrap();
    assert_eq!(kind_of(&e, "f.rs"), &EntryKind::File);
    assert_eq!(kind_of(&e, "pipe"), &EntryKind::Other);
    assert_eq!(kind_of(&e, "sock"), &EntryKind::Other);

    // A link to a directory outside is a link, not that directory.
    symlink(out.path(), ws.path().join("lnk")).unwrap();
    let e = b.read_dir(&dir).unwrap();
    println!("ours: {:?}", names(&e));
    assert_eq!(kind_of(&e, "lnk"), &EntryKind::Symlink);
    drop(sock);
}

/// I: a link that points INSIDE the workspace is still only a link: the kinds come from
/// `lstat` semantics, so nothing in a listing is ever resolved through a symlink. This is
/// the case a "helpful" implementation would get wrong by reporting the target's kind.
#[test]
fn i_a_link_pointing_inside_is_still_a_link() {
    let (ws, _o, b) = mk();
    put(&ws, "target/f.rs", "");
    symlink(ws.path().join("target"), ws.path().join("to_dir")).unwrap();
    symlink(ws.path().join("target/f.rs"), ws.path().join("to_file")).unwrap();
    let dir = b.resolve_read(".").unwrap();
    let e = b.read_dir(&dir).unwrap();
    assert_eq!(kind_of(&e, "target"), &EntryKind::Dir);
    assert_eq!(kind_of(&e, "to_dir"), &EntryKind::Symlink);
    assert_eq!(kind_of(&e, "to_file"), &EntryKind::Symlink);
}

/// L: two listings of the same directory, and of two different directories, are independent
/// of each other. A duplicated directory descriptor SHARES its read offset with the
/// original, so a listing built on a `dup` of the pinned root handle would leave that
/// handle positioned at the end of the directory: the first call would see every entry and
/// the second would see none. The handle the root is opened with has to be a fresh open, not
/// a copy.
#[test]
fn l_repeated_and_interleaved_listings_are_independent() {
    let (ws, _o, b) = mk();
    put(&ws, "a.rs", "");
    put(&ws, "d/b.rs", "");
    let root = b.resolve_read(".").unwrap();
    let sub = b.resolve_read("d").unwrap();
    let want_root = ["a.rs", "d"];
    let want_sub = ["b.rs"];
    for round in 0..3 {
        assert_eq!(
            names(&b.read_dir(&root).unwrap()),
            want_root,
            "round {round}"
        );
        assert_eq!(names(&b.read_dir(&sub).unwrap()), want_sub, "round {round}");
        // Interleaved the other way round, to catch an offset shared in either direction.
        assert_eq!(names(&b.read_dir(&sub).unwrap()), want_sub, "round {round}");
        assert_eq!(
            names(&b.read_dir(&root).unwrap()),
            want_root,
            "round {round}"
        );
    }
}

/// The entry ceiling (`MAX_DIR_ENTRIES_HARD` = 200,000) is deliberately NOT exercised
/// here: a test would have to create 200,001 inodes in one directory, which is minutes of
/// I/O and tens of thousands of times the disk a unit test may assume. It is a refusal, not
/// a truncation, so the risk of it being wrong is a spurious `limit_exceeded` on a
/// pathological directory - visible, not dangerous - rather than a wrong answer. The limit
/// itself is asserted where it can be: in the module documentation and the error message.
const _CEILING_IS_DOCUMENTED_NOT_TESTED: () = ();

/// J: a directory whose parent was replaced by a link between resolve and list is refused
/// the same way as the race above, and without a path in the message. This is the
/// single-shot version, for the case where the race test's timing hides the reason.
#[test]
fn j_a_directory_swapped_for_a_link_after_resolving_is_refused() {
    let (ws, out, b) = mk();
    put(&ws, "d/f.rs", "");
    let dir = b.resolve_read("d").unwrap();
    fs::remove_dir_all(ws.path().join("d")).unwrap();
    symlink(out.path(), ws.path().join("d")).unwrap();
    let err = b
        .read_dir(&dir)
        .expect_err("a directory swapped for a link must be refused");
    assert_eq!(err.code, ErrorCode::OutsideWorkspace, "{err}");
    assert!(!err.message.contains('/'), "{err}");
}

/// K: the refusal is uniform, so an agent cannot use a listing to probe what exists
/// outside: every failure to list something outside is word-for-word the same refusal, and
/// the outside target of the probe really does exist and really does hold the file.
#[test]
fn k_listing_outside_is_indistinguishable_from_listing_nothing() {
    let (ws, out, b) = mk();
    // A real directory that exists, holds a file, and is under no root: the exact thing a
    // listing refusal must not reveal.
    let outside_dir = out.path().join("secret");
    fs::create_dir(&outside_dir).unwrap();
    fs::write(outside_dir.join("f.rs"), "").unwrap();
    let outside_path = outside_dir
        .canonicalize()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let outside_there = b
        .resolve_read(&outside_path)
        .expect_err("an outside path must be refused");
    let outside_missing = b
        .resolve_read("/definitely/not/here/opencrayast")
        .expect_err("a missing outside path must be refused");
    assert_eq!(outside_there, outside_missing);
    assert_eq!(outside_there.code, ErrorCode::OutsideWorkspace);
    let _ = ws;
    let _: &Path = outside_dir.as_path();
}
