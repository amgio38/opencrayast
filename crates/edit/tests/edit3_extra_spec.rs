//! Extra EDIT3-xx cases for ISSUE-EDIT-3 (do not weaken store_spec).
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_core::ErrorCode;
use opencrayast_core::hash::ContentHash;
use opencrayast_core::limits::Limits;
use opencrayast_edit::{Clock, Edit, Plan, PlanFile, PlanRequest, PlanStore};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const WS: &str = "w-00112233445566778899aabbccddeeff";

struct FakeClock(AtomicU64);
impl Clock for FakeClock {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    state: PathBuf,
    clock: Arc<FakeClock>,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        Fixture {
            _dir: dir,
            state,
            clock: Arc::new(FakeClock(AtomicU64::new(1_000_000))),
        }
    }
    fn open(&self) -> PlanStore {
        PlanStore::open(&self.state, WS, Limits::default(), self.clock.clone()).unwrap()
    }
    fn plans_dir(&self) -> PathBuf {
        self.state.join(format!("ws-{WS}")).join("plans")
    }
}

fn plan(n: u32) -> Plan {
    Plan {
        format: 1,
        workspace_id: WS.into(),
        engine_format: 1,
        request: PlanRequest {
            kind: "rewrite".into(),
            summary: format!("change number {n}"),
            note: None,
        },
        files: vec![PlanFile {
            path: "src/a.ts".into(),
            language: "typescript".into(),
            pre_hash: ContentHash::of(b"before"),
            pre_size: 20,
            pre_errors: 0,
            post_hash: ContentHash::of(b"after"),
            post_size: 31,
            post_errors: 0,
            edits: vec![Edit {
                start: 5,
                end: 12,
                replacement: "logger.debug(a, b)".into(),
            }],
        }],
    }
}

/// EDIT3-01: a plan file without its meta is absent to every reader (plan-before-meta invariant).
#[test]
fn edit3_01_plan_without_meta_is_absent() {
    let f = Fixture::new();
    let s = f.open();
    let (id, _) = s.put(&plan(1)).unwrap();
    fs::remove_file(f.plans_dir().join(format!("{id}.meta.json"))).unwrap();
    assert_eq!(
        s.get_for_write(&id).unwrap_err().code,
        ErrorCode::PlanNotFound
    );
    assert!(s.list().unwrap().0.is_empty());
    s.sweep().unwrap();
    assert!(!f.plans_dir().join(format!("{id}.json")).exists());
}

/// EDIT3-02: two guards on one id; sweep waits until both are dropped.
#[test]
fn edit3_02_nested_use_guards_block_sweep() {
    let f = Fixture::new();
    let s = f.open();
    let (id, _) = s.put(&plan(0)).unwrap();
    let g1 = s.begin_use(&id).unwrap();
    let g2 = s.begin_use(&id).unwrap();
    f.clock.0.fetch_add(10_000, Ordering::SeqCst);
    assert_eq!(s.sweep().unwrap(), 0);
    drop(g1);
    assert_eq!(s.sweep().unwrap(), 0);
    drop(g2);
    assert_eq!(s.sweep().unwrap(), 1);
}

/// EDIT3-03: corrupt-read messages never quote replacement text from the file.
#[test]
fn edit3_03_corrupt_messages_never_quote_content() {
    let f = Fixture::new();
    let s = f.open();
    let (id, _) = s.put(&plan(1)).unwrap();
    let path = f.plans_dir().join(format!("{id}.json"));
    let marker = "EVIL_PAYLOAD_SHOULD_NOT_LEAK";
    let good = String::from_utf8(fs::read(&path).unwrap()).unwrap();
    let evil = good.replace("logger.debug(a, b)", marker);
    fs::write(&path, evil).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let e = s.get_for_write(&id).unwrap_err();
    assert_eq!(e.code, ErrorCode::PlanCorrupt);
    assert!(
        !e.message.contains(marker),
        "must not quote content: {}",
        e.message
    );
}

/// EDIT3-04: leftover temp files with the store prefix are removed by sweep.
#[test]
fn edit3_04_sweep_removes_temp_leftovers() {
    let f = Fixture::new();
    let s = f.open();
    s.put(&plan(1)).unwrap();
    let tmp = f.plans_dir().join(".opencrayast-plan-tmp-orphan-leftover");
    fs::write(&tmp, b"junk").unwrap();
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600)).unwrap();
    s.sweep().unwrap();
    assert!(!tmp.exists());
}

/// EDIT3-05: after open, a replaced `plans/` directory is refused (`io_error`); nothing written through.
#[test]
fn edit3_05_replaced_plans_dir_is_io_error() {
    let f = Fixture::new();
    let s = f.open();
    s.put(&plan(1)).unwrap();
    let dir = f.plans_dir();
    let elsewhere = f.state.join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fs::rename(&dir, f.state.join("moved")).unwrap();
    symlink(&elsewhere, &dir).unwrap();
    assert_eq!(s.put(&plan(2)).unwrap_err().code, ErrorCode::IoError);
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
}

/// EDIT3-06: unverifiable entries are reclaimed by sweep (not in use).
#[test]
fn edit3_06_unverifiable_entry_is_swept() {
    let f = Fixture::new();
    let s = f.open();
    let (id, _) = s.put(&plan(1)).unwrap();
    fs::write(f.plans_dir().join(format!("{id}.meta.json")), b"garbage").unwrap();
    assert_eq!(s.sweep().unwrap(), 1);
    assert!(fs::read_dir(f.plans_dir()).unwrap().next().is_none());
}
