//! CR findings on the plan store (written as specs; never weaken). Unix only.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_core::ErrorCode;
use opencrayast_core::hash::ContentHash;
use opencrayast_core::limits::Limits;
use opencrayast_edit::{Clock, Edit, Plan, PlanFile, PlanRequest, PlanStore};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const WS: &str = "w-00112233445566778899aabbccddeeff";

struct C(AtomicU64);
impl Clock for C {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn plan(n: u32) -> Plan {
    Plan {
        format: 1,
        workspace_id: WS.into(),
        engine_format: 1,
        request: PlanRequest {
            kind: "rewrite".into(),
            summary: format!("n{n}"),
            note: None,
        },
        files: vec![PlanFile {
            path: "a.rs".into(),
            language: "rust".into(),
            pre_hash: ContentHash::of(b"a"),
            pre_size: 20,
            pre_errors: 0,
            post_hash: ContentHash::of(b"b"),
            post_size: 21,
            post_errors: 0,
            edits: vec![Edit {
                start: 0,
                end: 0,
                replacement: "x".into(),
            }],
        }],
    }
}

fn setup(limits: Limits) -> (tempfile::TempDir, PathBuf, PlanStore, Arc<C>) {
    let d = tempfile::tempdir().unwrap();
    let st = d.path().join("s");
    let clock = Arc::new(C(AtomicU64::new(1000)));
    let s = PlanStore::open(&st, WS, limits, clock.clone()).unwrap();
    let dir = st.join(format!("ws-{WS}")).join("plans");
    (d, dir, s, clock)
}

/// A plan whose envelope or bytes no longer verify can never be applied, so it must not hold a
/// slot forever. `list` reports it first (so a doctor can say so); `sweep` and the make-room
/// logic of `put` reclaim it, expired or not, unless it is in use.
#[test]
fn corrupt_entries_are_reclaimable_and_cannot_starve_the_store() {
    let (_d, dir, s, _c) = setup(Limits {
        plan_max_plans: 2,
        ..Limits::default()
    });
    let a = s.put(&plan(1)).unwrap().0;
    let b = s.put(&plan(2)).unwrap().0;
    fs::write(dir.join(format!("{a}.meta.json")), b"garbage").unwrap(); // broken envelope
    fs::write(dir.join(format!("{b}.json")), b"not a plan").unwrap(); // broken plan bytes
    let (listed, bad) = s.list().unwrap();
    assert!(listed.is_empty());
    assert_eq!(bad.len(), 2, "both reported: {bad:?}");
    // put into the "full" store reclaims the two unusable entries instead of refusing forever
    let (c, _) = s.put(&plan(3)).unwrap();
    s.get_for_write(&c).unwrap();
    assert!(!dir.join(format!("{a}.json")).exists() && !dir.join(format!("{b}.json")).exists());
    // and sweep reclaims them too
    let (_d2, dir2, s2, _c2) = setup(Limits::default());
    let x = s2.put(&plan(1)).unwrap().0;
    fs::write(dir2.join(format!("{x}.meta.json")), b"garbage").unwrap();
    assert_eq!(s2.sweep().unwrap(), 1);
    assert!(fs::read_dir(&dir2).unwrap().next().is_none());
}

#[test]
fn a_corrupt_entry_in_use_is_never_reclaimed() {
    let (_d, dir, s, _c) = setup(Limits {
        plan_max_plans: 1,
        ..Limits::default()
    });
    let a = s.put(&plan(1)).unwrap().0;
    let guard = s.begin_use(&a).unwrap();
    fs::write(dir.join(format!("{a}.json")), b"tampered while in use").unwrap();
    assert_eq!(s.sweep().unwrap(), 0);
    assert_eq!(s.put(&plan(2)).unwrap_err().code, ErrorCode::LimitExceeded);
    assert!(dir.join(format!("{a}.json")).exists());
    drop(guard);
}

/// The store directory is attacker-reachable state: after `open`, replacing `plans/` (or the
/// workspace directory above it) with a symlink or with a different directory must make every
/// operation fail with `io_error` and must never write anywhere else.
#[test]
fn a_replaced_store_directory_is_refused_by_every_operation() {
    for how in ["symlink", "other_dir"] {
        let (d, dir, s, _c) = setup(Limits::default());
        let (id, _) = s.put(&plan(1)).unwrap();
        let elsewhere = d.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        fs::rename(&dir, d.path().join("moved")).unwrap();
        if how == "symlink" {
            symlink(&elsewhere, &dir).unwrap();
        } else {
            fs::create_dir(&dir).unwrap();
        }
        let before = fs::read_dir(&elsewhere).unwrap().count();
        let before_dir = fs::read_dir(&dir).unwrap().count();
        assert_eq!(
            s.put(&plan(2)).unwrap_err().code,
            ErrorCode::IoError,
            "{how}: put"
        );
        assert_eq!(
            s.get_for_write(&id).unwrap_err().code,
            ErrorCode::IoError,
            "{how}: get_for_write"
        );
        assert_eq!(
            s.get_for_read(&id).unwrap_err().code,
            ErrorCode::IoError,
            "{how}: get_for_read"
        );
        assert_eq!(
            s.begin_use(&id).unwrap_err().code,
            ErrorCode::IoError,
            "{how}: begin_use"
        );
        assert_eq!(
            s.list().unwrap_err().code,
            ErrorCode::IoError,
            "{how}: list"
        );
        assert_eq!(
            s.sweep().unwrap_err().code,
            ErrorCode::IoError,
            "{how}: sweep"
        );
        assert_eq!(
            fs::read_dir(&elsewhere).unwrap().count(),
            before,
            "{how}: nothing written through"
        );
        assert_eq!(
            fs::read_dir(&dir).unwrap().count(),
            before_dir,
            "{how}: nothing written into the impostor"
        );
    }
}
