//! SECFIX4: write capability is a type, not a public bool.
//!
//! The F-02 audit PoC flipped `ApplyContext.write_enabled` from false to true. That field is
//! gone; these tests lock the replacement contract.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::{
    ApplyContext, Clock, Edit, JournalStore, NoFault, Plan, PlanFile, PlanRequest, PlanStore,
    apply, apply_edits, policy, recover, undo,
};
use opencrayast_core::ErrorCode;
use opencrayast_core::boundary::{Boundary, BoundaryConfig};
use opencrayast_core::error::ToolError;
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

    fn ctx_write(&self) -> ApplyContext<'_> {
        ApplyContext::new(
            &self.boundary,
            &self.plans,
            &self.journals,
            &self.limits,
            &self.state,
            &self.ws,
            Some(policy::enable_writes()),
            Duration::from_secs(5),
            &NoFault,
        )
    }

    fn ctx_read(&self) -> ApplyContext<'_> {
        ApplyContext::new(
            &self.boundary,
            &self.plans,
            &self.journals,
            &self.limits,
            &self.state,
            &self.ws,
            None,
            Duration::from_secs(5),
            &NoFault,
        )
    }

    fn plan_replace(&self, rel: &str, old: &str, new: &str) -> String {
        self.write(rel, old.as_bytes());
        let edits = vec![Edit {
            start: 0,
            end: old.len(),
            replacement: new.into(),
        }];
        let got = apply_edits(old, &edits).unwrap();
        assert_eq!(got, new);
        let plan = Plan {
            format: 1,
            workspace_id: self.ws.clone(),
            engine_format: 1,
            request: PlanRequest {
                kind: "rewrite".into(),
                summary: "secfix4".into(),
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

fn code<T>(r: Result<T, ToolError>) -> ErrorCode {
    r.err().unwrap().code
}

/// SECFIX4-01: the public forge path is **gone**.
///
/// Dependents cannot call `policy::enable_writes` or `WriteCap::mint` — both are
/// `pub(crate)`, locked by the `compile_fail` doctests on [`crate::WriteCap`].
/// This in-crate check only proves the *internal* grant still works (write mode
/// is not dead). Mutation self-proof: change `pub(crate) fn enable_writes` to
/// `pub fn enable_writes` → those doctests go red (compile_fail starts compiling).
#[test]
fn secfix4_01_public_forge_path_is_closed() {
    // Crate-internal grant (shells / unit tests). Not a public API.
    let _cap = policy::enable_writes();
}

/// SECFIX4-02: read context reports no grant; write context requires an explicit capability.
#[test]
fn secfix4_02_no_public_bool_upgrade() {
    let w = World::new();
    assert!(!w.ctx_read().write_granted());
    assert!(w.ctx_write().write_granted());
}

/// SECFIX4-03: without WriteCap, apply / undo / recover all return write_disabled
/// and leave workspace bytes unchanged (the F-02 PoC inverted).
#[test]
fn secfix4_03_read_mode_blocks_all_three_write_entries() {
    let w = World::new();
    let id = w.plan_replace("a.txt", "hello", "HELLO");
    let before = w.read("a.txt");
    let ctx = w.ctx_read();
    assert_eq!(code(apply(&ctx, &id)), ErrorCode::WriteDisabled);
    assert_eq!(code(undo(&ctx, &id)), ErrorCode::WriteDisabled);
    assert_eq!(code(recover(&ctx)), ErrorCode::WriteDisabled);
    assert_eq!(w.read("a.txt"), before);
}

/// SECFIX4-04: with a WriteCap, apply succeeds (smoke that write mode still works).
#[test]
fn secfix4_04_write_cap_allows_apply() {
    let w = World::new();
    let id = w.plan_replace("a.txt", "hello", "HELLO");
    let res = apply(&w.ctx_write(), &id).unwrap();
    assert_eq!(res.changed, vec!["a.txt".to_string()]);
    assert_eq!(w.read("a.txt"), b"HELLO");
}
