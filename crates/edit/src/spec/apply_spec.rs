//! Spec for EDIT-7: the apply shell and recovery on a REAL filesystem (E-3..E-9, E-14;
//! EDT-04, EDT-05, EDT-07..EDT-10, EDT-13, EDT-15, EDT-17). Never weaken; add cases. If an
//! expectation looks wrong, block the ticket with a minimal reproduction.
//!
//! The centrepiece is the fault matrix: a clean apply is recorded step by step, then replayed
//! with a failure injected at every step, and with a crash injected at every step (followed by
//! a "restart" and recovery, itself crashed at every step). Whatever happens, the workspace ends
//! fully original, never a mixture.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crate::{
    ApplyContext, Clock, Edit, Fault, FaultAction, JournalState, JournalStore, NoFault, Plan,
    PlanFile, PlanRequest, PlanStore, Step, StepKind, apply, apply_edits, recover,
};
use opencrayast_core::ErrorCode;
use opencrayast_core::boundary::{Boundary, BoundaryConfig};
use opencrayast_core::error::ToolError;
use opencrayast_core::hash::ContentHash;
use opencrayast_core::limits::Limits;
use opencrayast_core::workspace::workspace_id;
use opencrayast_lang::{Language, ParseBudget, parse};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct FakeClock(AtomicU64);
impl Clock for FakeClock {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
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
    /// Build a plan from the files as they are now, store it, return its id.
    fn plan(&self, specs: &[(&str, Vec<Edit>)]) -> String {
        let p = self.build_plan(specs);
        self.plans.put(&p).unwrap().0
    }
    fn build_plan(&self, specs: &[(&str, Vec<Edit>)]) -> Plan {
        let mut specs: Vec<_> = specs.to_vec();
        specs.sort_by(|a, b| a.0.cmp(b.0));
        let budget = ParseBudget {
            max_bytes: 1 << 24,
            timeout: Duration::from_secs(10),
            max_depth: 4096,
            max_nodes: 10_000_000,
        };
        let files = specs
            .iter()
            .map(|(rel, edits)| {
                let bytes = fs::read(self.root.join(rel)).unwrap();
                let text = String::from_utf8(bytes.clone()).unwrap();
                let new = apply_edits(&text, edits).unwrap();
                let lang_id = match Path::new(rel).extension().and_then(|e| e.to_str()) {
                    Some("rs") => "rust",
                    Some("ts") | Some("mts") | Some("cts") => "typescript",
                    Some("tsx") => "tsx",
                    Some("js") | Some("jsx") | Some("mjs") | Some("cjs") => "javascript",
                    Some("py") | Some("pyi") => "python",
                    Some("go") => "go",
                    _ => "text",
                };
                let errors = |t: &str| match Language::from_id(lang_id) {
                    Some(l) => parse(l, t, &budget).unwrap().error_count as u64,
                    None => 0,
                };
                PlanFile {
                    path: rel.to_string(),
                    language: lang_id.into(),
                    pre_hash: ContentHash::of(&bytes),
                    pre_size: bytes.len() as u64,
                    pre_errors: errors(&text),
                    post_hash: ContentHash::of(new.as_bytes()),
                    post_size: new.len() as u64,
                    post_errors: errors(&new),
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
                note: Some(format!("{}", self.plans.list().unwrap().0.len())),
            },
            files,
        }
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

/// The standard scenario: two Rust files and a JavaScript file.
fn scenario() -> (World, String, Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let w = World::new();
    w.write("a.rs", b"fn one() { 1 }\n");
    w.write("b.rs", b"fn two() { 2 }\n");
    let pre = w.reads(&["a.rs", "b.rs"]);
    let id = w.plan(&[
        ("a.rs", vec![rep("fn one() { 1 }\n", "1", "11")]),
        ("b.rs", vec![rep("fn two() { 2 }\n", "2", "22")]),
    ]);
    let post = vec![b"fn one() { 11 }\n".to_vec(), b"fn two() { 22 }\n".to_vec()];
    (w, id, pre, post)
}

fn code(r: Result<impl std::fmt::Debug, ToolError>) -> ErrorCode {
    r.unwrap_err().code
}

#[test]
fn a_plan_is_applied_exactly_as_recorded_and_journaled() {
    let (w, id, pre, post) = scenario();
    let res = apply(&w.ctx(&NoFault), &id).unwrap();
    assert_eq!(res.plan_id, id);
    assert_eq!(res.changed, vec!["a.rs".to_string(), "b.rs".to_string()]);
    assert!(!res.suggestion.is_empty());
    assert_eq!(w.reads(&["a.rs", "b.rs"]), post);
    let m = w.journals.load(&id).unwrap();
    assert_eq!((m.state, m.progress), (JournalState::Applied, 2));
    for (i, p) in pre.iter().enumerate() {
        assert_eq!(
            &w.journals.read_original(&id, i).unwrap(),
            p,
            "originals are in the journal"
        );
    }
    assert!(w.tmp_leftovers().is_empty());
    // E-9: never twice
    assert_eq!(
        code(apply(&w.ctx(&NoFault), &id)),
        ErrorCode::AlreadyApplied
    );
    assert_eq!(w.reads(&["a.rs", "b.rs"]), post);
}

#[test]
fn file_properties_are_preserved() {
    let w = World::new();
    let original = b"\xef\xbb\xbffn one() {\r\n    1\r\n}\r\n".to_vec();
    w.write("a.rs", &original);
    fs::set_permissions(w.root.join("a.rs"), fs::Permissions::from_mode(0o640)).unwrap();
    let text = String::from_utf8(original.clone()).unwrap();
    let edits = vec![rep(&text, "1", "11")];
    let want = apply_edits(&text, &edits).unwrap().into_bytes();
    let id = w.plan(&[("a.rs", edits)]);
    apply(&w.ctx(&NoFault), &id).unwrap();
    assert_eq!(
        w.read("a.rs"),
        want,
        "BOM and CRLF untouched, only the range replaced"
    );
    assert_eq!(
        fs::metadata(w.root.join("a.rs"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o640
    );
}

#[test]
fn refusals_before_anything_happens() {
    let (mut w, id, pre, _) = scenario();
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
    assert_eq!(code(apply(&ctx, &id)), ErrorCode::WriteDisabled);
    assert_eq!(code(recover(&ctx)), ErrorCode::WriteDisabled);
    for short in ["", "p-", &id[..12]] {
        assert_eq!(
            code(apply(&w.ctx(&NoFault), short)),
            ErrorCode::InvalidArgs,
            "{short}"
        );
    }
    assert_eq!(
        code(apply(&w.ctx(&NoFault), "p-aaaaaaaaaaaaaaaaaaaaaaaaaa")),
        ErrorCode::PlanNotFound
    );
    w.clock.0.fetch_add(15 * 60, Ordering::SeqCst);
    assert_eq!(code(apply(&w.ctx(&NoFault), &id)), ErrorCode::PlanExpired);
    w.restart();
    assert_eq!(w.reads(&["a.rs", "b.rs"]), pre);
    assert!(
        w.journals.list().unwrap().0.is_empty(),
        "no journal was created"
    );
}

#[test]
fn a_file_changed_after_preview_is_stale_and_nothing_is_written() {
    let (w, id, pre, _) = scenario();
    w.write("b.rs", b"fn two() { 2 } // edited by a person\n");
    let e1 = apply(&w.ctx(&NoFault), &id).unwrap_err();
    assert_eq!(e1.code, ErrorCode::StalePlan);
    assert!(
        e1.message.contains("b.rs") && !e1.message.contains("a.rs"),
        "{}",
        e1.message
    );
    assert!(
        !e1.message.contains("edited by a person"),
        "never quotes content"
    );
    assert_eq!(w.read("a.rs"), pre[0], "the untouched file was not touched");
    assert!(w.journals.list().unwrap().0.is_empty());
    // both stale: both are listed
    w.write("a.rs", b"fn one() { 1 } // too\n");
    let e2 = apply(&w.ctx(&NoFault), &id).unwrap_err();
    assert!(
        e2.message.contains("a.rs") && e2.message.contains("b.rs"),
        "{}",
        e2.message
    );
    // rewriting the same bytes (a new mtime) is not a change: the hash decides
    w.write("a.rs", &pre[0]);
    w.write("b.rs", &pre[1]);
    apply(&w.ctx(&NoFault), &id).unwrap();
}

/// A **same-length** edit by a person is stale, not corrupt.
///
/// The sibling of `a_file_changed_after_preview_is_stale_and_holding_nothing_is_written` above,
/// and the case that test could not reach. That test's scenario edits `1`→`11` and `2`→`22`, and
/// then the test appends a comment — every one of those edits changes the file's **length**, so
/// `pre_size` alone already detects them and the content hash is never the deciding factor. Its
/// comment claims "the hash decides", but with the hash comparison deleted the workspace stayed
/// green: the claim was untested.
///
/// A person editing `fn one() { 1 }` to `fn one() { 9 }` produces the same number of bytes. That
/// is the only way the hash is load-bearing, and it is the common case — a fixed typo, a renamed
/// local of equal width, a changed numeric literal. So the size check passes, the hash comparison
/// is the only thing standing between this and a wrong answer.
///
/// What must NOT happen is `plan_corrupt`. That code says the recorded edits do not reproduce the
/// recorded post-state, i.e. "your store is damaged, this will never work" — which is both wrong
/// (the store is fine; the file moved under it) and unrecoverable in the caller's eyes: a client
/// that retries on `stale_plan` rebuilds and carries on, while a client that sees `plan_corrupt`
/// has a permanently-failing loop. The whole point of the staleness class is that it is the
/// recoverable answer, so the size-equal case has to land there.
#[test]
fn a_same_length_edit_by_a_person_is_stale_not_corrupt() {
    // A world of its own: `scenario()` is built around edits that change length, and reusing it
    // would inherit that and test nothing new.
    let w = World::new();
    w.write("a.rs", b"fn one() { 1 }\n");
    w.write("b.rs", b"fn two() { 2 }\n");
    let pre = w.reads(&["a.rs", "b.rs"]);

    // A plan that rewrites both files, growing each — so the plan itself is valid and the recorded
    // post-state is reachable. Only the *person's* edit afterwards is the same-length one.
    let id = w.plan(&[
        ("a.rs", vec![rep("fn one() { 1 }\n", "1", "11")]),
        ("b.rs", vec![rep("fn two() { 2 }\n", "2", "22")]),
    ]);

    // Same bytes out, different bytes in: `1`→`9`, `2`→`7`. Byte-for-byte different, same length.
    // This is the whole point: `pre_size` cannot see it.
    w.write("a.rs", b"fn one() { 9 }\n");
    w.write("b.rs", b"fn two() { 7 }\n");

    // Precondition, stated rather than assumed: the sizes really are equal, so a size-only check
    // would sail past this. If this assertion ever fails, the case below is testing something
    // weaker than it claims and the numbers must be revised.
    assert_eq!(
        w.read("a.rs").len() as u64,
        pre[0].len() as u64,
        "the person's edit must not change the length, or this test proves nothing"
    );
    assert_eq!(w.read("b.rs").len() as u64, pre[1].len() as u64);
    assert_ne!(
        w.read("a.rs"),
        pre[0],
        "and it must differ byte-for-byte, or this test proves nothing"
    );

    let e = apply(&w.ctx(&NoFault), &id).unwrap_err();

    assert_eq!(
        e.code,
        ErrorCode::StalePlan,
        "a same-length edit by a person is stale, not a damaged store: {}",
        e.message
    );
    assert_ne!(
        e.code,
        ErrorCode::PlanCorrupt,
        "plan_corrupt tells the caller to give up; stale_plan is the recoverable answer"
    );
    // Both files moved, so both are listed — a caller that rebuilds knows to rebuild both.
    assert!(
        e.message.contains("a.rs") && e.message.contains("b.rs"),
        "{}",
        e.message
    );
    assert!(
        !e.message.contains("fn one"),
        "never quotes content: {}",
        e.message
    );

    // Zero writes: the refusal is not cosmetic, and the person's bytes are still theirs.
    assert_eq!(
        w.read("a.rs"),
        b"fn one() { 9 }\n".to_vec(),
        "the person's edit is not overwritten"
    );
    assert!(w.journals.list().unwrap().0.is_empty(), "no journal");
    assert!(w.tmp_leftovers().is_empty());

    // And the class really is recoverable: putting the recorded bytes back applies cleanly, so a
    // client that retries on stale_plan converges instead of looping forever.
    w.write("a.rs", &pre[0]);
    w.write("b.rs", &pre[1]);
    apply(&w.ctx(&NoFault), &id).unwrap();
}

#[test]
fn the_write_policy_is_re_applied_to_every_stored_path() {
    let w = World::new();
    w.write("ok.rs", b"fn ok() { 1 }\n");
    w.write(".git/config", b"[core]\n");
    w.write("target_of_link.rs", b"fn t() { 1 }\n");
    symlink(w.root.join("target_of_link.rs"), w.root.join("link.rs")).unwrap();
    w.write("hard.rs", b"fn h() { 1 }\n");
    fs::hard_link(w.root.join("hard.rs"), w.root.join("hard2.rs")).unwrap();
    w.write("ro.rs", b"fn r() { 1 }\n");
    fs::set_permissions(w.root.join("ro.rs"), fs::Permissions::from_mode(0o444)).unwrap();
    let before = w.reads(&["ok.rs"]);
    let cases: [(&str, &[ErrorCode]); 5] = [
        (".git/config", &[ErrorCode::ProtectedPath]),
        (
            "link.rs",
            &[ErrorCode::UnsupportedTarget, ErrorCode::OutsideWorkspace],
        ),
        ("hard.rs", &[ErrorCode::UnsupportedTarget]),
        ("hard2.rs", &[ErrorCode::UnsupportedTarget]),
        ("ro.rs", &[ErrorCode::UnsupportedTarget]),
    ];
    for (bad, allowed) in cases {
        let edit = if bad == ".git/config" {
            rep("[core]\n", "core", "evil")
        } else {
            rep("fn x() { 1 }\n", "1", "2")
        };
        let id = w.plan(&[
            ("ok.rs", vec![rep("fn ok() { 1 }\n", "1", "2")]),
            (bad, vec![edit]),
        ]);
        let got = apply(&w.ctx(&NoFault), &id).unwrap_err().code;
        assert!(allowed.contains(&got), "{bad}: {got:?}");
        assert_eq!(
            w.reads(&["ok.rs"]),
            before,
            "{bad}: the good file in the same plan was not touched"
        );
        assert!(w.journals.list().unwrap().0.is_empty(), "{bad}: no journal");
        assert!(w.tmp_leftovers().is_empty());
    }
    assert_eq!(w.read(".git/config"), b"[core]\n");
    assert_eq!(w.read("target_of_link.rs"), b"fn t() { 1 }\n");
}

#[test]
fn the_syntax_gate_refuses_new_errors_and_allows_fixing_or_keeping_them() {
    let w = World::new();
    w.write("a.rs", b"fn main() {}\n");
    let id = w.plan(&[("a.rs", vec![rep("fn main() {}\n", ")", "( {")])]); // adds syntax errors
    let err = apply(&w.ctx(&NoFault), &id).unwrap_err();
    assert_eq!(err.code, ErrorCode::GateFailed);
    assert!(err.message.contains("a.rs"), "{}", err.message);
    assert_eq!(w.read("a.rs"), b"fn main() {}\n");
    assert!(w.journals.list().unwrap().0.is_empty());

    // fixing an error is allowed
    w.write("b.rs", b"fn f( {}\n");
    let id = w.plan(&[("b.rs", vec![rep("fn f( {}\n", "( ", "() ")])]); // "fn f() {}"
    apply(&w.ctx(&NoFault), &id).unwrap();
    assert_eq!(w.read("b.rs"), b"fn f() {}\n");
    // keeping the same number of errors while editing something else is allowed
    w.write("c.rs", b"fn g() { 1 }\nfn f( {}\n");
    let id = w.plan(&[("c.rs", vec![rep("fn g() { 1 }\nfn f( {}\n", "1", "2")])]);
    apply(&w.ctx(&NoFault), &id).unwrap();
    assert_eq!(w.read("c.rs"), b"fn g() { 2 }\nfn f( {}\n");
}

/// EDT-13 is `golden per language`, and the Rust cases above only ever covered
/// Rust. Rather than downgrade the row to `golden, rust`, this gives the gate a
/// real case in EVERY language the crate supports, so the claim is earned.
///
/// Each entry is a (path, pre-edit source, old, new, expected-after) tuple. A
/// source that already carries an error and is FIXED by the edit must be
/// allowed; a valid source BROKEN by the edit must be refused. So the gate is
/// checked in each language in BOTH directions, which is the whole claim:
///
/// The `World::build_plan` language mapping is what makes this possible - it used
/// to map only `.rs` and `.js`, sending everything else to `text`, and the
/// syntax gate SKIPS `text`. So before this change a `.py` file was not merely
/// untested here: it was not gated at all in this harness.
#[test]
fn the_syntax_gate_behaves_the_same_in_every_language() {
    /// (path, source, old, new, expected-after, fixes_an_error)
    const CASES: &[(&str, &str, &str, &str, &str, bool)] = &[
        // ---- an edit that FIXES a pre-existing error is allowed
        ("fix.rs", "fn f( {}\n", "( ", "() ", "fn f() {}\n", true),
        (
            "fix.js",
            "function f( {}\n",
            "( ",
            "() ",
            "function f() {}\n",
            true,
        ),
        (
            "fix.py",
            "def f(:\n    return 1\n",
            "(:\n",
            "():\n",
            "def f():\n    return 1\n",
            true,
        ),
        (
            "fix.go",
            "package p\n\nfunc f( {\n",
            "( {\n",
            "() {\n",
            "package p\n\nfunc f() {\n",
            true,
        ),
        (
            "fix.ts",
            "function f( {}\n",
            "( ",
            "() ",
            "function f() {}\n",
            true,
        ),
        (
            "fix.tsx",
            "function f( {}\n",
            "( ",
            "() ",
            "function f() {}\n",
            true,
        ),
        // ---- an edit that ADDS an error is refused
        (
            "break.rs",
            "fn main() {}\n",
            ")",
            "( {",
            "fn main() {}\n",
            false,
        ),
        (
            "break.js",
            "function main() {}\n",
            ")",
            "( {",
            "function main() {}\n",
            false,
        ),
        (
            "break.py",
            "def main():\n    return 1\n",
            "return 1\n",
            "return 1(\n",
            "def main():\n    return 1\n",
            false,
        ),
        (
            "break.go",
            "package p\n\nfunc main() {\n}\n",
            "func main",
            "func main(",
            "package p\n\nfunc main() {\n}\n",
            false,
        ),
        (
            "break.ts",
            "function main(): void {}\n",
            "): void",
            ") : void (",
            "function main(): void {}\n",
            false,
        ),
        (
            "break.tsx",
            "function main(): void {}\n",
            "): void",
            ") : void (",
            "function main(): void {}\n",
            false,
        ),
    ];

    for (path, src, old, new, expected_after, fixes) in CASES {
        let w = World::new();
        w.write(path, src.as_bytes());
        let id = w.plan(&[(path, vec![rep(src, old, new)])]);

        if *fixes {
            apply(&w.ctx(&NoFault), &id).unwrap_or_else(|e| {
                panic!("{path}: fixing an existing error must be allowed, got {e:?}")
            });
            assert_eq!(
                w.read(path),
                expected_after.as_bytes(),
                "{path}: the fix was not written",
            );
        } else {
            let err = apply(&w.ctx(&NoFault), &id)
                .expect_err(&format!("{path}: adding an error must be refused"));
            assert_eq!(
                err.code,
                ErrorCode::GateFailed,
                "{path}: expected a gate failure, got {err:?}",
            );
            assert!(
                err.message.contains(path),
                "{path}: the message must name the file, got {}",
                err.message,
            );
            // Nothing written, and no journal: a refused plan leaves no trace.
            assert_eq!(
                w.read(path),
                src.as_bytes(),
                "{path}: the file must be untouched",
            );
            assert!(
                w.journals.list().unwrap().0.is_empty(),
                "{path}: a refused apply must write no journal",
            );
        }
    }
}

/// The gate must not be reachable by naming a file whose language it does not
/// recognise. `text` files are skipped deliberately (a Markdown file has no
/// syntax), so the language mapping must not quietly widen: a `.bin` file is
/// still `text` and still skipped.
#[test]
fn the_syntax_gate_skips_a_file_whose_language_it_does_not_know() {
    let w = World::new();
    w.write("notes.bin", b"this is not code at all {{{\n");
    let id = w.plan(&[(
        "notes.bin",
        vec![rep(
            "this is not code at all {{{\n",
            "not code",
            "not ( code",
        )],
    )]);
    // Allowed: the gate skips `text`, so it never becomes an error here.
    apply(&w.ctx(&NoFault), &id).unwrap();
    assert_eq!(w.read("notes.bin"), b"this is not ( code at all {{{\n",);
}

#[test]
fn recorded_edits_that_do_not_produce_the_recorded_hash_are_refused() {
    let w = World::new();
    w.write("a.rs", b"fn one() { 1 }\n");
    let mut p = w.build_plan(&[("a.rs", vec![rep("fn one() { 1 }\n", "1", "11")])]);
    p.files[0].post_hash = ContentHash::of(b"something else entirely");
    let id = w.plans.put(&p).unwrap().0;
    assert_eq!(code(apply(&w.ctx(&NoFault), &id)), ErrorCode::PlanCorrupt);
    assert_eq!(w.read("a.rs"), b"fn one() { 1 }\n");
    assert!(w.journals.list().unwrap().0.is_empty());
}

// ---- the fault matrix --------------------------------------------------------------------

fn clean_steps() -> Vec<Step> {
    let (w, id, _, _) = scenario();
    let rec = Hook::recorder();
    apply(&w.ctx(&rec), &id).unwrap();
    rec.steps()
}

#[test]
fn a_clean_apply_announces_its_steps_in_the_documented_order() {
    let steps = clean_steps();
    let kinds: Vec<(StepKind, usize)> = steps.iter().map(|s| (s.kind, s.index)).collect();
    use StepKind::*;
    assert_eq!(
        kinds,
        vec![
            (Lock, 0),
            (Recover, 0),
            (Verify, 0),
            (Verify, 1),
            (Gates, 0),
            (JournalCreate, 0),
            (MarkWriting, 0),
            (Replace, 0),
            (Progress, 0),
            (Replace, 1),
            (Progress, 1),
            (MarkApplied, 0),
        ]
    );
}

#[test]
fn a_failure_at_any_step_rolls_everything_back_and_leaves_nothing_behind() {
    let n = clean_steps().len();
    for k in 0..n {
        let (w, id, pre, post) = scenario();
        let hook = Hook::at(k, io_fail());
        let err = apply(&w.ctx(&hook), &id).unwrap_err();
        assert_eq!(err.code, ErrorCode::IoError, "step {k}");
        assert_eq!(w.reads(&["a.rs", "b.rs"]), pre, "step {k}: fully original");
        assert!(w.tmp_leftovers().is_empty(), "step {k}");
        match w.journals.load(&id) {
            Err(e) => {
                assert_eq!(
                    e.code,
                    ErrorCode::PlanNotFound,
                    "step {k}: failed before the journal existed"
                );
                // nothing happened at all, so the plan is still good
                apply(&w.ctx(&NoFault), &id).unwrap();
                assert_eq!(w.reads(&["a.rs", "b.rs"]), post, "step {k}: re-apply works");
            }
            Ok(m) => {
                assert_eq!(m.state, JournalState::RolledBack, "step {k}");
                assert_eq!(
                    code(apply(&w.ctx(&NoFault), &id)),
                    ErrorCode::AlreadyApplied,
                    "step {k}: E-9 holds in every state"
                );
            }
        }
        assert!(
            recover(&w.ctx(&NoFault)).unwrap().is_empty(),
            "step {k}: nothing left to recover"
        );
    }
}

#[test]
fn a_crash_at_any_step_is_repaired_by_recovery_to_the_original_state() {
    let n = clean_steps().len();
    for k in 0..n {
        let (mut w, id, pre, _) = scenario();
        let hook = Hook::at(k, FaultAction::Crash);
        let err = apply(&w.ctx(&hook), &id).unwrap_err();
        assert_eq!(
            err.code,
            ErrorCode::Internal,
            "step {k}: the crash is reported as injected"
        );
        w.restart(); // the process died: new stores over the same directories
        let done = recover(&w.ctx(&NoFault)).unwrap();
        assert_eq!(
            w.reads(&["a.rs", "b.rs"]),
            pre,
            "step {k}: fully original after recovery"
        );
        assert!(w.tmp_leftovers().is_empty(), "step {k}: no temp files");
        if let Ok(m) = w.journals.load(&id) {
            assert!(m.state.is_terminal(), "step {k}: {:?}", m.state);
            assert_eq!(done.len(), 1, "step {k}");
            assert_eq!(done[0].plan_id, id);
        } else {
            assert!(done.is_empty(), "step {k}");
        }
        assert!(
            recover(&w.ctx(&NoFault)).unwrap().is_empty(),
            "step {k}: idempotent"
        );
        assert_eq!(w.reads(&["a.rs", "b.rs"]), pre);
    }
}

#[test]
fn recovery_itself_can_crash_at_any_step_and_be_run_again() {
    // crash the apply late (after both renames, before `applied`), then crash recovery at every step
    let late = clean_steps()
        .iter()
        .position(|s| s.kind == StepKind::MarkApplied)
        .unwrap();
    for apply_crash in [late, late - 2, late - 4] {
        // record the steps of a clean recovery for this crash point
        let steps = {
            let (mut w, id, _, _) = scenario();
            let _ = apply(&w.ctx(&Hook::at(apply_crash, FaultAction::Crash)), &id);
            w.restart();
            let rec = Hook::recorder();
            recover(&w.ctx(&rec)).unwrap();
            rec.steps()
        };
        assert!(
            steps.iter().any(|s| s.kind == StepKind::MarkTerminal),
            "{steps:?}"
        );
        for j in 0..steps.len() {
            let (mut w, id, pre, _) = scenario();
            let _ = apply(&w.ctx(&Hook::at(apply_crash, FaultAction::Crash)), &id);
            w.restart();
            let r = recover(&w.ctx(&Hook::at(j, FaultAction::Crash)));
            assert_eq!(
                r.unwrap_err().code,
                ErrorCode::Internal,
                "crash {apply_crash} recovery step {j}"
            );
            w.restart();
            recover(&w.ctx(&NoFault)).unwrap();
            assert_eq!(
                w.reads(&["a.rs", "b.rs"]),
                pre,
                "crash {apply_crash}, recovery crash {j}"
            );
            assert_eq!(
                w.journals.load(&id).unwrap().state,
                JournalState::RolledBack
            );
            assert!(recover(&w.ctx(&NoFault)).unwrap().is_empty());
        }
    }
}

#[test]
fn a_rollback_that_fails_keeps_the_journal_open_and_blocks_later_applies_until_recovered() {
    let (w, id, pre, _) = scenario();
    // fail the second replacement, then fail the first restore of the rollback
    let hook = Hook::new(|_, s| match (s.kind, s.index) {
        (StepKind::Replace, 1) | (StepKind::Restore, 0) => io_fail(),
        _ => FaultAction::Continue,
    });
    let err = apply(&w.ctx(&hook), &id).unwrap_err();
    assert_eq!(err.code, ErrorCode::RollbackIncomplete);
    assert!(
        err.message.contains("a.rs") && err.message.contains("b.rs"),
        "{}",
        err.message
    );
    assert_eq!(w.journals.load(&id).unwrap().state, JournalState::Writing);
    // later applies are refused with busy while a journal is unresolved ... unless recovery succeeds first.
    // Make recovery fail too, so the block is observable:
    let still_broken = Hook::new(|_, s| {
        if s.kind == StepKind::Restore {
            io_fail()
        } else {
            FaultAction::Continue
        }
    });
    w.write("c.rs", b"fn three() { 3 }\n");
    let other = w.plan(&[("c.rs", vec![rep("fn three() { 3 }\n", "3", "33")])]);
    assert_eq!(code(apply(&w.ctx(&still_broken), &other)), ErrorCode::Busy);
    assert_eq!(w.read("c.rs"), b"fn three() { 3 }\n");
    // the operator runs recovery for real
    let done = recover(&w.ctx(&NoFault)).unwrap();
    assert_eq!(done.len(), 1);
    assert_eq!(w.reads(&["a.rs", "b.rs"]), pre);
    apply(&w.ctx(&NoFault), &other).unwrap();
    assert_eq!(w.read("c.rs"), b"fn three() { 33 }\n");
}

#[test]
fn a_foreign_edit_during_a_half_applied_state_is_reported_and_nothing_is_rewritten() {
    let (mut w, id, pre, post) = scenario();
    let replace1 = clean_steps()
        .iter()
        .position(|s| s.kind == StepKind::Replace && s.index == 1)
        .unwrap();
    let _ = apply(&w.ctx(&Hook::at(replace1, FaultAction::Crash)), &id);
    w.restart();
    assert_eq!(w.read("a.rs"), post[0], "a.rs was already replaced");
    assert_eq!(w.read("b.rs"), pre[1]);
    w.write(
        "b.rs",
        b"fn two() { 2 } // a person edited this while we were dead\n",
    );
    let err = recover(&w.ctx(&NoFault)).unwrap_err();
    assert_eq!(err.code, ErrorCode::Diverged);
    assert!(
        err.message.contains("a.rs: post") && err.message.contains("b.rs: other"),
        "{}",
        err.message
    );
    assert_eq!(w.read("a.rs"), post[0], "nothing was rewritten");
    assert_eq!(w.journals.load(&id).unwrap().state, JournalState::Writing);
    // an unrelated apply is refused while the tree is unresolved
    w.write("c.rs", b"fn three() { 3 }\n");
    let other = w.plan(&[("c.rs", vec![rep("fn three() { 3 }\n", "3", "33")])]);
    assert_eq!(code(apply(&w.ctx(&NoFault), &other)), ErrorCode::Busy);
    // the person puts b.rs back; recovery now completes
    w.write("b.rs", &pre[1]);
    recover(&w.ctx(&NoFault)).unwrap();
    assert_eq!(w.reads(&["a.rs", "b.rs"]), pre);
    apply(&w.ctx(&NoFault), &other).unwrap();
}

#[test]
fn every_apply_first_recovers_what_an_earlier_crash_left() {
    let (mut w, id, pre, _) = scenario();
    let replace1 = clean_steps()
        .iter()
        .position(|s| s.kind == StepKind::Replace && s.index == 1)
        .unwrap();
    let _ = apply(&w.ctx(&Hook::at(replace1, FaultAction::Crash)), &id);
    w.restart();
    w.write("c.rs", b"fn three() { 3 }\n");
    let other = w.plan(&[("c.rs", vec![rep("fn three() { 3 }\n", "3", "33")])]);
    apply(&w.ctx(&NoFault), &other).unwrap();
    assert_eq!(
        w.reads(&["a.rs", "b.rs"]),
        pre,
        "the earlier plan was rolled back first"
    );
    assert_eq!(
        w.journals.load(&id).unwrap().state,
        JournalState::RolledBack
    );
    assert_eq!(
        w.journals.load(&other).unwrap().state,
        JournalState::Applied
    );
    assert_eq!(w.read("c.rs"), b"fn three() { 33 }\n");
}

#[test]
fn two_applies_of_one_plan_race_and_exactly_one_wins() {
    let (w, id, _, post) = scenario();
    let results: Vec<Result<_, ToolError>> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..4)
            .map(|_| s.spawn(|| apply(&w.ctx(&NoFault), &id)))
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let ok = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(
        ok,
        1,
        "{:?}",
        results
            .iter()
            .map(|r| r.as_ref().map(|_| ()).map_err(|e| e.code))
            .collect::<Vec<_>>()
    );
    for r in &results {
        if let Err(e) = r {
            assert!(
                matches!(e.code, ErrorCode::AlreadyApplied | ErrorCode::Busy),
                "{:?}",
                e.code
            );
        }
    }
    assert_eq!(w.reads(&["a.rs", "b.rs"]), post);
    assert_eq!(w.journals.load(&id).unwrap().state, JournalState::Applied);
}

#[test]
fn two_different_plans_on_one_file_serialise_and_the_second_is_stale() {
    let w = World::new();
    w.write("a.rs", b"fn one() { 1 }\n");
    let p1 = w.plan(&[("a.rs", vec![rep("fn one() { 1 }\n", "1", "11")])]);
    let p2 = w.plan(&[("a.rs", vec![rep("fn one() { 1 }\n", "1", "99")])]);
    apply(&w.ctx(&NoFault), &p1).unwrap();
    assert_eq!(code(apply(&w.ctx(&NoFault), &p2)), ErrorCode::StalePlan);
    assert_eq!(w.read("a.rs"), b"fn one() { 11 }\n");
}

// ---- CR findings: content is re-verified right before every overwrite ------------------------

/// Recovery classifies all files first and then restores them one by one. A person can edit a
/// file IN PLACE (same inode) between those two moments; `atomic_replace` only compares the file
/// identity, so the shell must re-hash the content itself right before overwriting, or it would
/// destroy that edit (E-14: never overwrite content we did not produce).
#[test]
fn recovery_re_verifies_a_file_right_before_overwriting_it() {
    let (mut w, id, _pre, post) = scenario();
    let replace1 = clean_steps()
        .iter()
        .position(|s| s.kind == StepKind::Replace && s.index == 1)
        .unwrap();
    let _ = apply(&w.ctx(&Hook::at(replace1, FaultAction::Crash)), &id);
    w.restart();
    assert_eq!(
        w.read("a.rs"),
        post[0],
        "a.rs was replaced before the crash"
    );
    let a = w.root.join("a.rs");
    let person = b"fn one() { 11 } // a person edited this in place\n".to_vec();
    let person2 = person.clone();
    let hook = Hook::new(move |_, s| {
        if s.kind == StepKind::Restore && s.index == 0 {
            fs::write(&a, &person2).unwrap(); // same inode, new content
        }
        FaultAction::Continue
    });
    let err = recover(&w.ctx(&hook)).unwrap_err();
    assert!(
        matches!(
            err.code,
            ErrorCode::Diverged | ErrorCode::RollbackIncomplete
        ),
        "{:?}",
        err.code
    );
    assert_eq!(w.read("a.rs"), person, "the person's edit survived");
    assert_eq!(
        w.journals.load(&id).unwrap().state,
        JournalState::Writing,
        "the journal stays open"
    );
}

/// The same in-place edit just before a target is replaced during apply itself.
#[test]
fn apply_re_verifies_a_file_right_before_replacing_it() {
    let (w, id, pre, _) = scenario();
    let a = w.root.join("a.rs");
    let person = b"fn one() { 1 } // edited in place after verification\n".to_vec();
    let person2 = person.clone();
    let hook = Hook::new(move |_, s| {
        if s.kind == StepKind::Replace && s.index == 0 {
            fs::write(&a, &person2).unwrap();
        }
        FaultAction::Continue
    });
    assert!(apply(&w.ctx(&hook), &id).is_err());
    assert_eq!(w.read("a.rs"), person, "the person's edit survived");
    assert_eq!(w.read("b.rs"), pre[1], "nothing else was touched");
    assert!(w.tmp_leftovers().is_empty());
}

/// `already_applied` must give the caller an action, not restate the rule.
///
/// The refusal's old next step cited an internal reference and repeated the invariant — accurate,
/// and useless: a caller reading it knows the plan is spent and has no move. There are exactly
/// two moves and both are nameable from the tool set: `ast_undo` with the same id, or a fresh
/// preview for a new plan. The next step has to say so.
///
/// Mutation self-proof: restore the citation-only next step in `already_applied()` — red.
#[test]
fn already_applied_names_the_two_things_a_caller_can_do() {
    let (w, id, _pre, post) = scenario();
    apply(&w.ctx(&NoFault), &id).unwrap();
    let err = apply(&w.ctx(&NoFault), &id).expect_err("a second apply must be refused");
    assert_eq!(err.code, ErrorCode::AlreadyApplied);
    // Nothing was written by the refusal itself.
    assert_eq!(w.reads(&["a.rs", "b.rs"]), post);

    // An action, and it must be one the caller can actually take from the tool set.
    assert!(
        err.next.contains("ast_undo"),
        "the next step must name the way back: {}",
        err.next
    );
    assert!(
        err.next.contains("ast_edit_preview"),
        "the next step must name the way forward: {}",
        err.next
    );
    // And no internal reference: it means nothing to the caller and reads as a dead end.
    assert!(
        !err.next.contains("(E-"),
        "an internal citation is not an action: {}",
        err.next
    );
}
