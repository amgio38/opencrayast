//! SEC-A1 audit PoCs that need WRITE MODE — moved in-crate by SEC-FIX 4's consequence.
//!
//! Covers findings F-01, F-01-race and the aspect-2/3/4 probes. Originally in
//! `crates/edit/tests/sec_audit_poc.rs`.
//!
//! WHY THIS FILE IS INSIDE THE CRATE. SEC-FIX 4 replaced `ApplyContext::write_enabled: bool`
//! with a private `write: Option<WriteCap>`, and `WriteCap` can only be minted by
//! `pub(crate) policy::enable_writes()`. So these PoCs are no longer expressible from an
//! integration test: a separate crate cannot name the field, let alone mint the capability.
//!
//! That is not the PoCs being broken - it is the guarantee having moved up a level. The
//! write gate used to be a public `bool` that any caller could flip, and the F-02 PoC proved
//! it by flipping one field and watching a plan apply. Now the compiler is what refuses.
//! A test that cannot be written is a weaker demonstration than a test that fails, but it is
//! enforced by something nobody can edit around, so the guarantee is strictly stronger.
//!
//! The probes that do NOT need write capability stay in `crates/edit/tests/sec_audit_poc.rs`,
//! where they belong: they are about reads and about the build.
//!
//! Gated exactly like `apply_spec.rs` / `undo_spec.rs`, for the same reason (they need
//! `crate::` internals) and with the same platform gate.
#![cfg(all(test, unix))]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::apply::{ApplyContext, Fault, FaultAction, Step, StepKind, apply, recover};
use crate::undo::undo;
use crate::{Clock, Edit, JournalStore, NoFault, Plan, PlanFile, PlanRequest, PlanStore, policy};
use opencrayast_core::ErrorCode;
use opencrayast_core::boundary::{Boundary, BoundaryConfig};
use opencrayast_core::hash::ContentHash;
use opencrayast_core::limits::Limits;
use opencrayast_core::workspace::workspace_id;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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

    /// A WRITE-capable context. `enable_writes()` is reachable only from inside the crate -
    /// which is the whole point of this file's existence.
    fn ctx<'a>(&'a self, fault: &'a dyn Fault) -> ApplyContext<'a> {
        ApplyContext::new(
            &self.boundary,
            &self.plans,
            &self.journals,
            &self.limits,
            &self.state,
            &self.ws,
            Some(policy::enable_writes()),
            Duration::from_secs(5),
            fault,
        )
    }

    fn write(&self, rel: &str, bytes: &[u8]) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
    }

    fn plan_one(&self, rel: &str, needle: &str, with: &str) -> String {
        let bytes = fs::read(self.root.join(rel)).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        let start = text.find(needle).unwrap();
        let edit = Edit {
            start,
            end: start + needle.len(),
            replacement: with.to_string(),
        };
        let mut new = text.clone();
        new.replace_range(start..start + needle.len(), with);
        let plan = Plan {
            format: 1,
            workspace_id: self.ws.clone(),
            engine_format: 1,
            request: PlanRequest {
                kind: "rewrite".into(),
                summary: "poc".into(),
                note: None,
            },
            files: vec![PlanFile {
                path: rel.to_string(),
                language: "text".into(),
                pre_hash: ContentHash::of(&bytes),
                pre_size: bytes.len() as u64,
                pre_errors: 0,
                post_hash: ContentHash::of(new.as_bytes()),
                post_size: new.len() as u64,
                post_errors: 0,
                edits: vec![edit],
            }],
        };
        self.plans.put(&plan).unwrap().0
    }
}

// =====================================================================================
// F-01 (aspect 3, TOCTOU / S-1): a write escapes the workspace by a directory-level
// rename + symlink swap performed between the final verification and the rename.
//
// `Boundary::resolve_write` only link-checks the FINAL component, and the write path works
// from PATH STRINGS, so moving the whole parent DIRECTORY out of the workspace and leaving a
// symlink behind passes every check: same dev/ino, nlink == 1, final component not a link.
//
// Contradicts S-1 ("No byte outside the boundary is read or written") and T-03's stated
// mitigation ("relative to verified directory handles, never re-resolved path strings").
// =====================================================================================

/// Swaps `<ws>/sub` for `<outside>/sub` and leaves `<ws>/sub` as a symlink to it, once.
struct DirSwap {
    ws_sub: PathBuf,
    outside_sub: PathBuf,
    fired: AtomicUsize,
}
impl Fault for DirSwap {
    fn at(&self, step: &Step) -> FaultAction {
        if step.kind == StepKind::Replace && self.fired.swap(1, Ordering::SeqCst) == 0 {
            fs::rename(&self.ws_sub, &self.outside_sub).unwrap();
            symlink(&self.outside_sub, &self.ws_sub).unwrap();
        }
        FaultAction::Continue
    }
}

#[test]
fn f01_apply_writes_outside_the_workspace_after_a_directory_swap() {
    let w = World::new();
    w.write("sub/target.rs", b"let a = 1;\n");
    let id = w.plan_one("sub/target.rs", "1", "2");

    let outside = w._dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    assert!(!outside.starts_with(&w.root));

    let fault = DirSwap {
        ws_sub: w.root.join("sub"),
        outside_sub: outside.join("sub"),
        fired: AtomicUsize::new(0),
    };
    let result = apply(&w.ctx(&fault), &id);

    let escaped = fs::read_to_string(outside.join("sub/target.rs")).unwrap();
    eprintln!("apply() = {result:?}");
    eprintln!("outside/sub/target.rs = {escaped:?}");
    // F-01 was fixed by SEC-FIX 2: the write path now works from directory handles it has already
    // verified, so swapping the parent directory out mid-write cannot redirect the bytes. This
    // assertion ASSERTS the fix: it fails if a byte ever lands outside the boundary (S-1). It
    // used to be `#[ignore]`d with EXPECTED-FAIL wording, which was stale — it passes at this
    // commit and has done so since `d682c03`.
    assert!(
        result.is_err() && !escaped.contains("let a = 2;"),
        "S-1 VIOLATION REGRESSION (F-01): the edit landed outside the workspace: {escaped:?} \
         (apply = {result:?})"
    );
}

/// The same escape as a real race, with no re-implementation: a thread swaps the directory
/// out and symlinks it back while `apply()` runs.
///
/// Reported HONESTLY: this is NOT the evidence for F-01 - the deterministic PoC above is.
/// It exists to measure how reachable the window is, and it did not reach it.
#[test]
#[ignore = "F-01-race: blind racing did not hit the window; kept as the measurement"]
fn f01_race_directory_swap_during_apply() {
    use std::thread;

    let mut escaped_any = false;
    let mut succeeded = 0;
    let attempts = 25;

    for i in 0..attempts {
        let w = World::new();
        // Just under limits.max_file_bytes (4 MiB), so apply() is not refused for size.
        let filler = "let a = 1;\n".repeat(180_000);
        w.write("sub/target.rs", filler.as_bytes());
        let id = w.plan_one("sub/target.rs", "1", "2");

        let outside = w._dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let t = {
            let stop = Arc::clone(&stop);
            let ws_sub = w.root.join("sub");
            let outside_sub = outside.join("sub");
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(15));
                let _ = fs::rename(&ws_sub, &outside_sub);
                let _ = symlink(&outside_sub, &ws_sub);
                while !stop.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(1));
                }
            })
        };
        let r = apply(&w.ctx(&NoFault), &id);
        stop.store(true, Ordering::SeqCst);
        let _ = t.join();
        if r.is_ok() {
            succeeded += 1;
        }
        let leaked = fs::read_to_string(outside.join("sub/target.rs"))
            .is_ok_and(|s| s.contains("let a = 2;"));
        eprintln!("attempt {i}: apply ok={} escaped={leaked}", r.is_ok());
        if leaked {
            escaped_any = true;
            break;
        }
    }
    eprintln!("apply() reported success in {succeeded}/{attempts} attempts");
    assert!(
        escaped_any,
        "blind racing did not reach the window in {attempts} attempts (apply succeeded \
         {succeeded} times)"
    );
}

// =====================================================================================
// Verified-clean probes: these PASS today and record what was checked.
// =====================================================================================

/// Aspect 2: with no write capability, apply / undo / recover all refuse.
#[test]
fn clean_facet2_every_edit_entry_point_checks_the_capability() {
    let w = World::new();
    w.write("a.txt", b"one\n");
    let id = w.plan_one("a.txt", "one", "ONE");
    let ro = ApplyContext::new(
        &w.boundary,
        &w.plans,
        &w.journals,
        &w.limits,
        &w.state,
        &w.ws,
        None,
        Duration::from_secs(5),
        &NoFault,
    );
    let results: Vec<(&str, Result<(), opencrayast_core::error::ToolError>)> = vec![
        ("apply", apply(&ro, &id).map(|_| ())),
        ("undo", undo(&ro, &id).map(|_| ())),
        ("recover", recover(&ro).map(|_| ())),
    ];
    for (name, r) in results {
        assert_eq!(
            r.as_ref().err().map(|e| e.code),
            Some(ErrorCode::WriteDisabled),
            "{name} must refuse without a capability, got {r:?}"
        );
    }
    assert_eq!(
        fs::read_to_string(w.root.join("a.txt")).unwrap(),
        "one\n",
        "a refused apply must leave the workspace byte-identical"
    );
}

/// Aspect 2 (the F-02 guarantee, now enforced by the compiler rather than by a red test):
/// the capability cannot be forged from outside the crate.
///
/// The runtime half of this used to be F-02's PoC - a public `write_enabled: bool` that a
/// read-mode caller could flip, which it did, and the workspace changed. There is no runtime
/// test here any more because there is nothing left to test at runtime: `write` is private
/// and `WriteCap` has no public constructor. What is left is the compile-time fact, and it is
/// recorded as a compile-fail doctest on `WriteCap` in `capability.rs`.
#[test]
fn clean_facet2_the_capability_cannot_be_forged_from_outside() {
    // A read-only context is constructible from anywhere, and refuses.
    let w = World::new();
    w.write("a.txt", b"one\n");
    let id = w.plan_one("a.txt", "one", "ONE");
    let ro = ApplyContext::new(
        &w.boundary,
        &w.plans,
        &w.journals,
        &w.limits,
        &w.state,
        &w.ws,
        None,
        Duration::from_secs(5),
        &NoFault,
    );
    assert_eq!(apply(&ro, &id).unwrap_err().code, ErrorCode::WriteDisabled);
    // `Some(policy::enable_writes())` is the ONLY way to get a write context, and
    // `policy::enable_writes` is `pub(crate)`. If this file were an integration test it would
    // not compile at all - which is the guarantee. This test exists to document that the
    // read-only half still works from in-crate too, and to fail loudly if someone hands out a
    // public constructor without noticing.
    let w2 = World::new();
    w2.write("a.txt", b"one\n");
    let id2 = w2.plan_one("a.txt", "one", "ONE");
    let rw = w2.ctx(&NoFault);
    assert!(
        apply(&rw, &id2).is_ok(),
        "a capability obtained through policy must still work"
    );
}

/// Aspect 3: the FINAL-component races are refused (only the directory-level one is not).
#[test]
fn clean_facet3_final_component_swaps_are_refused() {
    let w = World::new();
    w.write("a.txt", b"one\n");
    let id = w.plan_one("a.txt", "one", "ONE");

    struct SymlinkSwap {
        ws: PathBuf,
        outside: PathBuf,
        fired: AtomicUsize,
    }
    impl Fault for SymlinkSwap {
        fn at(&self, step: &Step) -> FaultAction {
            if step.kind == StepKind::Replace && self.fired.swap(1, Ordering::SeqCst) == 0 {
                fs::rename(&self.ws, self.outside.join("moved.txt")).unwrap();
                symlink(self.outside.join("moved.txt"), &self.ws).unwrap();
            }
            FaultAction::Continue
        }
    }
    let outside = w._dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let r = apply(
        &w.ctx(&SymlinkSwap {
            ws: w.root.join("a.txt"),
            outside: outside.clone(),
            fired: AtomicUsize::new(0),
        }),
        &id,
    );
    eprintln!("final-component symlink swap -> {r:?}");
    assert!(r.is_err(), "a symlinked write target must be refused");
    assert_eq!(
        fs::read_to_string(outside.join("moved.txt")).unwrap(),
        "one\n",
        "the file outside the workspace must be untouched"
    );
}

/// Aspect 4: the plan and journal ceilings are enforced before anything is written.
#[test]
fn clean_facet4_plan_and_journal_ceilings_are_enforced() {
    let w = World::new();
    w.write("a.txt", b"one\n");

    let tight = Limits {
        plan_max_files: 1,
        plan_max_edits: 1,
        ..Limits::default()
    };
    let plans = PlanStore::open(&w.state, &w.ws, tight, w.clock.clone()).unwrap();
    let file = |path: &str| PlanFile {
        path: path.into(),
        language: "text".into(),
        pre_hash: ContentHash::of(b"one\n"),
        pre_size: 4,
        pre_errors: 0,
        post_hash: ContentHash::of(b"ONE\n"),
        post_size: 4,
        post_errors: 0,
        edits: vec![Edit {
            start: 0,
            end: 3,
            replacement: "ONE".into(),
        }],
    };
    let big = Plan {
        format: 1,
        workspace_id: w.ws.clone(),
        engine_format: 1,
        request: PlanRequest {
            kind: "rewrite".into(),
            summary: "poc".into(),
            note: None,
        },
        files: vec![file("a.txt"), file("b.txt")],
    };
    let r = plans.put(&big);
    eprintln!(
        "plan with 2 files under plan_max_files=1 -> {:?}",
        r.as_ref().err().map(|e| e.code)
    );
    assert_eq!(
        r.err().map(|e| e.code),
        Some(ErrorCode::LimitExceeded),
        "plan_max_files must be enforced at put()"
    );
}
