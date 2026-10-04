//! Extra EDIT5-xx cases for ISSUE-EDIT-5 (do not weaken jstore_spec).
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_core::ErrorCode;
use opencrayast_core::hash::ContentHash;
use opencrayast_core::limits::Limits;
use opencrayast_edit::{Clock, Edit, JournalState, JournalStore, Plan, PlanFile, PlanRequest};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const WS: &str = "w-00112233445566778899aabbccddeeff";
const DAY: u64 = 86_400;

struct FakeClock(AtomicU64);
impl Clock for FakeClock {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct Fx {
    _dir: tempfile::TempDir,
    state: PathBuf,
    clock: Arc<FakeClock>,
}

impl Fx {
    fn new() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        Fx {
            _dir: dir,
            state,
            clock: Arc::new(FakeClock(AtomicU64::new(1_000_000))),
        }
    }
    fn open(&self, limits: Limits) -> JournalStore {
        JournalStore::open(&self.state, WS, limits, self.clock.clone()).unwrap()
    }
    fn advance(&self, secs: u64) {
        self.clock.0.fetch_add(secs, Ordering::SeqCst);
    }
    fn jdir(&self) -> PathBuf {
        self.state.join(format!("ws-{WS}")).join("journal")
    }
}

fn h(b: &[u8]) -> ContentHash {
    ContentHash::of(b)
}

fn plan(tag: u32, originals: &[Vec<u8>]) -> Plan {
    Plan {
        format: 1,
        workspace_id: WS.into(),
        engine_format: 1,
        request: PlanRequest {
            kind: "rewrite".into(),
            summary: format!("change {tag}"),
            note: None,
        },
        files: originals
            .iter()
            .enumerate()
            .map(|(i, o)| PlanFile {
                path: format!("f{i:02}.rs"),
                language: "rust".into(),
                pre_hash: h(o),
                pre_size: o.len() as u64,
                pre_errors: 0,
                post_hash: h(&(o.iter().map(|b| b.wrapping_add(1)).collect::<Vec<_>>())),
                post_size: o.len() as u64,
                post_errors: 0,
                edits: vec![Edit {
                    start: 0,
                    end: 1,
                    replacement: "x".into(),
                }],
            })
            .collect(),
    }
}

/// EDIT5-01: exists is true for every live journal state (Prepared through Applied).
#[test]
fn edit5_01_exists_across_states() {
    let f = Fx::new();
    let s = f.open(Limits::default());
    let o = vec![b"body\n".to_vec()];
    let p = plan(1, &o);
    let id = s.create(&p, &o).unwrap().plan_id;
    assert!(s.exists(&id).unwrap());
    s.set_state(&id, JournalState::Writing, 0, &p).unwrap();
    assert!(s.exists(&id).unwrap());
    s.set_state(&id, JournalState::Applied, 1, &p).unwrap();
    assert!(s.exists(&id).unwrap());
    assert!(!s.exists("p-aaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap());
}

/// EDIT5-02: illegal set_state leaves the on-disk manifest bytes untouched.
#[test]
fn edit5_02_illegal_transition_writes_nothing() {
    let f = Fx::new();
    let s = f.open(Limits::default());
    let o = vec![b"body\n".to_vec()];
    let p = plan(1, &o);
    let id = s.create(&p, &o).unwrap().plan_id;
    let path = f.jdir().join(&id).join("manifest.json");
    let before = fs::read(&path).unwrap();
    let err = s.set_state(&id, JournalState::Applied, 0, &p).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgs);
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(s.load(&id).unwrap().state, JournalState::Prepared);
}

/// EDIT5-03: Undone journals are age-evictable (terminal + recoverable finished).
#[test]
fn edit5_03_undone_is_age_evictable() {
    let f = Fx::new();
    let s = f.open(Limits {
        journal_retention_days: 7,
        ..Limits::default()
    });
    let o = vec![b"body\n".to_vec()];
    let p = plan(1, &o);
    let id = s.create(&p, &o).unwrap().plan_id;
    s.set_state(&id, JournalState::Writing, 0, &p).unwrap();
    s.set_state(&id, JournalState::Applied, 1, &p).unwrap();
    s.set_state(&id, JournalState::Undoing, 0, &p).unwrap();
    s.set_state(&id, JournalState::Undone, 1, &p).unwrap();
    f.advance(7 * DAY);
    assert_eq!(s.evict().unwrap(), vec![id.clone()]);
    assert_eq!(s.load(&id).unwrap_err().code, ErrorCode::PlanNotFound);
}

/// EDIT5-04: short / hostile plan ids never touch the journal directory.
#[test]
fn edit5_04_short_ids_are_refused() {
    let f = Fx::new();
    let s = f.open(Limits::default());
    let o = vec![b"body\n".to_vec()];
    let _ = s.create(&plan(1, &o), &o).unwrap();
    // A short id is refused before the binding is consulted, so this plan is never matched
    // against anything.
    let pb = plan(9, &o);
    let before = fs::read_dir(f.jdir()).unwrap().count();
    for bad in ["p-short", "../x", "", "p-AAAAAAAAAAAAAAAAAAAAAAAAAA"] {
        assert_eq!(
            s.load(bad).unwrap_err().code,
            ErrorCode::InvalidArgs,
            "{bad}"
        );
        assert_eq!(
            s.exists(bad).unwrap_err().code,
            ErrorCode::InvalidArgs,
            "{bad}"
        );
        assert_eq!(
            s.set_state(bad, JournalState::Writing, 0, &pb)
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgs,
            "{bad}"
        );
        assert_eq!(
            s.read_original(bad, 0).unwrap_err().code,
            ErrorCode::InvalidArgs,
            "{bad}"
        );
    }
    assert_eq!(fs::read_dir(f.jdir()).unwrap().count(), before);
}

/// EDIT5-05: create size-make-room still keeps a non-evictable Writing journal.
#[test]
fn edit5_05_create_never_evicts_writing_for_room() {
    let f = Fx::new();
    let s = f.open(Limits {
        journal_max_total_mib: 1,
        journal_retention_days: 365,
        ..Limits::default()
    });
    let big = |c: u8| vec![vec![c; 600 * 1024]];
    let pa = plan(1, &big(b'a'));
    let stuck = s.create(&pa, &big(b'a')).unwrap().plan_id;
    s.set_state(&stuck, JournalState::Writing, 0, &pa).unwrap();
    // applied fills the rest of the budget once Writing is counted
    let applied = s.create(&plan(2, &big(b'b')), &big(b'b'));
    // 600+600 > 1 MiB; Writing is not evictable → create of second must either
    // succeed by not needing eviction of Writing, or fail LimitExceeded — never remove Writing.
    match applied {
        Ok(m) => {
            s.load(&stuck).unwrap();
            s.load(&m.plan_id).unwrap();
        }
        Err(e) => {
            assert_eq!(e.code, ErrorCode::LimitExceeded);
            s.load(&stuck).unwrap();
            assert_eq!(
                fs::read_dir(f.jdir())
                    .unwrap()
                    .filter_map(|e| e.ok())
                    .filter(|e| e.file_name().to_str().is_some_and(|n| !n.starts_with('.')))
                    .count(),
                1
            );
        }
    }
}
