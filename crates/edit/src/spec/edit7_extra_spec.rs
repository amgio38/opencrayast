//! Extra EDIT7-xx cases for EDIT-7 (do not weaken apply_spec).
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::{
    ApplyContext, Clock, Edit, Fault, FaultAction, JournalState, JournalStore, NoFault, Plan,
    PlanFile, PlanRequest, PlanStore, Step, StepKind, apply, apply_edits, journal_of, recover,
};
use opencrayast_core::boundary::{Boundary, BoundaryConfig};
use opencrayast_core::hash::ContentHash;
use opencrayast_core::limits::Limits;
use opencrayast_core::workspace::workspace_id;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

struct FakeClock(AtomicU64);
impl Clock for FakeClock {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct Hook {
    n: AtomicUsize,
    #[allow(clippy::type_complexity)]
    f: Box<dyn Fn(usize, &Step) -> FaultAction + Send + Sync>,
}
impl Hook {
    fn at(k: usize, action: FaultAction) -> Hook {
        Hook {
            n: AtomicUsize::new(0),
            f: Box::new(move |n, _| {
                if n == k {
                    action.clone()
                } else {
                    FaultAction::Continue
                }
            }),
        }
    }
    fn recorder(log: Arc<Mutex<Vec<Step>>>) -> Hook {
        Hook {
            n: AtomicUsize::new(0),
            f: Box::new(move |_, s| {
                log.lock().unwrap().push(*s);
                FaultAction::Continue
            }),
        }
    }
}
impl Fault for Hook {
    fn at(&self, step: &Step) -> FaultAction {
        let n = self.n.fetch_add(1, Ordering::SeqCst);
        (self.f)(n, step)
    }
}

struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
    state: PathBuf,
    ws: String,
    boundary: Boundary,
    plans: PlanStore,
    journals: JournalStore,
    limits: Limits,
    clock: Arc<FakeClock>,
}

impl World {
    fn new() -> World {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        fs::create_dir(&root).unwrap();
        let state = dir.path().join("state");
        let ws = workspace_id(&root).unwrap();
        let clock = Arc::new(FakeClock(AtomicU64::new(1_000_000)));
        let limits = Limits::default();
        let boundary = Boundary::new(BoundaryConfig {
            root: root.clone(),
            state_dir: Some(state.clone()),
            limits: Limits::default(),
            read_roots: Vec::new(),
            extra_protected: Vec::new(),
        })
        .unwrap();
        let plans = PlanStore::open(&state, &ws, limits.clone(), clock.clone()).unwrap();
        let journals = JournalStore::open(&state, &ws, limits.clone(), clock.clone()).unwrap();
        World {
            _dir: dir,
            root,
            state,
            ws,
            boundary,
            plans,
            journals,
            limits,
            clock,
        }
    }
    fn restart(&mut self) {
        self.plans = PlanStore::open(
            &self.state,
            &self.ws,
            self.limits.clone(),
            self.clock.clone(),
        )
        .unwrap();
        self.journals = JournalStore::open(
            &self.state,
            &self.ws,
            self.limits.clone(),
            self.clock.clone(),
        )
        .unwrap();
    }
    fn ctx<'a>(&'a self) -> ApplyContext<'a> {
        self.ctx_fault(&NoFault)
    }
    fn ctx_fault<'a>(&'a self, fault: &'a dyn Fault) -> ApplyContext<'a> {
        ApplyContext::new(
            &self.boundary,
            &self.plans,
            &self.journals,
            &self.limits,
            &self.state,
            &self.ws,
            Some(crate::policy::enable_writes()),
            Duration::from_secs(5),
            fault,
        )
    }
    fn write(&self, rel: &str, bytes: &[u8]) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
    }
    fn plan_one(&self, rel: &str, edits: Vec<Edit>) -> String {
        let bytes = fs::read(self.root.join(rel)).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        let new = apply_edits(&text, &edits).unwrap();
        let p = Plan {
            format: 1,
            workspace_id: self.ws.clone(),
            engine_format: 1,
            request: PlanRequest {
                kind: "rewrite".into(),
                summary: "extra".into(),
                note: None,
            },
            files: vec![PlanFile {
                path: rel.into(),
                language: "rust".into(),
                pre_hash: ContentHash::of(&bytes),
                pre_size: bytes.len() as u64,
                pre_errors: 0,
                post_hash: ContentHash::of(new.as_bytes()),
                post_size: new.len() as u64,
                post_errors: 0,
                edits,
            }],
        };
        self.plans.put(&p).unwrap().0
    }
    fn plan_two(&self) -> String {
        let mk = |rel: &str, src: &str, from: &str, to: &str| {
            let edits = vec![rep(src, from, to)];
            let new = apply_edits(src, &edits).unwrap();
            PlanFile {
                path: rel.into(),
                language: "rust".into(),
                pre_hash: ContentHash::of(src.as_bytes()),
                pre_size: src.len() as u64,
                pre_errors: 0,
                post_hash: ContentHash::of(new.as_bytes()),
                post_size: new.len() as u64,
                post_errors: 0,
                edits,
            }
        };
        let p = Plan {
            format: 1,
            workspace_id: self.ws.clone(),
            engine_format: 1,
            request: PlanRequest {
                kind: "rewrite".into(),
                summary: "two".into(),
                note: None,
            },
            files: vec![
                mk("a.rs", "fn one() { 1 }\n", "1", "11"),
                mk("b.rs", "fn two() { 2 }\n", "2", "22"),
            ],
        };
        self.plans.put(&p).unwrap().0
    }
}

fn rep(src: &str, needle: &str, with: &str) -> Edit {
    let i = src.find(needle).unwrap();
    Edit {
        start: i,
        end: i + needle.len(),
        replacement: with.into(),
    }
}

/// EDIT7-01: journal_of returns the applied manifest.
#[test]
fn edit7_01_journal_of_after_apply() {
    let w = World::new();
    w.write("a.rs", b"fn one() { 1 }\n");
    let id = w.plan_one("a.rs", vec![rep("fn one() { 1 }\n", "1", "11")]);
    apply(&w.ctx(), &id).unwrap();
    let m = journal_of(&w.ctx(), &id).unwrap();
    assert_eq!(m.state, JournalState::Applied);
    assert_eq!(m.progress, 1);
}

/// EDIT7-02: recover on a clean workspace is empty and idempotent.
#[test]
fn edit7_02_recover_empty_is_idempotent() {
    let w = World::new();
    assert!(recover(&w.ctx()).unwrap().is_empty());
    assert!(recover(&w.ctx()).unwrap().is_empty());
}

/// EDIT7-03: ApplyResult.changed is ascending by path.
#[test]
fn edit7_03_changed_paths_are_sorted() {
    let w = World::new();
    w.write("z.rs", b"fn z() { 1 }\n");
    w.write("a.rs", b"fn a() { 1 }\n");
    let mk = |rel: &str, src: &str| {
        let edits = vec![rep(src, "1", "2")];
        let new = apply_edits(src, &edits).unwrap();
        PlanFile {
            path: rel.into(),
            language: "rust".into(),
            pre_hash: ContentHash::of(src.as_bytes()),
            pre_size: src.len() as u64,
            pre_errors: 0,
            post_hash: ContentHash::of(new.as_bytes()),
            post_size: new.len() as u64,
            post_errors: 0,
            edits,
        }
    };
    let p = Plan {
        format: 1,
        workspace_id: w.ws.clone(),
        engine_format: 1,
        request: PlanRequest {
            kind: "rewrite".into(),
            summary: "two".into(),
            note: None,
        },
        files: vec![mk("a.rs", "fn a() { 1 }\n"), mk("z.rs", "fn z() { 1 }\n")],
    };
    let id = w.plans.put(&p).unwrap().0;
    let res = apply(&w.ctx(), &id).unwrap();
    assert_eq!(res.changed, vec!["a.rs".to_string(), "z.rs".to_string()]);
}

/// EDIT7-04: suggestion is non-empty and does not embed absolute workspace paths.
#[test]
fn edit7_04_suggestion_has_no_absolute_path() {
    let w = World::new();
    w.write("a.rs", b"fn one() { 1 }\n");
    let id = w.plan_one("a.rs", vec![rep("fn one() { 1 }\n", "1", "11")]);
    let res = apply(&w.ctx(), &id).unwrap();
    assert!(!res.suggestion.is_empty());
    assert!(
        !res.suggestion.contains(w.root.to_str().unwrap()),
        "{}",
        res.suggestion
    );
}

/// EDIT7-05: after a mid-recovery crash, the second recover leaves an already-restored file's
/// mtime untouched (`pre_hash` → skip rewrite in `restore_one` / classify omits Pre files).
#[test]
fn edit7_05_restore_skips_pre_hash_and_does_not_rewrite() {
    // Clean apply steps → crash index for MarkApplied (both files Post afterwards).
    let mark_applied = {
        let w = World::new();
        w.write("a.rs", b"fn one() { 1 }\n");
        w.write("b.rs", b"fn two() { 2 }\n");
        let id = w.plan_two();
        let log = Arc::new(Mutex::new(Vec::new()));
        apply(&w.ctx_fault(&Hook::recorder(log.clone())), &id).unwrap();
        log.lock()
            .unwrap()
            .iter()
            .position(|s| s.kind == StepKind::MarkApplied)
            .unwrap()
    };

    // Recovery steps after that crash → crash index for Restore(1).
    let restore1 = {
        let mut w = World::new();
        w.write("a.rs", b"fn one() { 1 }\n");
        w.write("b.rs", b"fn two() { 2 }\n");
        let id = w.plan_two();
        let _ = apply(
            &w.ctx_fault(&Hook::at(mark_applied, FaultAction::Crash)),
            &id,
        );
        w.restart();
        let log = Arc::new(Mutex::new(Vec::new()));
        recover(&w.ctx_fault(&Hook::recorder(log.clone()))).unwrap();
        log.lock()
            .unwrap()
            .iter()
            .position(|s| s.kind == StepKind::Restore && s.index == 1)
            .expect("Restore(1)")
    };

    let mut w = World::new();
    w.write("a.rs", b"fn one() { 1 }\n");
    w.write("b.rs", b"fn two() { 2 }\n");
    let id = w.plan_two();
    let _ = apply(
        &w.ctx_fault(&Hook::at(mark_applied, FaultAction::Crash)),
        &id,
    );
    w.restart();
    let _ = recover(&w.ctx_fault(&Hook::at(restore1, FaultAction::Crash)));
    w.restart();

    let a_path = w.root.join("a.rs");
    assert_eq!(fs::read(&a_path).unwrap(), b"fn one() { 1 }\n");
    let mtime_before = fs::metadata(&a_path).unwrap().mtime();
    thread::sleep(Duration::from_millis(25));

    let done = recover(&w.ctx()).unwrap();
    assert_eq!(done.len(), 1);
    assert_eq!(
        w.journals.load(&id).unwrap().state,
        JournalState::RolledBack
    );
    assert_eq!(fs::read(&a_path).unwrap(), b"fn one() { 1 }\n");
    assert_eq!(
        fs::metadata(&a_path).unwrap().mtime(),
        mtime_before,
        "already-restored a.rs must not be rewritten"
    );
    assert_eq!(fs::read(w.root.join("b.rs")).unwrap(), b"fn two() { 2 }\n");
}
