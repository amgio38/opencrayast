//! EDIT-11: apply re-runs the shared encoding gate (BOM / line ending / trailing newline).
//!
//! Preview already refuses these via `encoding_gate`. These cases hand-build a plan that
//! *would* flip an attribute (bypassing preview), store it, and prove apply refuses with
//! the same reason strings — workspace untouched, no journal created.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::{
    ApplyContext, Clock, Edit, JournalStore, NoFault, Plan, PlanFile, PlanRequest, PlanStore,
    apply, apply_edits,
};
use opencrayast_core::ErrorCode;
use opencrayast_core::boundary::{Boundary, BoundaryConfig};
use opencrayast_core::hash::ContentHash;
use opencrayast_core::limits::Limits;
use opencrayast_core::workspace::workspace_id;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

struct FakeClock(AtomicU64);
impl Clock for FakeClock {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
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
    _clock: Arc<FakeClock>,
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
            _clock: clock,
        }
    }

    fn write(&self, rel: &str, bytes: &[u8]) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
    }

    fn read(&self, rel: &str) -> Vec<u8> {
        fs::read(self.root.join(rel)).unwrap()
    }

    fn ctx(&self) -> ApplyContext<'_> {
        ApplyContext::new(
            &self.boundary,
            &self.plans,
            &self.journals,
            &self.limits,
            &self.state,
            &self.ws,
            Some(crate::policy::enable_writes()),
            Duration::from_secs(5),
            &NoFault,
        )
    }

    /// Store a plan whose recorded edits turn `old` into `new` (must match `apply_edits`).
    /// Language is `"text"` so the syntax gate is skipped; encoding is still checked.
    fn put_forged(&self, rel: &str, old: &str, new: &str) -> String {
        self.write(rel, old.as_bytes());
        let edits = vec![Edit {
            start: 0,
            end: old.len(),
            replacement: new.into(),
        }];
        assert_eq!(apply_edits(old, &edits).unwrap(), new);
        let plan = Plan {
            format: 1,
            workspace_id: self.ws.clone(),
            engine_format: 1,
            request: PlanRequest {
                kind: "rewrite".into(),
                summary: "edit11 forged encoding flip".into(),
                note: None,
            },
            files: vec![PlanFile {
                path: rel.into(),
                language: "text".into(),
                pre_hash: ContentHash::of(old.as_bytes()),
                pre_size: old.len() as u64,
                pre_errors: 0,
                post_hash: ContentHash::of(new.as_bytes()),
                post_size: new.len() as u64,
                post_errors: 0,
                edits,
            }],
        };
        self.plans.put(&plan).unwrap().0
    }
}

/// EDIT11-01: a forged plan that adds a trailing newline is refused at apply with the
/// shared reason string; workspace bytes and the journal store are unchanged.
#[test]
fn edit11_01_apply_refuses_forged_trailing_newline_flip() {
    let w = World::new();
    let id = w.put_forged("a.txt", "log(1)", "log(1)\n");
    let before = w.read("a.txt");
    assert!(!before.ends_with(b"\n"));

    let err = apply(&w.ctx(), &id).unwrap_err();
    assert_eq!(err.code, ErrorCode::GateFailed, "{err:?}");
    assert!(
        err.message.contains("encoding") && err.message.contains("trailing newline"),
        "same reason fragment as preview: {:?}",
        err.message
    );
    assert_eq!(w.read("a.txt"), before, "workspace must be untouched");
    assert!(
        !w.journals.exists(&id).unwrap(),
        "no journal may be created for a gate refusal"
    );
}

/// EDIT11-02: a forged plan that flattens CRLF to LF is refused naming `line ending`.
#[test]
fn edit11_02_apply_refuses_forged_crlf_to_lf_flip() {
    let w = World::new();
    let old = "a\r\nb\r\n";
    let new = "a\nb\n";
    let id = w.put_forged("b.txt", old, new);
    let before = w.read("b.txt");
    assert_eq!(before, old.as_bytes());

    let err = apply(&w.ctx(), &id).unwrap_err();
    assert_eq!(err.code, ErrorCode::GateFailed, "{err:?}");
    assert!(
        err.message.contains("encoding") && err.message.contains("line ending"),
        "same reason fragment as preview: {:?}",
        err.message
    );
    assert_eq!(w.read("b.txt"), before);
    assert!(!w.journals.exists(&id).unwrap());
}

/// EDIT11-03: an encoding-preserving forged plan still applies (the gate is not closed shut).
#[test]
fn edit11_03_apply_accepts_encoding_preserving_forged_plan() {
    let w = World::new();
    let id = w.put_forged("c.txt", "log(1)\n", "log(2)\n");
    let res = apply(&w.ctx(), &id).unwrap();
    assert_eq!(res.changed, vec!["c.txt".to_string()]);
    assert_eq!(w.read("c.txt"), b"log(2)\n");
    assert!(w.journals.exists(&id).unwrap());
}
