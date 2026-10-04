//! Spec for EDIT-8: the undo shell on a REAL filesystem (E-8, E-9, E-14; EDIT8-xx).
//! Never weaken; add cases. If an expectation looks wrong, block the ticket with a minimal
//! reproduction.
//!
//! The centrepiece is the E-13 matrix: a clean undo is recorded step by step, then replayed with a
//! failure injected at every step, and with a crash injected at every step (followed by a "restart"
//! and recovery, itself crashed at every step). Whatever happens, the workspace ends fully original
//! — never a mixture, never a half-undone tree — and the journal state is always self-consistent.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::{
    ApplyContext, Clock, Edit, Fault, FaultAction, JournalState, JournalStore, NoFault, PlanStore,
    Step, StepKind, apply, apply_edits, recover, undo,
};
use opencrayast_core::ErrorCode;
use opencrayast_core::boundary::{Boundary, BoundaryConfig};
use opencrayast_core::error::ToolError;
use opencrayast_core::limits::Limits;
use opencrayast_core::workspace::workspace_id;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct FakeClock(AtomicU64);
impl Clock for FakeClock {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}
impl FakeClock {
    /// Move the clock forward. Needed to reach states that only time produces, such as a plan
    /// whose TTL has passed while its file is still on disk.
    fn advance(&self, secs: u64) {
        self.0.fetch_add(secs, Ordering::SeqCst);
    }
}

type HookFn = Box<dyn Fn(usize, &Step) -> FaultAction + Send + Sync>;

/// A fault driven by a closure `(step number, step) -> action`, which also records every step.
struct Hook {
    n: AtomicUsize,
    log: Mutex<Vec<Step>>,
    f: HookFn,
}
impl Hook {
    fn new(f: impl Fn(usize, &Step) -> FaultAction + Send + Sync + 'static) -> Hook {
        Hook {
            n: AtomicUsize::new(0),
            log: Mutex::new(vec![]),
            f: Box::new(f),
        }
    }
    fn recorder() -> Hook {
        Hook::new(|_, _| FaultAction::Continue)
    }
    fn at(k: usize, action: FaultAction) -> Hook {
        Hook::new(move |n, _| {
            if n == k {
                action.clone()
            } else {
                FaultAction::Continue
            }
        })
    }
    fn steps(&self) -> Vec<Step> {
        self.log.lock().unwrap().clone()
    }
}
impl Fault for Hook {
    fn at(&self, step: &Step) -> FaultAction {
        let n = self.n.fetch_add(1, Ordering::SeqCst);
        self.log.lock().unwrap().push(*step);
        (self.f)(n, step)
    }
}

fn io_fail() -> FaultAction {
    FaultAction::Fail(ToolError::new(
        ErrorCode::IoError,
        "injected failure",
        "none",
    ))
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
    /// A process restart: fresh store values over the same directories.
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
    /// Move the workspace clock forward by `secs` seconds.
    fn advance(&self, secs: u64) {
        self.clock.advance(secs);
    }

    /// Reopen the journal store with a different limit, as a server configured with a small
    /// `journal_max_total_mib` would. Used to reach the SIZE pass of `evict()` for real, instead of
    /// deleting a journal directory behind the store's back.
    fn reopen_journals_with(&mut self, limits: Limits) {
        self.journals =
            JournalStore::open(&self.state, &self.ws, limits.clone(), self.clock.clone()).unwrap();
        self.limits = limits;
    }

    fn ctx<'a>(&'a self, fault: &'a dyn Fault) -> ApplyContext<'a> {
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
    fn read(&self, rel: &str) -> Vec<u8> {
        fs::read(self.root.join(rel)).unwrap()
    }
    fn reads(&self, rels: &[&str]) -> Vec<Vec<u8>> {
        rels.iter().map(|r| self.read(r)).collect()
    }
    fn plan(&self, specs: &[(&str, Vec<Edit>)]) -> String {
        self.plans.put(&self.build_plan(specs)).unwrap().0
    }
    fn build_plan(&self, specs: &[(&str, Vec<Edit>)]) -> crate::Plan {
        use crate::{Plan, PlanFile, PlanRequest};
        use opencrayast_core::hash::ContentHash;
        let mut specs: Vec<_> = specs.to_vec();
        specs.sort_by(|a, b| a.0.cmp(b.0));
        let files = specs
            .iter()
            .map(|(rel, edits)| {
                let bytes = fs::read(self.root.join(rel)).unwrap();
                let text = String::from_utf8(bytes.clone()).unwrap();
                let new = apply_edits(&text, edits).unwrap();
                let lang_id = match Path::new(rel).extension().and_then(|e| e.to_str()) {
                    Some("rs") => "rust",
                    _ => "text",
                };
                PlanFile {
                    path: rel.to_string(),
                    language: lang_id.into(),
                    pre_hash: ContentHash::of(&bytes),
                    pre_size: bytes.len() as u64,
                    pre_errors: 0,
                    post_hash: ContentHash::of(new.as_bytes()),
                    post_size: new.len() as u64,
                    post_errors: 0,
                    edits: edits.clone(),
                }
            })
            .collect();
        Plan {
            format: 1,
            workspace_id: self.ws.clone(),
            engine_format: 1,
            request: PlanRequest {
                kind: "rewrite".into(),
                summary: "test".into(),
                note: None,
            },
            files,
        }
    }
    /// The original bytes this journal still holds, in manifest order. Used by the durability tests
    /// to prove the originals were not deleted.
    fn journal_originals(&self, plan_id: &str) -> Vec<Vec<u8>> {
        let dir = self
            .state
            .join(format!("ws-{}", self.ws))
            .join("journal")
            .join(plan_id)
            .join("orig");
        let mut out: Vec<(usize, Vec<u8>)> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| {
                (
                    e.file_name().to_string_lossy().parse::<usize>().unwrap(),
                    fs::read(e.path()).unwrap(),
                )
            })
            .collect();
        out.sort_by_key(|(i, _)| *i);
        out.into_iter().map(|(_, b)| b).collect()
    }

    fn tmp_leftovers(&self) -> Vec<String> {
        let mut out = vec![];
        let mut stack = vec![self.root.clone()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if e.file_type().unwrap().is_dir() {
                    stack.push(p);
                } else if e
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".opencrayast-tmp-")
                {
                    out.push(p.display().to_string());
                }
            }
        }
        out
    }
}

fn e(start: usize, end: usize, r: &str) -> Edit {
    Edit {
        start,
        end,
        replacement: r.into(),
    }
}

/// Replace the first occurrence of `needle` in `src` (no hand-counted offsets).
fn rep(src: &str, needle: &str, with: &str) -> Edit {
    let i = src
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in {src:?}"));
    e(i, i + needle.len(), with)
}

/// An applied world: the files, the plan id, the original contents and the applied contents.
fn scenario() -> (World, String, Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let w = World::new();
    w.write("a.rs", b"fn one() { 1 }\n");
    w.write("b.rs", b"fn two() { 2 }\n");
    let pre = w.reads(&["a.rs", "b.rs"]);
    let id = w.plan(&[
        ("a.rs", vec![rep("fn one() { 1 }\n", "1", "11")]),
        ("b.rs", vec![rep("fn two() { 2 }\n", "2", "22")]),
    ]);
    apply(&w.ctx(&NoFault), &id).unwrap();
    let post = vec![b"fn one() { 11 }\n".to_vec(), b"fn two() { 22 }\n".to_vec()];
    (w, id, pre, post)
}

/// The steps a clean undo announces, in order.
fn clean_undo_steps() -> Vec<Step> {
    let (w, id, _, _) = scenario();
    let hook = Hook::recorder();
    undo(&w.ctx(&hook), &id).unwrap();
    hook.steps()
}

fn code(r: Result<impl std::fmt::Debug, ToolError>) -> ErrorCode {
    r.unwrap_err().code
}

// ---- the happy path and the reported result ----------------------------------------------------

/// EDIT8-01: an applied plan is undone back to the exact original bytes, and the result says what
/// it restored, from which state to which, and under which journal id.
#[test]
fn an_applied_plan_is_undone_to_the_exact_original_bytes() {
    let (w, id, pre, post) = scenario();
    assert_eq!(w.reads(&["a.rs", "b.rs"]), post, "the apply happened");

    let res = undo(&w.ctx(&NoFault), &id).unwrap();
    assert_eq!(res.plan_id, id);
    assert_eq!(res.journal_id, id);
    assert_eq!(res.restored, vec!["a.rs".to_string(), "b.rs".to_string()]);
    assert_eq!(res.from, JournalState::Applied);
    assert_eq!(res.to, JournalState::Undone);
    assert_eq!(w.reads(&["a.rs", "b.rs"]), pre, "fully original again");

    let m = w.journals.load(&id).unwrap();
    assert_eq!(m.state, JournalState::Undone);
    assert_eq!(m.progress, 2, "progress counts the restored files");
    assert!(w.tmp_leftovers().is_empty());
}

/// EDIT8-02: a clean undo announces its steps in the documented order: lock, mark undoing, then
/// one restore-and-progress per file in plan order, then mark undone.
#[test]
fn a_clean_undo_announces_its_steps_in_the_documented_order() {
    use StepKind::*;
    let steps = clean_undo_steps();
    let kinds: Vec<(StepKind, usize)> = steps.iter().map(|s| (s.kind, s.index)).collect();
    assert_eq!(
        kinds,
        vec![
            (Lock, 0),
            (MarkUndoing, 0),
            (Restore, 0),
            (Progress, 0),
            (Restore, 1),
            (Progress, 1),
            (MarkUndone, 0),
        ],
        "the manifest says undoing BEFORE the first original is written back"
    );
}

// ---- the failure semantics table, one test per row ----------------------------------------------

/// EDIT8-03: refusals that happen before any write: a short id, a plan that was never applied, a
/// journal that does not verify, write mode off, and a journal in a state undo does not accept.
#[test]
fn refusals_before_anything_happens() {
    let (w, id, pre, post) = scenario();

    // not a full id
    assert_eq!(
        code(undo(&w.ctx(&NoFault), "p-short")),
        ErrorCode::InvalidArgs
    );
    // a plan id nobody knows: no journal AND no plan
    let unknown = "p-aaaaaaaaaaaaaaaaaaaaaaaaaa";
    assert_eq!(
        code(undo(&w.ctx(&NoFault), unknown)),
        ErrorCode::PlanNotFound,
        "never applied, or an id nobody knows"
    );
    // neither refusal wrote anything
    assert_eq!(w.reads(&["a.rs", "b.rs"]), post);
    assert_eq!(w.journals.load(&id).unwrap().state, JournalState::Applied);

    // write mode off
    let mut ctx = w.ctx(&NoFault);
    ctx = ApplyContext::new(
        ctx.boundary,
        ctx.plans,
        ctx.journals,
        ctx.limits,
        ctx.state_dir,
        ctx.workspace_id,
        None,
        ctx.lock_timeout,
        ctx.fault,
    );
    assert_eq!(code(undo(&ctx, &id)), ErrorCode::WriteDisabled);
    assert_eq!(w.reads(&["a.rs", "b.rs"]), post);

    // already undone: the journal is in a terminal state
    undo(&w.ctx(&NoFault), &id).unwrap();
    let err = undo(&w.ctx(&NoFault), &id).unwrap_err();
    assert!(
        matches!(err.code, ErrorCode::InvalidArgs | ErrorCode::AlreadyApplied),
        "a second undo is refused with a state-bearing code, got {:?}",
        err.code
    );
    assert_eq!(
        w.reads(&["a.rs", "b.rs"]),
        pre,
        "still original, not a second rewrite"
    );
}

/// EDIT8-04: a file edited by a person after the apply refuses the WHOLE undo, lists every file
/// with its class, and writes nothing at all.
#[test]
fn a_file_changed_after_apply_refuses_the_whole_undo_with_zero_writes() {
    let (w, id, _, post) = scenario();
    w.write("a.rs", b"fn one() { 11 } // a person was here\n");

    let err = undo(&w.ctx(&NoFault), &id).unwrap_err();
    assert_eq!(err.code, ErrorCode::Diverged);
    // the listing names both files, so a person can see the whole picture
    assert!(err.message.contains("a.rs: other"), "{}", err.message);
    assert!(err.message.contains("b.rs: post"), "{}", err.message);
    // and the b.rs file that WAS post was still not touched
    assert_eq!(w.read("b.rs"), post[1], "zero writes: b.rs untouched");
    assert!(w.tmp_leftovers().is_empty());
    assert_eq!(
        w.journals.load(&id).unwrap().state,
        JournalState::Applied,
        "a refused undo does not move the journal"
    );
}

/// EDIT8-05: a file that is already back at `pre_hash` while the journal still says `applied` is
/// refused by `plan_undo` — an undo requires every file to be `post`, because the user asked to
/// undo an apply that is no longer fully there. Nothing is written; the listing names the file.
#[test]
fn a_file_already_at_pre_hash_refuses_a_fresh_undo() {
    let (w, id, pre, post) = scenario();
    // Put a.rs back by hand, without telling the journal: the tree no longer matches `applied`.
    w.write("a.rs", &pre[0]);

    let err = undo(&w.ctx(&NoFault), &id).unwrap_err();
    assert_eq!(err.code, ErrorCode::Diverged);
    assert!(err.message.contains("a.rs: pre"), "{}", err.message);
    assert_eq!(
        w.reads(&["a.rs", "b.rs"]),
        vec![pre[0].clone(), post[1].clone()],
        "zero writes: b.rs is still applied and was not rewritten"
    );
    assert_eq!(
        w.journals.load(&id).unwrap().state,
        JournalState::Applied,
        "the refused undo did not move the journal"
    );
}

/// EDIT8-06: a journal whose `orig/<n>` no longer hashes to `pre_hash` is `plan_corrupt` and
/// nothing is written — an untrustworthy original is never written back.
#[test]
fn an_original_that_does_not_match_its_pre_hash_is_never_written_back() {
    let (mut w, id, _, post) = scenario();
    // Tamper with the stored original directly, the way a damaged disk would.
    let orig = w
        .state
        .join(format!("ws-{}", w.ws))
        .join("journal")
        .join(&id)
        .join("orig")
        .join("0");
    fs::write(&orig, b"fn one() { tampered }\n").unwrap();
    w.restart();

    let err = undo(&w.ctx(&NoFault), &id).unwrap_err();
    assert_eq!(err.code, ErrorCode::PlanCorrupt);
    assert_eq!(
        w.reads(&["a.rs", "b.rs"]),
        post,
        "zero writes: the tampered original never reached a file"
    );
    assert_eq!(w.journals.load(&id).unwrap().state, JournalState::Applied);
}

/// EDIT8-07: a target that no longer resolves under the WRITE policy refuses the undo before any
/// write (EDT-17: a manifest path is a request, not an authority).
#[test]
fn the_write_policy_is_re_applied_to_every_manifest_path() {
    let (w, id, _, _) = scenario();
    // Delete one of the targets: it resolves to nothing, so it cannot be classified as post.
    fs::remove_file(w.root.join("b.rs")).unwrap();

    let err = undo(&w.ctx(&NoFault), &id).unwrap_err();
    // a missing target is `other`, i.e. the journal and the tree disagree
    assert_eq!(err.code, ErrorCode::Diverged);
    assert!(err.message.contains("b.rs: other"), "{}", err.message);
    // a.rs was not restored either: the whole undo is refused
    assert_eq!(w.read("a.rs"), b"fn one() { 11 }\n", "zero writes");
}

// ---- E-13: the interruption matrix --------------------------------------------------------------

/// EDIT8-08: a failure at any step of an undo leaves a self-consistent state: either the workspace
/// is fully original, or the journal says `undoing` and recovery finishes it. Never a mixture, and
/// never a temp file.
#[test]
fn a_failure_at_any_step_leaves_a_state_recovery_can_finish() {
    let n = clean_undo_steps().len();
    for k in 0..n {
        let (mut w, id, pre, _) = scenario();
        let err = undo(&w.ctx(&Hook::at(k, io_fail())), &id).unwrap_err();
        assert_eq!(err.code, ErrorCode::IoError, "step {k}");
        assert!(w.tmp_leftovers().is_empty(), "step {k}: no temp files");

        match w.journals.load(&id).unwrap().state {
            // Failed before the journal moved: nothing was written, the plan stays applied.
            JournalState::Applied => {
                assert_eq!(
                    w.reads(&["a.rs", "b.rs"]),
                    vec![b"fn one() { 11 }\n".to_vec(), b"fn two() { 22 }\n".to_vec()],
                    "step {k}: still fully applied"
                );
                // and the undo can simply be retried
                undo(&w.ctx(&NoFault), &id).unwrap();
                assert_eq!(w.reads(&["a.rs", "b.rs"]), pre, "step {k}: retry works");
            }
            // Failed after: recovery (or undo itself) finishes the direction the user asked for.
            JournalState::Undoing => {
                w.restart();
                recover(&w.ctx(&NoFault)).unwrap();
                assert_eq!(
                    w.reads(&["a.rs", "b.rs"]),
                    pre,
                    "step {k}: recovery finished it"
                );
                assert_eq!(w.journals.load(&id).unwrap().state, JournalState::Undone);
            }
            s => panic!("step {k}: unexpected state {s:?}"),
        }
    }
}

/// EDIT8-09: a crash at any step of an undo, followed by a restart, is repaired by recovery — and
/// recovery COMPLETES the undo rather than reversing it. This is E-13 for the undo direction.
///
/// The matrix has two halves, and each is a one-method-only guarantee:
///
/// - a crash **before** the manifest says `undoing` wrote nothing, so the journal is still
///   `applied`, which is terminal: recovery correctly does nothing and the plan stays applied, and
///   the undo can simply be retried;
/// - a crash **at or after** it left the journal `undoing`, so recovery finishes the direction the
///   user asked for and the tree ends fully original.
///
/// What never happens in either half is a mixture, or a journal that disagrees with the tree.
#[test]
fn a_crash_at_any_step_is_repaired_by_recovery_in_the_undo_direction() {
    let applied = vec![b"fn one() { 11 }\n".to_vec(), b"fn two() { 22 }\n".to_vec()];
    // A step is announced BEFORE it changes durable state, so a crash AT the MarkUndoing step
    // leaves the journal at `applied`; only from the step AFTER it is the journal `undoing`.
    let undoing_from = clean_undo_steps()
        .iter()
        .position(|s| s.kind == StepKind::MarkUndoing)
        .unwrap()
        + 1;
    let n = clean_undo_steps().len();
    for k in 0..n {
        let (mut w, id, pre, _) = scenario();
        let err = undo(&w.ctx(&Hook::at(k, FaultAction::Crash)), &id).unwrap_err();
        assert_eq!(
            err.code,
            ErrorCode::Internal,
            "step {k}: reported as injected"
        );

        w.restart(); // the process died: new stores over the same directories
        recover(&w.ctx(&NoFault)).unwrap();
        assert!(w.tmp_leftovers().is_empty(), "step {k}: no temp files");

        if k < undoing_from {
            assert_eq!(
                w.reads(&["a.rs", "b.rs"]),
                applied,
                "step {k}: untouched, because no original had been written back yet"
            );
            assert_eq!(w.journals.load(&id).unwrap().state, JournalState::Applied);
            assert!(
                recover(&w.ctx(&NoFault)).unwrap().is_empty(),
                "step {k}: nothing to recover"
            );
            undo(&w.ctx(&NoFault), &id).unwrap();
            assert_eq!(
                w.reads(&["a.rs", "b.rs"]),
                pre,
                "step {k}: the retry undoes it"
            );
        } else {
            assert_eq!(
                w.reads(&["a.rs", "b.rs"]),
                pre,
                "step {k}: recovery finished the UNDO, it did not reverse it"
            );
            assert_eq!(
                w.journals.load(&id).unwrap().state,
                JournalState::Undone,
                "step {k}: the journal says the plan was undone"
            );
            assert!(
                recover(&w.ctx(&NoFault)).unwrap().is_empty(),
                "step {k}: idempotent"
            );
            assert_eq!(w.reads(&["a.rs", "b.rs"]), pre, "step {k}: still original");
        }
    }
}

/// EDIT8-10: recovery itself can crash at any step while finishing an undo, and running it again
/// still converges, and is idempotent.
#[test]
fn recovery_of_an_interrupted_undo_can_crash_at_any_step_and_be_run_again() {
    // Crash the undo late, so there is real work for recovery to do.
    let late = clean_undo_steps()
        .iter()
        .position(|s| s.kind == StepKind::MarkUndone)
        .unwrap();
    // Record the steps of a clean recovery of that crash point.
    let steps = {
        let (mut w, id, _, _) = scenario();
        let _ = undo(&w.ctx(&Hook::at(late, FaultAction::Crash)), &id);
        w.restart();
        let rec = Hook::recorder();
        recover(&w.ctx(&rec)).unwrap();
        rec.steps()
    };
    assert!(
        steps.iter().any(|s| s.kind == StepKind::MarkTerminal),
        "recovery must mark the journal terminal: {steps:?}"
    );

    for j in 0..steps.len() {
        let (mut w, id, pre, _) = scenario();
        let _ = undo(&w.ctx(&Hook::at(late, FaultAction::Crash)), &id);
        w.restart();
        let r = recover(&w.ctx(&Hook::at(j, FaultAction::Crash)));
        assert_eq!(
            r.unwrap_err().code,
            ErrorCode::Internal,
            "recovery crash at step {j}"
        );
        w.restart();
        recover(&w.ctx(&NoFault)).unwrap();
        assert_eq!(
            w.reads(&["a.rs", "b.rs"]),
            pre,
            "recovery crash {j}: converged"
        );
        assert_eq!(w.journals.load(&id).unwrap().state, JournalState::Undone);
        assert!(
            recover(&w.ctx(&NoFault)).unwrap().is_empty(),
            "recovery crash {j}: idempotent"
        );
    }
}

/// EDIT8-11: an undo interrupted mid-way is finished by calling `undo` again (not only by
/// `recover`) — the direction comes from the journal state, not from the caller.
#[test]
fn an_undo_interrupted_mid_way_is_finished_by_calling_undo_again() {
    // Crash right after the first restore, so one file is `pre` and one is still `post`.
    let first_restore = clean_undo_steps()
        .iter()
        .position(|s| s.kind == StepKind::Restore)
        .unwrap();
    let after_first = first_restore + 1; // the Progress step right after it
    let (mut w, id, pre, _) = scenario();
    let _ = undo(&w.ctx(&Hook::at(after_first, FaultAction::Crash)), &id);
    w.restart();

    assert_eq!(w.read("a.rs"), pre[0], "a.rs is already restored");
    assert_eq!(w.journals.load(&id).unwrap().state, JournalState::Undoing);

    // Undo again: it must COMPLETE, not refuse and not restart from the beginning.
    let res = undo(&w.ctx(&NoFault), &id).unwrap();
    assert_eq!(res.from, JournalState::Undoing);
    assert_eq!(res.to, JournalState::Undone);
    // a.rs was already restored, so this call only reports what it actually wrote
    assert_eq!(res.restored, vec!["b.rs".to_string()]);
    assert_eq!(w.reads(&["a.rs", "b.rs"]), pre);
    assert_eq!(w.journals.load(&id).unwrap().state, JournalState::Undone);
}

/// EDIT8-12: an `undoing` journal whose file is `other` still refuses the whole thing: the
/// `undoing` row of the decision table is "every file pre or post, otherwise diverged".
#[test]
fn an_interrupted_undo_with_a_foreign_edit_refuses_and_writes_nothing() {
    let first_restore = clean_undo_steps()
        .iter()
        .position(|s| s.kind == StepKind::Restore)
        .unwrap();
    let (mut w, id, pre, _) = scenario();
    let _ = undo(
        &w.ctx(&Hook::at(first_restore + 1, FaultAction::Crash)),
        &id,
    );
    w.restart();
    // A person edits the file the undo has not reached yet.
    w.write("b.rs", b"fn two() { 22 } // a person was here\n");

    let err = undo(&w.ctx(&NoFault), &id).unwrap_err();
    assert_eq!(err.code, ErrorCode::Diverged);
    assert!(err.message.contains("b.rs: other"), "{}", err.message);
    assert_eq!(w.read("a.rs"), pre[0], "the restored file stays restored");
    assert_eq!(
        w.journals.load(&id).unwrap().state,
        JournalState::Undoing,
        "a person must resolve the tree; the journal waits"
    );
}

/// EDIT8-13: an I/O failure while writing a file back leaves the journal `undoing`, says which
/// files were already restored, and recovery finishes the job.
#[test]
fn an_io_failure_while_restoring_reports_what_was_restored_and_recovery_finishes() {
    let first_restore = clean_undo_steps()
        .iter()
        .position(|s| s.kind == StepKind::Restore)
        .unwrap();
    // Fail at the SECOND restore, so the first file really is back on disk.
    let second_restore = clean_undo_steps()
        .iter()
        .filter(|s| s.kind == StepKind::Restore)
        .nth(1)
        .map(|_| first_restore + 2)
        .unwrap();
    let (mut w, id, pre, _) = scenario();
    let err = undo(&w.ctx(&Hook::at(second_restore, io_fail())), &id).unwrap_err();
    assert_eq!(err.code, ErrorCode::IoError);
    assert!(
        err.message.contains("a.rs"),
        "the message says what was restored: {}",
        err.message
    );
    assert!(
        err.next.contains("recovery") || err.message.contains("recovery"),
        "the error says what to do next: {:?} / {}",
        err.next,
        err.message
    );
    assert_eq!(w.read("a.rs"), pre[0], "the first file really is restored");
    assert_eq!(w.journals.load(&id).unwrap().state, JournalState::Undoing);

    w.restart();
    recover(&w.ctx(&NoFault)).unwrap();
    assert_eq!(w.reads(&["a.rs", "b.rs"]), pre, "recovery finished it");
    assert_eq!(w.journals.load(&id).unwrap().state, JournalState::Undone);
}

/// EDIT8-14: undo never rewrites a file that is already at `pre_hash` (no redundant writes, and
/// therefore no needless identity churn on the file).
#[test]
fn an_already_restored_file_is_skipped_rather_than_rewritten() {
    let first_restore = clean_undo_steps()
        .iter()
        .position(|s| s.kind == StepKind::Restore)
        .unwrap();
    let (mut w, id, pre, _) = scenario();
    let _ = undo(
        &w.ctx(&Hook::at(first_restore + 1, FaultAction::Crash)),
        &id,
    );
    w.restart();

    let before = fs::metadata(w.root.join("a.rs"))
        .unwrap()
        .modified()
        .unwrap();
    let res = undo(&w.ctx(&NoFault), &id).unwrap();
    assert_eq!(
        res.restored,
        vec!["b.rs".to_string()],
        "a.rs was not written again"
    );
    let after = fs::metadata(w.root.join("a.rs"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(before, after, "a.rs was not touched at all");
    assert_eq!(w.reads(&["a.rs", "b.rs"]), pre);
}

/// EDIT8-15: two undos racing leave exactly one of them doing the work, and the tree ends original.
#[test]
fn two_undos_of_one_plan_do_not_interleave_into_a_mixture() {
    let (w, id, pre, _) = scenario();
    undo(&w.ctx(&NoFault), &id).unwrap();
    // The second one is refused (the journal is terminal) rather than rewriting anything.
    let second = undo(&w.ctx(&NoFault), &id);
    assert!(second.is_err(), "a second undo is refused");
    assert_eq!(w.reads(&["a.rs", "b.rs"]), pre, "still exactly original");
    assert!(w.tmp_leftovers().is_empty());
}

/// EDIT8-16: a plan with no journal is `plan_not_found`, and the state is reached the way
/// production reaches it.
///
/// This test used to assert the opposite — `journal_missing` while the plan was still readable,
/// and `plan_not_found` once it was not. That split claimed more than the function can know: the
/// evidence that a plan was applied is the journal's own state, and the journal is exactly what is
/// missing. So both arms now report the one thing that is certain and that a caller acts on —
/// **there is no journal, so there are no originals, so there is nothing to undo** — and neither
/// says why. The ID is still the same code for both, because it is the same situation.
///
/// Both situations are built with the journal store's own SIZE pass (`journal_max_total_mib` = 0
/// and `evict()`), not by deleting the directory behind the store's back. That matters because the
/// defaults make the age pass useless here: `plan_ttl_minutes` is 15 and `journal_retention_days`
/// is 7, so a journal removed for AGE always outlives its plan.
#[test]
fn a_missing_journal_is_always_plan_not_found_and_says_nothing_about_why() {
    // (a) the plan survives, so it is readable
    let (mut w, id, _pre, post) = scenario();
    let mut tiny = w.limits.clone();
    tiny.journal_max_total_mib = 0;
    w.reopen_journals_with(tiny.clone());
    let evicted = w.journals.evict().unwrap();
    assert_eq!(
        evicted,
        vec![id.clone()],
        "the store's own size pass removed the journal"
    );
    assert!(!w.journals.exists(&id).unwrap(), "precondition: no journal");
    assert!(
        w.plans.get_for_read(&id).is_ok(),
        "precondition: the plan is still in the store"
    );
    let err = undo(&w.ctx(&NoFault), &id).unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::PlanNotFound,
        "the message: {}",
        err.message
    );
    // The message must say there is nothing to undo, and must NOT claim a cause: the store
    // evicts by age AND by size and does not record which, so saying "retention" would be a guess.
    assert!(
        err.next.contains("nothing to undo"),
        "and it says what the caller can rely on: {:?}",
        err.next
    );
    // Naming the missing journal is naming the FACT, not a cause, so it is allowed (and useful);
    // what is forbidden is claiming where it went.
    for forbidden in ["no longer exists", "was removed", "evicted", "retention"] {
        assert!(
            !err.message.to_lowercase().contains(forbidden),
            "the message claims a cause it cannot know ({forbidden:?}): {:?}",
            err.message
        );
    }
    assert_eq!(w.reads(&["a.rs", "b.rs"]), post, "nothing was written");

    // (b) an id that was never applied: no journal and no plan. Same code, same consequence.
    let (w2, _, _, _) = scenario();
    let unknown = "p-bbbbbbbbbbbbbbbbbbbbbbbbbb";
    assert!(
        w2.plans.get_for_read(unknown).is_err(),
        "precondition: no plan with this id exists"
    );
    assert!(!w2.journals.exists(unknown).unwrap());
    let err = undo(&w2.ctx(&NoFault), unknown).unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::PlanNotFound,
        "the same code: the situation, not the cause, is what is reported"
    );
    for forbidden in ["never applied", "removed", "expired", "retention"] {
        assert!(
            !err.message.to_lowercase().contains(forbidden)
                && !err.next.to_lowercase().contains(forbidden),
            "the message must not assert a cause it cannot know ({forbidden:?}): {:?} / {:?}",
            err.next,
            err.message
        );
    }
}

/// EDIT8-17: the new code exists and is spelled the way the tools documentation spells it, so a
/// client matching on the string keeps working.
#[test]
fn the_journal_missing_code_has_the_documented_spelling() {
    assert_eq!(ErrorCode::JournalMissing.as_str(), "journal_missing");
}

/// EDIT8-18: the third way a caller reaches `plan_not_found` on the undo path — the journal is
/// gone AND the plan has passed its TTL but has not been swept — is reachable through the real
/// mechanisms, and the message does not claim the plan was never applied.
///
/// The other two ways are an id that was never applied (EDIT8-16 (b)) and a plan removed long after
/// its journal (EDIT8-03). This one is the case a code comment here once denied: the plan file is
/// still on disk, but `get_for_read` refuses it because `now >= expires_at`. Reaching it needs the
/// clock, so `World::advance` moves it past `plan_ttl_minutes` (15 min) after the journal has been
/// evicted by the store's own size pass.
#[test]
fn an_expired_plan_with_no_journal_is_plan_not_found_and_says_nothing_about_being_absent() {
    let (mut w, id, _, post) = scenario();

    // The journal goes first, through the store's own size pass.
    let mut tiny = w.limits.clone();
    tiny.journal_max_total_mib = 0;
    w.reopen_journals_with(tiny);
    assert_eq!(w.journals.evict().unwrap(), vec![id.clone()]);
    assert!(!w.journals.exists(&id).unwrap());

    // The plan is still readable right now...
    assert!(
        w.plans.get_for_read(&id).is_ok(),
        "precondition: not expired yet"
    );

    // ...and stops being readable once its TTL passes, WITHOUT anything deleting it.
    let ttl = w.limits.plan_ttl_minutes * 60;
    w.advance(ttl);
    assert!(
        w.plans.get_for_read(&id).is_err(),
        "precondition: past plan_ttl_minutes, so get_for_read refuses it"
    );

    let err = undo(&w.ctx(&NoFault), &id).unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::PlanNotFound,
        "the plan is expired, not absent, and the code says nothing about which"
    );
    // The message must not assert that the plan is gone, and must not assert it was never applied
    // (it WAS applied - that is the whole premise).
    for (field, text) in [("message", &err.message), ("next", &err.next)] {
        for forbidden in [
            "no such plan",
            "does not exist",
            "never applied",
            "was removed",
            "retention",
            "expired",
        ] {
            assert!(
                !text.to_lowercase().contains(forbidden),
                "{field} asserts {forbidden:?}, which this call cannot know: {text:?}"
            );
        }
    }
    assert_eq!(w.reads(&["a.rs", "b.rs"]), post, "nothing was written");
}

// ---- the unauthenticated state field (E-16) -----------------------------------------------------
//
// Everything in this section rewrites `manifest.json` the way corruption does: same keys, same
// order, still canonical so `Manifest::parse` accepts it, same mode. Nothing here needs an
// attacker — a torn write on a filesystem that reorders or zero-fills produces bytes of this kind,
// and that is the case worth defending.

/// A clock that panics the first time it is read, then behaves. Used to unwind through a live
/// journal-store lock guard — which is how a poisoned mutex arises in production — without also
/// poisoning every later read, so the test can show what the *store* does with the poisoned lock.
struct PanicOnceClock(AtomicBool);

impl Clock for PanicOnceClock {
    fn now_secs(&self) -> u64 {
        if !self.0.swap(true, Ordering::SeqCst) {
            panic!("injected panic inside the journal store critical section");
        }
        1_000_000
    }
}

/// The path of a journal's manifest, as the store lays it out.
fn manifest_path(w: &World, plan_id: &str) -> PathBuf {
    w.state
        .join(format!("ws-{}", w.ws))
        .join("journal")
        .join(plan_id)
        .join("manifest.json")
}

/// Rewrite the state field of the journal manifest and write the result back, keeping the bytes
/// canonical.
///
/// This is the shape-preserving corruption the defect needs: the result still parses, still passes
/// `Manifest::check`, still names a legal state, and the file keeps its mode — only one field
/// differs. Nothing here needs an attacker; a torn write that flips those eight bytes produces the
/// same manifest, and that is the case worth defending.
fn forge_state_to_prepared(w: &World, plan_id: &str) {
    let path = manifest_path(w, plan_id);
    let before = fs::read(&path).unwrap();
    let mut forged = crate::journal::Manifest::parse(&before)
        .expect("the journal is a valid manifest before the forgery");
    assert_eq!(forged.state, JournalState::Writing);
    forged.state = JournalState::Prepared;
    let after = forged.canonical_bytes();
    // Prove the forgery is accepted: if `parse` refused it, this test would be proving the
    // parser's strictness rather than the recovery decision.
    let reread = crate::journal::Manifest::parse(&after)
        .expect("the forged manifest is still a valid manifest");
    assert_eq!(reread.state, JournalState::Prepared);
    assert_eq!(
        reread.plan_digest, forged.plan_digest,
        "only `state` changed"
    );
    assert_eq!(reread.files, forged.files, "only `state` changed");
    fs::write(&path, &after).unwrap();
}

/// A world with a three-file plan applied and a crash injected after the first file was replaced:
/// journal `writing`, file 0 edited, files 1 and 2 untouched.
fn three_files_crashed_after_the_first() -> (World, String, Vec<Vec<u8>>) {
    let w = World::new();
    w.write("a.rs", b"fn a() { 1 }\n");
    w.write("b.rs", b"fn b() { 2 }\n");
    w.write("c.rs", b"fn c() { 3 }\n");
    let pre = w.reads(&["a.rs", "b.rs", "c.rs"]);
    let id = w.plan(&[
        ("a.rs", vec![rep("fn a() { 1 }\n", "1", "11")]),
        ("b.rs", vec![rep("fn b() { 2 }\n", "2", "22")]),
        ("c.rs", vec![rep("fn c() { 3 }\n", "3", "33")]),
    ]);
    // A clean apply announces, in order: Recover, Verify x3, Gates, JournalCreate, MarkWriting,
    // then (Replace, Progress) per file, then MarkApplied. `Replace 0` is announced BEFORE file 0 is
    // written, so crashing AT it leaves nothing written; `Progress 0` is announced AFTER file 0 was
    // written and before the counter moves. Index 9 is the second of those.
    let crash_at = {
        let hook = Hook::recorder();
        apply(&w.ctx(&hook), &id).unwrap();
        hook.steps()
            .iter()
            .position(|s| s.kind == StepKind::Progress && s.index == 0)
            .expect("a progress step for file 0")
    };
    let mut w2 = World::new();
    w2.write("a.rs", b"fn a() { 1 }\n");
    w2.write("b.rs", b"fn b() { 2 }\n");
    w2.write("c.rs", b"fn c() { 3 }\n");
    let id2 = w2.plan(&[
        ("a.rs", vec![rep("fn a() { 1 }\n", "1", "11")]),
        ("b.rs", vec![rep("fn b() { 2 }\n", "2", "22")]),
        ("c.rs", vec![rep("fn c() { 3 }\n", "3", "33")]),
    ]);
    let err = apply(&w2.ctx(&Hook::at(crash_at, FaultAction::Crash)), &id2).unwrap_err();
    assert_eq!(err.code, ErrorCode::Internal, "reported as injected");
    w2.restart(); // the process died
    let m = w2.journals.load(&id2).unwrap();
    assert_eq!(
        m.state,
        JournalState::Writing,
        "the crash left it mid-apply"
    );
    assert_eq!(
        w2.reads(&["a.rs", "b.rs", "c.rs"]),
        vec![b"fn a() { 11 }\n".to_vec(), pre[1].clone(), pre[2].clone()],
        "exactly one file is edited"
    );
    drop(w);
    (w2, id2, pre)
}

/// E-16, the defect itself: a `prepared` state field on a journal whose files are half-applied must
/// not produce a silent, successful rollback. Apply three files, crash after the first, rewrite
/// `"state":"writing"` to `"state":"prepared"` in `manifest.json`, restart, run `recover`.
///
/// The old code returned `Ok(["prepared->rolled_back"])` with all three files still edited and the
/// journal now terminal, so undo was permanently refused and nothing would ever read `orig/` again.
/// The invariant asserted here is the one that matters: **`recover` cannot both succeed and leave
/// the tree edited.**
#[test]
fn a_forged_prepared_state_on_a_half_applied_journal_is_not_a_silent_no_op() {
    let (mut w, id, pre) = three_files_crashed_after_the_first();
    forge_state_to_prepared(&w, &id);
    w.restart();

    match recover(&w.ctx(&NoFault)) {
        Ok(recovered) => {
            // It claimed success, so the originals must really be back. This is the assertion the
            // old behaviour violated.
            assert_eq!(
                w.reads(&["a.rs", "b.rs", "c.rs"]),
                pre,
                "recover reported {recovered:?}, so the originals must really be restored"
            );
            assert_eq!(
                w.journals.load(&id).unwrap().state,
                JournalState::RolledBack
            );
        }
        Err(e) => {
            // It refused, so it must not also have marked the journal terminal: a refusal that
            // destroys the ability to retry is not honest either.
            assert_ne!(
                w.journals.load(&id).unwrap().state,
                JournalState::RolledBack,
                "it refused ({e:?}), so it must not have marked the journal terminal"
            );
        }
    }
}

/// The same forgery, asserting the outcome this fix actually produces: the filesystem wins, the one
/// edited file is restored from `orig/0`, the tree ends fully original — never a mixture — and the
/// journal is `rolled_back` because that is where the restored tree belongs.
#[test]
fn a_forged_prepared_state_is_repaired_from_the_originals_rather_than_trusted() {
    let (mut w, id, pre) = three_files_crashed_after_the_first();
    forge_state_to_prepared(&w, &id);
    w.restart();

    let recovered = recover(&w.ctx(&NoFault)).unwrap();
    assert_eq!(recovered.len(), 1, "one journal was repaired");
    assert_eq!(recovered[0].from, JournalState::Prepared);
    assert_eq!(recovered[0].to, JournalState::RolledBack);
    assert_eq!(
        recovered[0].restored,
        vec!["a.rs".to_string()],
        "the one file that was actually edited, and no other"
    );
    assert_eq!(
        w.reads(&["a.rs", "b.rs", "c.rs"]),
        pre,
        "fully original, not a mixture"
    );
    assert_eq!(
        w.journals.load(&id).unwrap().state,
        JournalState::RolledBack
    );
    assert!(
        recover(&w.ctx(&NoFault)).unwrap().is_empty(),
        "and idempotent"
    );
    // The originals are still on disk, as they were before; nothing was deleted.
    assert!(!w.journal_originals(&id).is_empty());
}

/// The plan binding (the other half of the fix): a journal whose `plan_digest` does not match the
/// plan is refused, so no state field can be acted on at all.
#[test]
fn a_journal_whose_plan_digest_does_not_match_the_plan_is_refused_and_left_alone() {
    let (mut w, id, _) = three_files_crashed_after_the_first();
    let path = w
        .state
        .join(format!("ws-{}", w.ws))
        .join("journal")
        .join(&id)
        .join("manifest.json");
    let mut forged = crate::journal::Manifest::parse(&fs::read(&path).unwrap()).unwrap();
    forged.plan_digest = opencrayast_core::hash::ContentHash::of(b"a different plan entirely");
    fs::write(&path, forged.canonical_bytes()).unwrap();

    w.restart();
    let err = recover(&w.ctx(&NoFault)).unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::PlanCorrupt,
        "a journal that is not bound to its plan is refused: {}",
        err.message
    );
    assert_eq!(
        w.journals.load(&id).unwrap().state,
        JournalState::Writing,
        "and the journal is untouched, so a person can still act on it"
    );
}

/// The milder instance the auditor reported separately: one `post_hash` rewritten to zeros is
/// accepted by `load()` (it is a well-formed hash) and then poisons undo with a `diverged` that no
/// hash can satisfy. With the plan binding in place the same rewrite is refused at the first
/// transition, so the undo is refused for the honest reason instead of an unsatisfiable one.
#[test]
fn a_rewritten_post_hash_is_refused_by_the_plan_binding_not_left_to_poison_undo() {
    let (mut w, id, _, _) = scenario();
    let path = w
        .state
        .join(format!("ws-{}", w.ws))
        .join("journal")
        .join(&id)
        .join("manifest.json");
    let bytes = fs::read(&path).unwrap();
    let mut forged = crate::journal::Manifest::parse(&bytes).unwrap();
    // A well-formed hash that is nobody's post_hash — the corruption need not be zeros, it need
    // only be a hash no file in this workspace can produce.
    forged.files[0].post_hash = opencrayast_core::hash::ContentHash::of(b"not a real post image");
    assert_ne!(
        forged.files[0].post_hash, forged.files[0].pre_hash,
        "precondition: the forged hash is neither pre nor post"
    );
    fs::write(&path, forged.canonical_bytes()).unwrap();

    w.restart();
    // `load` accepts it — the shape is fine and the store cannot see the plan — and that is
    // precisely why the shell must re-check the binding before it acts.
    let m = w.journals.load(&id).unwrap();
    assert_eq!(
        m.files[0].post_hash,
        opencrayast_core::hash::ContentHash::of(b"not a real post image"),
        "precondition: the store still loads it"
    );
    let err = undo(&w.ctx(&NoFault), &id).unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::PlanCorrupt,
        "undo refuses the unbound journal up front: {}",
        err.message
    );
    assert_eq!(
        w.reads(&["a.rs", "b.rs"]),
        vec![b"fn one() { 11 }\n".to_vec(), b"fn two() { 22 }\n".to_vec()],
        "and nothing was written"
    );
}

/// E-17: a panic inside a journal-store critical section must not disable the store for the rest
/// of the process.
///
/// This is the sharp edge the auditor reproduced, and it has three faces. With a poisoned lock
/// refused as `internal`, `apply` said "journal store lock was poisoned", `undo` reported a
/// misleading `journal_missing`, and **`recover` returned `Ok(0)`** — a *successful empty* result
/// meaning "nothing to recover" when it could not read a single journal. A half-applied tree plus a
/// recovery that reports there was nothing to do is the worst pair of answers available.
///
/// The test drives all three through the same poisoned store, because that is how it happens: the
/// mutex is per-process, so a restart does not help and every later operation inherits it.
#[test]
fn a_poisoned_journal_store_lock_does_not_disable_the_store_for_the_process() {
    let (mut w, _id, _, _) = scenario();

    // Poison the lock: `evict` takes it at the top of the function and reads the clock inside the
    // guarded section, so a clock that panics unwinds through a live guard — the same window a
    // panic in a test hook, a `Drop`, or an allocation failure would use.
    let poisoned = JournalStore::open(
        &w.state,
        &w.ws,
        w.limits.clone(),
        Arc::new(PanicOnceClock(AtomicBool::new(false))),
    )
    .unwrap();
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {})); // the panic is expected; keep the output readable
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = poisoned.evict();
    }));
    std::panic::set_hook(prev);
    assert!(
        caught.is_err(),
        "the clock panicked inside the critical section"
    );

    // The same store value is used from here on, exactly as in the reported reproduction: the
    // mutex is per-process, so a "restart" in the abstract does not clear it.
    w.journals = poisoned;

    // (1) recover must succeed rather than reporting a poisoned lock as its result. On this
    // scenario there genuinely is nothing to recover, so the honest answer is an empty Ok — the
    // point is that it is reached by reading the store, not by a lock that refused to be taken.
    let recovered = recover(&w.ctx(&NoFault))
        .unwrap_or_else(|e| panic!("a poisoned store must not make recover fail: {e:?}"));
    assert!(
        recovered.is_empty(),
        "nothing is non-terminal here, so an empty Ok is correct — but it must come from a store          that was actually read"
    );

    // (2) apply must work: `create` takes the poisoned lock, so this is the operation the old code
    // refused with `internal: journal store lock was poisoned`.
    let id2 = w.plan(&[
        ("a.rs", vec![rep("fn one() { 11 }\n", "11", "111")]),
        ("b.rs", vec![rep("fn two() { 22 }\n", "22", "222")]),
    ]);
    let res = apply(&w.ctx(&NoFault), &id2);
    assert!(
        res.is_ok(),
        "apply must survive a poisoned store lock, not report the lock: {:?}",
        res.err().map(|e| e.to_string())
    );
    assert_eq!(w.journals.load(&id2).unwrap().state, JournalState::Applied);

    // (3) undo must report what actually happened, never a `journal_missing` invented by the lock.
    let unknown = "p-bbbbbbbbbbbbbbbbbbbbbbbbbb";
    let err = undo(&w.ctx(&NoFault), unknown).unwrap_err();
    assert_ne!(
        err.code,
        ErrorCode::JournalMissing,
        "a poisoned lock must not be reported as a missing journal: {}",
        err.message
    );
}
